//! Bounded, in-process execution of the official Web player's URL transforms.
//!
//! Player source is untrusted. No network, filesystem, process or host callbacks
//! are exposed to JavaScript, and no credentials are passed to this module.

use crate::{Error, Result};
use rquickjs::{CatchResultExt, CaughtError, Context, Runtime};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

const LIBRARY: &str = include_str!("../vendor/yt-dlp-ejs/lib.min.js");
const SOLVER: &str = include_str!("../vendor/yt-dlp-ejs/core.min.js");
// QuickJS uses flat strings: Astring's default repeated `output += fragment`
// becomes quadratic for a multi-megabyte player. Its documented stream output
// lets us collect fragments and join once, without patching the pinned bundle.
const AST_BINDINGS: &str = r#"
    var meriyah = lib.meriyah;
    var astring = { generate: function(node) {
        var chunks = [];
        lib.astring.generate(node, { output: { write: function(chunk) { chunks.push(chunk); } } });
        return chunks.join('');
    } };
"#;
const MAX_PLAYER_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHALLENGES: usize = 256;
const MAX_CHALLENGE_BYTES: usize = 8192;
const MEMORY_BYTES: usize = 256 * 1024 * 1024;
// Native interpreter frame sizes vary by compiler (especially MSVC). Reserve
// enough room for real player AST traversal while retaining a strict ceiling.
const STACK_BYTES: usize = 2 * 1024 * 1024;
const TIME_BUDGET: Duration = Duration::from_secs(30);
const MAX_PREPARED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PREPARED_PLAYERS: usize = 2;

struct PreparedPlayer {
    hash: [u8; 32],
    source_bytes: usize,
    script: Arc<String>,
}

static PREPARED_PLAYERS: OnceLock<Mutex<VecDeque<PreparedPlayer>>> = OnceLock::new();

pub(crate) struct SolvedChallenges {
    pub signatures: HashMap<String, String>,
    pub n_values: HashMap<String, String>,
}

/// Solve every challenge together so a player needs to be parsed only once.
/// Error strings intentionally exclude JavaScript errors and challenge values.
pub(crate) fn solve(
    player: &str,
    signatures: &[String],
    n_values: &[String],
) -> Result<SolvedChallenges> {
    crate::operation::check()?;
    if player.len() > MAX_PLAYER_BYTES
        || signatures.len().saturating_add(n_values.len()) > MAX_CHALLENGES
        || signatures
            .iter()
            .chain(n_values)
            .any(|s| s.is_empty() || s.len() > MAX_CHALLENGE_BYTES)
    {
        return Err(failure("player or challenges exceed solver input limits"));
    }
    if signatures.is_empty() && n_values.is_empty() {
        return Ok(SolvedChallenges {
            signatures: HashMap::new(),
            n_values: HashMap::new(),
        });
    }
    let prepared = prepare(player)?;
    crate::operation::phase("solving_challenges");
    let input = json!({
        "type": "preprocessed", "preprocessed_player": prepared.as_str(),
        "requests": [
            {"type": "sig", "challenges": signatures},
            {"type": "n", "challenges": n_values}
        ]
    });
    let output = evaluate(&input.to_string(), TIME_BUDGET)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|_| failure("invalid player solver response"))?;
    let responses = value
        .get("responses")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 2)
        .ok_or_else(|| failure("player transforms could not be extracted"))?;
    Ok(SolvedChallenges {
        signatures: collect(&responses[0], signatures)?,
        n_values: collect(&responses[1], n_values)?,
    })
}

/// Only player source enters this stage. Challenge values, signed URLs and
/// account credentials cannot be retained in the process-wide bounded cache.
pub(crate) fn prepare(player: &str) -> Result<Arc<String>> {
    crate::operation::check()?;
    if player.len() > MAX_PLAYER_BYTES {
        return Err(failure("player exceeds solver input limits"));
    }
    crate::operation::phase("preparing_player");
    let hash: [u8; 32] = Sha256::digest(player.as_bytes()).into();
    let cache_mutex = PREPARED_PLAYERS.get_or_init(|| Mutex::new(VecDeque::new()));
    let mut cache = loop {
        crate::operation::check()?;
        match cache_mutex.try_lock() {
            Ok(cache) => break cache,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(TryLockError::Poisoned(_)) => {
                return Err(failure("player script cache is unavailable"))
            }
        }
    };
    if let Some(index) = cache
        .iter()
        .position(|p| p.hash == hash && p.source_bytes == player.len())
    {
        let entry = cache.remove(index).expect("cache position was checked");
        let script = Arc::clone(&entry.script);
        cache.push_back(entry);
        return Ok(script);
    }
    // Serializing cold preparation avoids multiplying the 256 MiB AST budget
    // for concurrent callers. The cache lock is released before any challenge
    // is solved; preparation has the same enforced deadline as execution.
    let input = json!({
        "type": "player", "player": player,
        "output_preprocessed": true, "requests": []
    });
    let output = evaluate(&input.to_string(), TIME_BUDGET)?;
    let mut value: Value = serde_json::from_str(&output)
        .map_err(|_| failure("invalid player preparation response"))?;
    if value.get("type").and_then(Value::as_str) != Some("result") {
        return Err(failure("player transforms could not be prepared"));
    }
    let script = match value.get_mut("preprocessed_player").map(Value::take) {
        Some(Value::String(script)) if !script.is_empty() && script.len() <= MAX_PREPARED_BYTES => {
            Arc::new(script)
        }
        _ => return Err(failure("invalid prepared player script")),
    };
    if cache.len() >= MAX_PREPARED_PLAYERS {
        cache.pop_front();
    }
    cache.push_back(PreparedPlayer {
        hash,
        source_bytes: player.len(),
        script: Arc::clone(&script),
    });
    Ok(script)
}

