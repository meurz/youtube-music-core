//! CLI bridge to an explicitly enabled local Chromium debugging port.
use reqwest::{blocking::Client, Url};
use serde_json::{json, Value};
use std::{
    net::TcpStream,
    thread,
    time::{Duration, Instant},
};
use tungstenite::{protocol::WebSocketConfig, Message, WebSocket};
use youtube_music_core::{auth::BrowserSession, Error, Result};

const MUSIC: &str = "https://music.youtube.com";
const SNAPSHOT: &str = "({origin:location.origin,logged_in:globalThis.ytcfg?.get('LOGGED_IN')===true,auth_user:globalThis.ytcfg?.get('SESSION_INDEX')??0,delegated_session_id:globalThis.ytcfg?.get('DELEGATED_SESSION_ID')??null})";
fn failure(message: &str) -> Error {
    if let Some(operation) = youtube_music_core::operation::current() {
        if let Err(error) = operation.check() {
            return error;
        }
    }
    Error::InvalidInput(message.into())
}

fn checked_json(response: reqwest::blocking::Response) -> Result<Value> {
    use std::io::Read;
    if !response.status().is_success() {
        return Err(failure("local browser endpoint rejected the request"));
    }
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read local browser response"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(failure("local browser response exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|_| failure("local browser returned invalid JSON"))
}

fn websocket_url(raw: &str, port: u16) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| failure("invalid browser WebSocket endpoint"))?;
    if url.scheme() != "ws"
        || !matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
        || url.port() != Some(port)
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().starts_with("/devtools/")
    {
        return Err(failure(
            "browser WebSocket must remain on the selected local debugging port",
        ));
    }
    Ok(url)
}

fn operation_check() -> Result<()> {
    youtube_music_core::operation::current().map_or(Ok(()), |operation| operation.check())
}
fn budget(maximum: Duration) -> Result<Duration> {
    operation_check()?;
    Ok(youtube_music_core::operation::current()
        .map_or(maximum, |operation| maximum.min(operation.remaining())))
}
fn read_message(socket: &mut WebSocket<TcpStream>, deadline: Instant) -> Result<Message> {
    loop {
        operation_check()?;
        if Instant::now() >= deadline {
            return Err(Error::Timeout);
        }
        socket
            .get_mut()
            .set_read_timeout(Some(
                budget(Duration::from_millis(100))?
                    .min(deadline.saturating_duration_since(Instant::now()))
                    .max(Duration::from_millis(1)),
            ))
            .map_err(|_| failure("cannot set local browser deadline"))?;
        match socket.read() {
            Ok(message) => return Ok(message),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => return Err(failure("local browser connection closed")),
        }
    }
}

fn call(socket: &mut WebSocket<TcpStream>, id: u64, method: &str, params: Value) -> Result<Value> {
    operation_check()?;
    let deadline = Instant::now()
        + budget(Duration::from_secs(if params["awaitPromise"] == true {
            30
        } else {
            10
        }))?;
    socket
        .get_mut()
        .set_write_timeout(Some(budget(Duration::from_secs(10))?))
        .map_err(|_| failure("cannot set local browser deadline"))?;
    socket
        .send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .map_err(|_| failure("cannot send a request to the local browser"))?;
    for _ in 0..1024 {
        let message = read_message(socket, deadline)?;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)
                .map_err(|_| failure("invalid browser protocol response"))?;
            if value["id"].as_u64() != Some(id) {
                continue;
            }
            if value.get("error").is_some() {
                return Err(failure("browser protocol command failed"));
            }
            operation_check()?;
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| failure("browser protocol result is missing"));
        }
    }
    Err(failure("too many unrelated browser protocol events"))
}

fn session_from_snapshot(snapshot: &Value, headers: &Value) -> Result<BrowserSession> {
    if snapshot["origin"] != MUSIC || snapshot["logged_in"] != true {
        return Err(Error::AuthenticationRequired);
    }
    let auth_user = snapshot["auth_user"]
        .as_u64()
        .or_else(|| snapshot["auth_user"].as_str()?.parse().ok())
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| failure("browser account index is invalid"))?;
    let cookie = headers
        .as_object()
        .and_then(|headers| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
                .and_then(|(_, value)| value.as_str())
        })
        .ok_or_else(|| failure("browser request has no Cookie header"))?;
    let session = BrowserSession {
        cookie: cookie.into(),
        cookie_expirations: Default::default(),
        auth_user,
        delegated_session_id: snapshot["delegated_session_id"].as_str().map(str::to_owned),
    };
    session.validate()?;
    Ok(session)
}