fn evaluate(input: &str, budget: Duration) -> Result<String> {
    evaluate_with_memory_limit(input, budget, MEMORY_BYTES)
}

fn evaluate_with_memory_limit(
    input: &str,
    budget: Duration,
    memory_bytes: usize,
) -> Result<String> {
    // Callers may use small native stacks (Windows, Tokio, C ABI hosts). Keep
    // QuickJS's recursion budget well below an explicitly reserved worker stack.
    crate::operation::check()?;
    let operation = crate::operation::current();
    let input = input.to_owned();
    std::thread::Builder::new()
        .name("ytmusic-player".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || match operation {
            Some(operation) => crate::operation::with_context(&operation, || {
                evaluate_inner(&input, budget, memory_bytes)
            }),
            None => evaluate_inner(&input, budget, memory_bytes),
        })
        .map_err(|_| failure("cannot initialize player solver worker"))?
        .join()
        .map_err(|_| failure("player solver worker failed"))?
}

fn evaluate_inner(input: &str, budget: Duration, memory_bytes: usize) -> Result<String> {
    crate::operation::check()?;
    let rt = Runtime::new().map_err(|_| operation_failure("cannot initialize player solver"))?;
    rt.set_memory_limit(memory_bytes);
    rt.set_gc_threshold(64 * 1024 * 1024);
    rt.set_max_stack_size(STACK_BYTES);
    let start = Instant::now();
    let operation = crate::operation::current();
    rt.set_interrupt_handler(Some(Box::new(move || {
        start.elapsed() >= budget || operation.as_ref().is_some_and(|op| op.check().is_err())
    })));
    let ctx = Context::full(&rt).map_err(|_| {
        if start.elapsed() >= budget {
            Error::Timeout
        } else {
            operation_failure("cannot initialize player solver context")
        }
    })?;
    ctx.with(|ctx| {
        crate::operation::check()?;
        // A data binding keeps player text and challenges out of generated code.
        ctx.globals()
            .set("__solver_input", input)
            .map_err(|_| operation_failure("cannot initialize player solver input"))?;
        ctx.eval::<(), _>(LIBRARY)
            .and_then(|_| ctx.eval::<(), _>(AST_BINDINGS))
            .and_then(|_| ctx.eval::<(), _>(SOLVER))
            .and_then(|_| ctx.eval::<String, _>("JSON.stringify(jsc(JSON.parse(__solver_input)))"))
            .catch(&ctx)
            .map_err(|error| {
                if let Err(error) = crate::operation::check() {
                    return error;
                }
                if start.elapsed() >= budget {
                    return Error::Timeout;
                }
                // Only fixed categories may leave the sandbox. Raw exception
                // messages/stacks can contain player input or signed URLs.
                let message = match error {
                    CaughtError::Exception(exception) => exception.message().unwrap_or_default(),
                    _ => String::new(),
                };
                failure(
                    if message.contains("stack overflow")
                        || message.contains("Maximum call stack size exceeded")
                    {
                        "player solver exceeded its stack budget"
                    } else {
                        "player solver failed or exceeded its resource budget"
                    },
                )
            })
    })
}