// ExtraInfo can precede requestWillBeSent. Match by request ID and the exact
// unique probe URL before accepting headers; never combine cookie-store entries.
#[derive(Default)]
struct RequestHeaders {
    request_id: Option<String>,
    pending: std::collections::BTreeMap<String, Value>,
}
impl RequestHeaders {
    fn observe(&mut self, event: &Value, url: &str) -> Result<Option<Value>> {
        let params = &event["params"];
        if event["method"] == "Network.requestWillBeSent" && params["request"]["url"] == url {
            self.request_id = params["requestId"].as_str().map(str::to_owned);
        }
        if event["method"] == "Network.requestWillBeSentExtraInfo" {
            if let Some(id) = params["requestId"].as_str() {
                self.pending.insert(id.into(), params["headers"].clone());
            }
        }
        if let Some(headers) = self
            .request_id
            .as_ref()
            .and_then(|id| self.pending.remove(id))
        {
            return Ok(Some(headers));
        }
        if self.pending.len() > 128 {
            return Err(failure("too many unrelated browser requests"));
        }
        Ok(None)
    }
}

fn capture_request(socket: &mut WebSocket<TcpStream>, id: u64) -> Result<Value> {
    call(socket, id, "Network.enable", json!({}))?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| failure("secure randomness unavailable"))?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let url = format!("{MUSIC}/generate_204?ytmusic_session_probe={nonce}");
    let expression = format!(
        "if(location.origin==={}){{void fetch({},{{credentials:'same-origin',cache:'no-store',redirect:'error'}}).catch(()=>{{}})}}",
        json!(MUSIC), json!(url)
    );
    socket
        .send(Message::Text(
            json!({"id":id+1,"method":"Runtime.evaluate",
        "params":{"expression":expression}})
            .to_string()
            .into(),
        ))
        .map_err(|_| failure("cannot start a Music session probe"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut headers = RequestHeaders::default();
    for _ in 0..1024 {
        if Instant::now() >= deadline {
            break;
        }
        if let Message::Text(text) = read_message(socket, deadline)? {
            let event: Value = serde_json::from_str(&text)
                .map_err(|_| failure("invalid browser protocol response"))?;
            if event["id"].as_u64() == Some(id + 1)
                && (event.get("error").is_some()
                    || event["result"].get("exceptionDetails").is_some())
            {
                return Err(failure("browser could not start the Music session probe"));
            }
            if let Some(headers) = headers.observe(&event, &url)? {
                return Ok(headers);
            }
        }
    }
    Err(failure(
        "browser did not expose headers for the Music session probe",
    ))
}

fn connect_music(port: u16) -> Result<WebSocket<TcpStream>> {
    if port == 0 {
        return Err(failure("browser port must be nonzero"));
    }
    let http = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(budget(Duration::from_secs(10))?)
        .build()
        .map_err(|_| failure("cannot create local browser client"))?;
    let endpoint = format!("http://127.0.0.1:{port}");
    let tabs = checked_json(http.get(format!("{endpoint}/json/list")).send()
        .map_err(|_| failure("cannot connect to Chrome/Edge; enable a local debugging port or use auth import --headers-file"))?)?;
    let target = tabs
        .as_array()
        .and_then(|tabs| {
            tabs.iter().find(|tab| {
                tab["type"] == "page"
                    && tab["url"]
                        .as_str()
                        .and_then(|v| Url::parse(v).ok())
                        .is_some_and(|url| url.origin().ascii_serialization() == MUSIC)
            })
        })
        .cloned();
    let target = match target {
        Some(target) => target,
        None => checked_json(
            http.put(format!("{endpoint}/json/new?{MUSIC}/"))
                .send()
                .map_err(|_| failure("cannot open a Music tab in the local browser"))?,
        )?,
    };
    let ws_url = websocket_url(
        target["webSocketDebuggerUrl"]
            .as_str()
            .ok_or_else(|| failure("browser target has no debugging endpoint"))?,
        port,
    )?;
    let stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}")
            .parse()
            .map_err(|_| failure("invalid browser address"))?,
        budget(Duration::from_secs(10))?,
    )
    .map_err(|_| failure("cannot connect to browser WebSocket"))?;
    stream
        .set_read_timeout(Some(budget(Duration::from_secs(10))?))
        .map_err(|_| failure("cannot set browser timeout"))?;
    stream
        .set_write_timeout(Some(budget(Duration::from_secs(10))?))
        .map_err(|_| failure("cannot set browser timeout"))?;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(1024 * 1024);
    config.max_frame_size = Some(1024 * 1024);
    let (socket, _) =
        tungstenite::client::client_with_config(ws_url.as_str(), stream, Some(config))
            .map_err(|_| failure("browser WebSocket handshake failed"))?;
    Ok(socket)
}

/// Existing Music tabs are reused; otherwise one is opened. The user handles
/// password/MFA/account selection in the browser. Only the exact same-origin Music probe request is imported.
pub fn capture(port: u16, wait_seconds: u64) -> Result<BrowserSession> {
    if port == 0 || !(1..=600).contains(&wait_seconds) {
        return Err(failure(
            "browser port must be nonzero and wait_seconds must be 1..600",
        ));
    }
    let mut socket = connect_music(port)?;
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    let mut id = 1;
    loop {
        let snapshot = call(
            &mut socket,
            id,
            "Runtime.evaluate",
            json!({"expression":SNAPSHOT,"returnByValue":true}),
        );
        id += 1;
        if let Ok(snapshot) = snapshot {
            let snapshot = &snapshot["result"]["value"];
            if snapshot["origin"] == MUSIC && snapshot["logged_in"] == true {
                let headers = capture_request(&mut socket, id)?;
                let current = call(
                    &mut socket,
                    id + 2,
                    "Runtime.evaluate",
                    json!({"expression":SNAPSHOT,"returnByValue":true}),
                )?;
                if current["result"]["value"] != *snapshot {
                    return Err(failure("browser account changed during import; retry"));
                }
                let result = session_from_snapshot(snapshot, &headers);
                let _ = socket.close(None);
                return result;
            }
        }
        if Instant::now() >= deadline {
            return Err(failure("Music is not signed in; finish browser sign-in/account selection and retry auth login --browser-port"));
        }
        thread::sleep(budget(Duration::from_millis(100))?);
    }
}