fn collect(response: &Value, challenges: &[String]) -> Result<HashMap<String, String>> {
    if challenges.is_empty() {
        return Ok(HashMap::new());
    }
    let data = response
        .get("data")
        .and_then(Value::as_object)
        .filter(|_| response.get("type").and_then(Value::as_str) == Some("result"))
        .ok_or_else(|| failure("player transform is unavailable"))?;
    challenges
        .iter()
        .map(|challenge| {
            let result = data
                .get(challenge)
                .and_then(Value::as_str)
                .filter(|s| {
                    !s.is_empty()
                        && s.len() <= MAX_CHALLENGE_BYTES
                        && !s.starts_with("enhanced_except_")
                        && !s.ends_with(&format!("_w8_{challenge}"))
                })
                .ok_or_else(|| failure("player returned an invalid transform"))?;
            Ok((challenge.clone(), result.to_owned()))
        })
        .collect()
}

fn operation_failure(message: &str) -> Error {
    crate::operation::check()
        .err()
        .unwrap_or_else(|| failure(message))
}

fn failure(message: &str) -> Error {
    Error::StreamUnavailable(message.into())
}

/// Extract the timestamp advertised by the same player used for deciphering.
pub(crate) fn signature_timestamp(player: &str) -> Option<u32> {
    player
        .match_indices("signatureTimestamp")
        .find_map(|(offset, _)| {
            let remaining = player[offset + "signatureTimestamp".len()..]
                .trim_start_matches(['\'', '"'])
                .trim_start();
            let remaining = remaining.strip_prefix(':')?.trim_start();
            let digits: String = remaining.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok().filter(|n| *n > 0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A small authored player fixture exercises AST discovery, code generation,
    // execution and both challenge types without vendoring Google's player.
    const FIXTURE: &str = r#"(function(){
        function PlayerUrl(){ this.values = {}; }
        PlayerUrl.prototype.set = function(k,v){ this.values[k] = v; };
        PlayerUrl.prototype.get = function(k){ return this.values[k]; };
        PlayerUrl.prototype.transform = function(){
            if(this.values.n) this.values.n = this.values.n.split('').reverse().join('');
        };
        function makeUrl(url, key, signature){
            var u = new PlayerUrl();
            u.set('alr', 'yes');
            if(signature) u.set(key, encodeURIComponent(decodeURIComponent(signature).slice(1)));
            return u;
        }
    }).call(this);"#;

    #[test]
    fn embedded_ast_solver_solves_both_transforms() {
        let solved = solve(FIXTURE, &["abc%?".into()], &["xyz123".into()]).unwrap();
        assert_eq!(solved.signatures["abc%?"], "bc%?");
        assert_eq!(solved.n_values["xyz123"], "321zyx");
    }

    #[test]
    fn nested_player_expressions_fit_the_bounded_stack() {
        let expression = format!(
            "{}decodeURIComponent(signature).slice(1){}",
            "String(".repeat(64),
            ")".repeat(64)
        );
        let player = FIXTURE.replace("decodeURIComponent(signature).slice(1)", &expression);
        let solved = solve(&player, &["abcdef".into()], &["xyz123".into()]).unwrap();
        assert_eq!(solved.signatures["abcdef"], "bcdef");
        assert_eq!(solved.n_values["xyz123"], "321zyx");
    }

    #[test]
    fn prepared_cache_reuses_only_player_source() {
        let prepared = prepare(FIXTURE).unwrap();
        let again = prepare(FIXTURE).unwrap();
        assert!(Arc::ptr_eq(&prepared, &again));
        let signature = "private-signature-for-cache-test".to_owned();
        let n_value = "private-n-value-for-cache-test".to_owned();
        let solved = solve(
            FIXTURE,
            std::slice::from_ref(&signature),
            std::slice::from_ref(&n_value),
        )
        .unwrap();
        assert_eq!(solved.signatures[&signature], signature[1..]);
        assert_eq!(
            solved.n_values[&n_value],
            n_value.chars().rev().collect::<String>()
        );
        assert!(!prepared.contains(&signature));
        assert!(!prepared.contains(&n_value));
    }

    #[test]
    fn interrupts_player_infinite_loop_and_redacts_errors() {
        let input =
            json!({"type":"preprocessed", "preprocessed_player":"while(true) {}", "requests":[]});
        let started = Instant::now();
        assert!(matches!(
            evaluate(&input.to_string(), Duration::from_millis(30)),
            Err(Error::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        let error = solve(
            "throw 'private_challenge'",
            &["private_challenge".into()],
            &[],
        )
        .err()
        .unwrap()
        .to_string();
        assert!(!error.contains("private_challenge"));
    }

    #[test]
    fn operation_interrupts_active_javascript_and_worker_inherits_deadline() {
        use crate::operation::{OperationContext, OperationOptions};
        for cancel in [false, true] {
            let context = OperationContext::new(OperationOptions {
                timeout_ms: if cancel { 5000 } else { 150 },
            })
            .unwrap();
            let controller = context.clone();
            let input = json!({"type":"preprocessed", "preprocessed_player":"while(true) {}", "requests":[]}).to_string();
            let worker = std::thread::spawn(move || context.run(|| evaluate(&input, TIME_BUDGET)));
            if cancel {
                std::thread::sleep(Duration::from_millis(40));
                controller.cancel();
            }
            let result = worker.join().unwrap();
            if cancel {
                assert!(matches!(result, Err(Error::Cancelled)));
            } else {
                assert!(matches!(result, Err(Error::Timeout)));
            }
        }
    }
    #[test]
    fn solver_has_no_host_io_and_runtime_is_isolated() {
        let input = json!({"type":"preprocessed", "preprocessed_player":
            "_result.sig = () => [typeof fetch, typeof require, typeof process, typeof std, typeof os].join(',')",
            "requests":[{"type":"sig","challenges":["input"]}]});
        let output: Value =
            serde_json::from_str(&evaluate(&input.to_string(), TIME_BUDGET).unwrap()).unwrap();
        assert_eq!(
            output["responses"][0]["data"]["input"],
            "undefined,undefined,undefined,undefined,undefined"
        );
    }

    #[test]
    fn memory_and_recursion_exhaustion_return_errors() {
        let input = json!({"type":"preprocessed", "preprocessed_player":
            "var tooLarge = new Uint8Array(32 * 1024 * 1024)", "requests":[]});
        assert!(
            evaluate_with_memory_limit(&input.to_string(), TIME_BUDGET, 16 * 1024 * 1024).is_err()
        );
        let input = json!({"type":"preprocessed", "preprocessed_player":
            "function recurse(){return recurse()} recurse()", "requests":[]});
        let error = evaluate(&input.to_string(), TIME_BUDGET).unwrap_err();
        assert!(error.to_string().contains("stack budget"));
    }

    #[test]
    fn validates_inputs_and_timestamp() {
        assert!(solve(FIXTURE, &["a".repeat(MAX_CHALLENGE_BYTES + 1)], &[]).is_err());
        assert_eq!(
            signature_timestamp("{signatureTimestamp:20710}"),
            Some(20710)
        );
        assert_eq!(
            signature_timestamp("{'signatureTimestamp': 12345}"),
            Some(12345)
        );
        assert_eq!(signature_timestamp("{signatureTimestamp:unknown}"), None);
    }

    /// Opt-in regression check against locally captured official player files.
    /// The files may contain signed URLs and must never be added to the repo.
    #[test]
    #[ignore = "requires YTMUSIC_TEST_PLAYER_JS and YTMUSIC_TEST_PLAYER_RESPONSE files"]
    fn captured_official_player() {
        let player =
            std::fs::read_to_string(std::env::var("YTMUSIC_TEST_PLAYER_JS").unwrap()).unwrap();
        let response: Value = serde_json::from_slice(
            &std::fs::read(std::env::var("YTMUSIC_TEST_PLAYER_RESPONSE").unwrap()).unwrap(),
        )
        .unwrap();
        let formats = response["streamingData"]["adaptiveFormats"]
            .as_array()
            .unwrap();
        let mut signatures = Vec::new();
        let mut n_values = Vec::new();
        for format in formats {
            let cipher = format["signatureCipher"]
                .as_str()
                .or(format["cipher"].as_str());
            let mut stream_url = format["url"].as_str().map(str::to_owned);
            if let Some(cipher) = cipher {
                let fields = reqwest::Url::parse(&format!("https://localhost/?{cipher}")).unwrap();
                for (key, value) in fields.query_pairs() {
                    match key.as_ref() {
                        "s" => signatures.push(value.into_owned()),
                        "url" => stream_url = Some(value.into_owned()),
                        _ => {}
                    }
                }
            }
            if let Some(url) = stream_url {
                let url = reqwest::Url::parse(&url).unwrap();
                n_values.extend(
                    url.query_pairs()
                        .filter(|(key, _)| key == "n")
                        .map(|(_, value)| value.into_owned()),
                );
            }
        }
        assert!(
            !signatures.is_empty() || !n_values.is_empty(),
            "capture has no challenges"
        );
        let started = Instant::now();
        let solved = solve(&player, &signatures, &n_values).unwrap();
        println!(
            "official player: signatures={}, n={}, elapsed_ms={}",
            solved.signatures.len(),
            solved.n_values.len(),
            started.elapsed().as_millis()
        );
        let started = Instant::now();
        let warmed = solve(&player, &signatures, &n_values).unwrap();
        assert!(warmed.signatures == solved.signatures && warmed.n_values == solved.n_values);
        println!(
            "cached player: elapsed_ms={}",
            started.elapsed().as_millis()
        );
        assert!(signature_timestamp(&player).is_some());
    }
}