/// Mint scoped tokens in Google's own browser runtime. Raw token values must
/// only enter a protected profile/configuration, never normal CLI diagnostics.
pub fn capture_po_token(
    port: u16,
    video_id: &str,
) -> Result<youtube_music_core::attestation::PoTokenBundle> {
    use youtube_music_core::attestation::{browser_script, session_binding, PoTokenBundle};
    let expression = browser_script(video_id)?;
    let mut socket = connect_music(port)?;
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|_| failure("cannot set browser attestation timeout"))?;
    let snapshot = call(
        &mut socket,
        1,
        "Runtime.evaluate",
        json!({"expression":SNAPSHOT,"returnByValue":true}),
    )?["result"]["value"]
        .clone();
    let before = session_from_snapshot(&snapshot, &capture_request(&mut socket, 2)?)?;
    let binding = session_binding(
        &before.cookie,
        before.auth_user,
        before.delegated_session_id.as_deref(),
    )?;
    let value = call(
        &mut socket,
        4,
        "Runtime.evaluate",
        json!({
            "expression": expression, "returnByValue": true, "awaitPromise": true,
            "timeout": 28000
        }),
    )?;
    if value.get("exceptionDetails").is_some() {
        return Err(failure("official browser attestation failed"));
    }
    let mut value = value["result"]["value"].clone();
    if value.get("error").is_some() {
        return Err(failure("official browser could not mint PO tokens; keep a signed-in Music player tab open and retry"));
    }
    let current = call(
        &mut socket,
        5,
        "Runtime.evaluate",
        json!({"expression":SNAPSHOT,"returnByValue":true}),
    )?["result"]["value"]
        .clone();
    let after = session_from_snapshot(&current, &capture_request(&mut socket, 6)?)?;
    if current != snapshot
        || binding
            != session_binding(
                &after.cookie,
                after.auth_user,
                after.delegated_session_id.as_deref(),
            )?
    {
        return Err(failure("browser account changed during attestation; retry"));
    }
    value["session_binding"] = binding.into();
    let mut bundle: PoTokenBundle = serde_json::from_value(value)
        .map_err(|_| failure("official browser returned invalid PO token metadata"))?;
    if bundle.video_id != video_id {
        return Err(failure("official browser PO token video does not match"));
    }
    bundle.validate()?;
    // WSL and the Windows browser can differ by a second. Never extend the
    // reported expiry, but enforce the retention ceiling using the core clock.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| failure("system clock before Unix epoch"))?
        .as_secs();
    bundle.expires_at = bundle.expires_at.min(now.saturating_add(3600));
    let _ = socket.close(None);
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_wait_obeys_operation_deadline_and_cancellation() {
        use youtube_music_core::operation::{OperationContext, OperationOptions};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let mut socket =
            WebSocket::from_raw_socket(stream, tungstenite::protocol::Role::Client, None);
        let operation = OperationContext::new(OperationOptions { timeout_ms: 25 }).unwrap();
        let start = Instant::now();
        let result = operation.run(|| {
            call(
                &mut socket,
                1,
                "Runtime.evaluate",
                json!({"awaitPromise":true}),
            )
        });
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(start.elapsed() < Duration::from_millis(500));
        let operation = OperationContext::new(OperationOptions::default()).unwrap();
        operation.cancel();
        assert!(matches!(
            operation.run(|| call(&mut socket, 2, "Runtime.evaluate", json!({}))),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn capture_is_music_scoped_and_retains_account_selection() {
        let session = session_from_snapshot(&json!({"origin":MUSIC,"logged_in":true,"auth_user":"2","delegated_session_id":"123"}),
            &json!({"Cookie":"SAPISID=synthetic; VISITOR_PRIVACY_METADATA=one; VISITOR_PRIVACY_METADATA=two", "Authorization":"discarded"})).unwrap();
        assert_eq!(
            session.cookie,
            "SAPISID=synthetic; VISITOR_PRIVACY_METADATA=one; VISITOR_PRIVACY_METADATA=two"
        );
        assert_eq!(session.auth_user, 2);
        assert_eq!(session.delegated_session_id.as_deref(), Some("123"));
        assert!(session_from_snapshot(
            &json!({"origin":"https://evil.test","logged_in":true,"auth_user":0}),
            &json!({"cookies":[]})
        )
        .is_err());
    }
    #[test]
    fn only_exact_probe_headers_are_accepted_in_either_event_order() {
        let url = "https://music.youtube.com/generate_204?ytmusic_session_probe=test";
        let request = json!({"method":"Network.requestWillBeSent","params":{"requestId":"probe","request":{"url":url}}});
        let extra = json!({"method":"Network.requestWillBeSentExtraInfo","params":{"requestId":"probe","headers":{"Cookie":"SAPISID=synthetic"}}});
        for events in [[&request, &extra], [&extra, &request]] {
            let mut capture = RequestHeaders::default();
            assert!(capture.observe(&json!({"method":"Network.requestWillBeSentExtraInfo","params":{"requestId":"unrelated","headers":{"Cookie":"private"}}}), url).unwrap().is_none());
            assert!(capture.observe(events[0], url).unwrap().is_none());
            assert_eq!(
                capture.observe(events[1], url).unwrap().unwrap()["Cookie"],
                "SAPISID=synthetic"
            );
        }
    }
    #[test]
    fn debugger_cannot_redirect_to_a_remote_endpoint() {
        assert!(websocket_url("ws://localhost:9222/devtools/page/123", 9222).is_ok());
        for url in [
            "ws://evil.test:9222/devtools/page/123",
            "ws://127.0.0.1:9000/devtools/page/123",
            "wss://localhost:9222/devtools/page/123",
            "ws://user:pass@localhost:9222/devtools/page/123",
        ] {
            assert!(websocket_url(url, 9222).is_err());
        }
    }
}
