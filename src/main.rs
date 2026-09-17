use clap::{Parser, Subcommand};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
};
use youtube_music_core::{
    model::SearchFilter, Config, ContinuationEndpoint, Error, MusicClient, Request,
};
mod browser;
mod credentials;
use credentials::{SessionStore, StoreKind};
use youtube_music_core::auth::{AuthState, AuthStatus, BrowserSession, Session};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    version,
    about = "YouTube Music native core. Outputs JSON; result IDs can be passed to song, browse, queue, or lyrics."
)]
struct Cli {
    /// JSON configuration file. Prefer auth import for secure credential storage.
    #[arg(long, env = "YTMUSIC_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    pretty: bool,
    /// Saved account profile (credentials stay outside the repository).
    #[arg(long, global = true, default_value = "default")]
    profile: String,
    /// Secure storage: pass on Linux, system credential-backed vault on Windows/macOS.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    store: StoreKind,
    /// Ignore saved profiles and any credentials in the configuration file.
    #[arg(long, global = true)]
    anonymous: bool,
    /// Core operation timeout, including retries and player processing.
    #[arg(long, global = true, default_value = "120000")]
    timeout_ms: u64,
    #[command(subcommand)]
    command: Command,
    /// Generate fresh Proof of Origin tokens in a signed-in official Music browser tab.
    #[arg(long, global = true)]
    attestation_browser_port: Option<u16>,
}

#[derive(Subcommand)]
enum Command {
    /// Report local protocol/ABI versions and supported operations without network I/O.
    Capabilities,
    /// Prepare the Web player cache (reuse a persistent library client for desktop apps).
    Prewarm,
    /// List account choices exposed by the official Web session.
    Accounts,
    /// Get search completions.
    Suggestions { query: String },
    /// Read the home feed; reuse returned filters and continuation values.
    Home {
        #[arg(long)]
        params: Option<String>,
        #[arg(long)]
        continuation: Option<String>,
    },
    /// Read the explore feed.
    Explore {
        #[arg(long)]
        params: Option<String>,
        #[arg(long)]
        continuation: Option<String>,
    },
    /// Return time-aligned lyrics only when supplied by Web, otherwise plain text.
    TimedLyrics { video_id: String },
    /// Generate a DASH manifest for unchanged Web AAC audio (Windows adaptive playback).
    DashManifest { video_id: String },
    /// Read native SABR audio into a new file; emits unchanged init and media segments.
    Sabr {
        video_id: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum, default_value = "webm")]
        format: youtube_music_core::model::AudioFormat,
    },
    /// Browser login guidance, verified session import, status, and local logout.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Show the remotely verified identity of the selected account.
    Account,
    /// Read your playlists, saved songs/albums/artists, subscriptions, or likes.
    Library {
        #[arg(value_enum)]
        section: youtube_music_core::library::LibrarySection,
        #[arg(long)]
        continuation: Option<String>,
    },
    /// Search music. Use returned video_id with song, queue, player, or lyrics.
    Search {
        query: String,
        #[arg(long, value_enum, default_value = "all")]
        filter: SearchFilter,
    },
    /// Browse an album, artist, or playlist by browse_id.
    Browse { browse_id: String },
    /// Browse a playlist by playlist_id (with or without the VL prefix).
    Playlist { playlist_id: String },
    /// Fetch another page using a section's continuation token and original endpoint.
    Continue {
        #[arg(value_enum)]
        endpoint: ContinuationEndpoint,
        token: String,
    },
    /// Read song metadata, even when playback is restricted.
    Song { video_id: String },
    /// Inspect playback status and direct audio formats.
    Player { video_id: String },
    /// Resolve audio and verify CDN bytes. Pass --format mp4 for an M4A-compatible stream.
    Stream {
        video_id: String,
        #[arg(long, value_enum, default_value = "any")]
        format: youtube_music_core::model::AudioFormat,
    },
    /// Fetch the playback queue and recommendations.
    Queue { video_id: String },
    /// Fetch plain lyrics when available in this region/session.
    Lyrics { video_id: String },
    /// Read a Request JSON object from stdin (same operations as subcommands).
    Call,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Open Google sign-in; import the official Music browser session after login.
    Login {
        #[arg(long)]
        no_open: bool,
        /// Connect to an explicitly enabled local Chrome/Edge debugging port.
        #[arg(long)]
        browser_port: Option<u16>,
        /// Maximum time to wait for browser sign-in with --browser-port.
        #[arg(long, default_value = "600")]
        wait_seconds: u64,
    },
    /// Verify request headers / Cookie / Netscape cookies, then save securely.
    Import {
        /// Local file containing browser request headers; never put credentials in arguments.
        #[arg(long, conflicts_with_all = ["stdin", "browser_port"], required_unless_present_any = ["stdin", "browser_port"])]
        headers_file: Option<PathBuf>,
        /// Read credentials from stdin; only a non-secret account summary is printed.
        #[arg(long)]
        stdin: bool,
        /// Import the selected Music account from a local Chrome/Edge debugging port.
        #[arg(long, conflicts_with_all = ["stdin", "headers_file"])]
        browser_port: Option<u16>,
        /// Override the browser's account index (0..99), e.g. for Netscape exports.
        #[arg(long)]
        auth_user: Option<u32>,
    },
    /// Verify the saved session with YouTube; a cookie's presence is insufficient.
    Status,
    /// Update Music cookies, verify the account, and save changes securely.
    Refresh,
    /// Remove this local profile. Does not sign other browser sessions out of Google.
    Logout,
}

fn read_config(cli: &Cli) -> youtube_music_core::Result<Config> {
    if let Some(path) = &cli.config {
        let bytes = Zeroizing::new(
            std::fs::read(path)
                .map_err(|_| Error::InvalidInput("cannot read config file".into()))?,
        );
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::InvalidInput("invalid config JSON".into()))?;
        if cli.anonymous {
            if let Some(fields) = value.as_object_mut() {
                for key in [
                    "cookie",
                    "cookie_expirations",
                    "oauth",
                    "music_oauth",
                    "po_token",
                    "po_tokens",
                    "visitor_data",
                    "auth_user",
                    "delegated_session_id",
                ] {
                    fields.remove(key);
                }
            }
        }
        // An explicit import replaces the previous account credentials. Read
        // non-secret network settings without making legacy OAuth block its own
        // migration instructions; normal commands still reject those profiles.
        if matches!(
            &cli.command,
            Command::Auth {
                command: AuthCommand::Import { .. }
                    | AuthCommand::Login {
                        browser_port: Some(_),
                        ..
                    }
            }
        ) {
            if let Some(fields) = value.as_object_mut() {
                for key in [
                    "cookie",
                    "cookie_expirations",
                    "oauth",
                    "music_oauth",
                    "po_token",
                    "po_tokens",
                    "visitor_data",
                    "auth_user",
                    "delegated_session_id",
                ] {
                    fields.remove(key);
                }
            }
        }
        serde_json::from_value(value).map_err(|error| {
            Error::InvalidInput(
                if error
                    .to_string()
                    .contains(youtube_music_core::auth::LEGACY_AUTH_MESSAGE)
                {
                    youtube_music_core::auth::LEGACY_AUTH_MESSAGE.into()
                } else {
                    "invalid config JSON".into()
                },
            )
        })
    } else {
        Ok(Config::default())
    }
}

fn open_url(url: &str) -> youtube_music_core::Result<()> {
    use std::process::{Command as Process, Stdio};
    let mut command;
    #[cfg(target_os = "windows")]
    {
        command = Process::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command"]);
        command.arg(format!(
            "Start-Process -FilePath '{}'",
            url.replace('\'', "''")
        ));
    }
    #[cfg(target_os = "macos")]
    {
        command = Process::new("open");
        command.arg(url);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            command = Process::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command"]);
            command.arg(format!(
                "Start-Process -FilePath '{}'",
                url.replace('\'', "''")
            ));
        } else {
            command = Process::new("xdg-open");
            command.arg(url);
        }
    }
    let status = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| {
            Error::InvalidInput(
                "cannot open the browser; run auth login --no-open for the login URL".into(),
            )
        })?;
    if !status.success() {
        return Err(Error::InvalidInput(
            "browser launch failed; run auth login --no-open".into(),
        ));
    }
    Ok(())
}

fn auth_command(cli: &Cli, command: &AuthCommand) -> youtube_music_core::Result<serde_json::Value> {
    let store = SessionStore::new(cli.store, &cli.profile)?;
    match command {
        AuthCommand::Login {
            no_open,
            browser_port,
            wait_seconds,
        } => {
            if cli.anonymous {
                return Err(Error::InvalidInput(
                    "auth login cannot be combined with --anonymous".into(),
                ));
            }
            if !(1..=600).contains(wait_seconds) {
                return Err(Error::InvalidInput(
                    "wait_seconds must be between 1 and 600".into(),
                ));
            }
            if let Some(port) = browser_port {
                eprintln!("Finish sign-in in the Music browser tab.");
                return save_verified_session(cli, &store, browser::capture(*port, *wait_seconds)?);
            }
            let url = youtube_music_core::auth::LOGIN_URL;
            if !no_open && open_url(url).is_err() {
                eprintln!("Open the login_url in your browser to sign in.");
            }
            Ok(serde_json::json!({
                "state":"import_required", "method":"browser_cookie", "profile":cli.profile,
                "login_url":url,
                "instructions":"Sign in on Google's official page, then import the Music browser session. A normal browser login does not automatically share cookies with this CLI.",
                "next":format!("ytmusic --profile {} --store {} auth import --browser-port PORT", cli.profile, cli.store.as_str()),
                "alternative":format!("ytmusic --profile {} --store {} auth import --headers-file FILE", cli.profile, cli.store.as_str())
            }))
        }

        AuthCommand::Logout => Ok(
            serde_json::json!({"profile":cli.profile, "removed":store.delete()?, "scope":"saved_profile"}),
        ),
        AuthCommand::Import {
            headers_file,
            stdin,
            auth_user,
            browser_port,
        } => {
            if cli.anonymous {
                return Err(Error::InvalidInput(
                    "auth import cannot be combined with --anonymous".into(),
                ));
            }
            if let Some(port) = browser_port {
                let mut session = browser::capture(*port, 15)?;
                if let Some(index) = auth_user {
                    session.auth_user = *index;
                }
                return save_verified_session(cli, &store, session);
            }
            let mut input = Zeroizing::new(String::new());
            let reader: Box<dyn Read> = if *stdin {
                Box::new(io::stdin())
            } else {
                Box::new(
                    std::fs::File::open(
                        headers_file
                            .as_ref()
                            .ok_or_else(|| Error::InvalidInput("missing headers file".into()))?,
                    )
                    .map_err(|_| Error::InvalidInput("cannot read browser headers file".into()))?,
                )
            };
            reader
                .take(1024 * 1024 + 1)
                .read_to_string(&mut input)
                .map_err(|_| {
                    Error::InvalidInput("browser headers must be readable UTF-8".into())
                })?;
            let mut session = BrowserSession::from_browser_headers(&input)?;
            if let Some(index) = auth_user {
                session.auth_user = *index;
            }
            save_verified_session(cli, &store, session)
        }
        AuthCommand::Status | AuthCommand::Refresh => {
            unreachable!("session operations follow normal credential loading")
        }
    }
}

fn save_verified_session(
    cli: &Cli,
    store: &SessionStore,
    session: BrowserSession,
) -> youtube_music_core::Result<serde_json::Value> {
    let mut config = read_config(cli)?;
    session.apply_to(&mut config)?;
    let client = MusicClient::new(config)?;
    let account = client.account()?;
    let session = client
        .browser_session()?
        .ok_or(Error::AuthenticationRequired)?;
    store.save(&Session::Browser(session))?;
    Ok(
        serde_json::json!({"state":"authenticated", "method":"browser_cookie", "profile":cli.profile, "account":account,
        "next":format!("ytmusic --profile {} --store {} library playlists", cli.profile, cli.store.as_str())}),
    )
}

fn run(cli: &Cli) -> youtube_music_core::Result<serde_json::Value> {
    if let Command::Auth { command } = &cli.command {
        if !matches!(command, AuthCommand::Status | AuthCommand::Refresh) {
            return auth_command(cli, command);
        }
    }
    let request = match &cli.command {
        Command::Capabilities => return Ok(youtube_music_core::capabilities()),
        Command::DashManifest { video_id } => Request::DashManifest {
            video_id: video_id.clone(),
        },
        Command::Sabr {
            video_id, format, ..
        } => Request::SabrOpen {
            video_id: video_id.clone(),
            format: *format,
        },
        Command::Prewarm => Request::Prewarm,
        Command::Accounts => Request::Accounts,
        Command::Suggestions { query } => Request::SearchSuggestions {
            query: query.clone(),
        },
        Command::Home {
            params,
            continuation,
        } => Request::Home {
            params: params.clone(),
            continuation: continuation.clone(),
        },
        Command::Explore {
            params,
            continuation,
        } => Request::Explore {
            params: params.clone(),
            continuation: continuation.clone(),
        },
        Command::TimedLyrics { video_id } => Request::TimedLyrics {
            video_id: video_id.clone(),
        },
        Command::Auth {
            command: AuthCommand::Status,
        } => Request::AuthStatus,
        Command::Auth {
            command: AuthCommand::Refresh,
        } => Request::AuthRefresh,
        Command::Auth { .. } => unreachable!(),
        Command::Account => Request::Account,
        Command::Library {
            section,
            continuation,
        } => Request::Library {
            section: *section,
            continuation: continuation.clone(),
        },
        Command::Search { query, filter } => Request::Search {
            query: query.clone(),
            filter: *filter,
        },
        Command::Browse { browse_id } => Request::Browse {
            browse_id: browse_id.clone(),
        },
        Command::Playlist { playlist_id } => Request::Playlist {
            playlist_id: playlist_id.clone(),
        },
        Command::Continue { endpoint, token } => Request::Continue {
            endpoint: *endpoint,
            token: token.clone(),
        },
        Command::Song { video_id } => Request::Song {
            video_id: video_id.clone(),
        },
        Command::Player { video_id } => Request::Player {
            video_id: video_id.clone(),
        },
        Command::Stream { video_id, format } => Request::Stream {
            video_id: video_id.clone(),
            format: *format,
        },
        Command::Queue { video_id } => Request::Queue {
            video_id: video_id.clone(),
        },
        Command::Lyrics { video_id } => Request::Lyrics {
            video_id: video_id.clone(),
        },
        Command::Call => {
            let mut input = String::new();
            io::stdin()
                .take(1024 * 1024 + 1)
                .read_to_string(&mut input)
                .map_err(|_| Error::InvalidInput("cannot read UTF-8 stdin".into()))?;
            if input.len() > 1024 * 1024 {
                return Err(Error::InvalidInput("stdin exceeds 1 MiB".into()));
            }
            serde_json::from_str(&input).map_err(|_| {
                Error::InvalidInput("stdin must contain a Request JSON object".into())
            })?
        }
    };
    request.validate()?;
    if matches!(request, Request::Capabilities) {
        return Ok(youtube_music_core::capabilities());
    }
    if matches!(cli.command, Command::Call)
        && matches!(
            request,
            Request::SabrOpen { .. }
                | Request::SabrRead { .. }
                | Request::SabrSeek { .. }
                | Request::SabrClose { .. }
                | Request::SetPoTokens { .. }
        )
    {
        return Err(Error::InvalidInput("this operation needs a persistent library client; use the sabr command for a complete audio transfer".into()));
    }
    let mut config = read_config(cli)?;
    let store = SessionStore::new(cli.store, &cli.profile)?;
    let mut loaded_session = None;
    if cli.anonymous {
        config.cookie = None;
        config.cookie_expirations.clear();
        config.po_token = None;
        config.po_tokens.clear();
        config.visitor_data = None;
        config.auth_user = 0;
        config.delegated_session_id = None;
    } else if config.cookie.is_none() {
        if let Some(session) = store.load()? {
            session.apply_to(&mut config)?;
            loaded_session = Some(session);
        }
    }
    if config.cookie.is_none() {
        if matches!(request, Request::AuthStatus) {
            return Ok(serde_json::json!(AuthStatus {
                state: AuthState::SignedOut,
                account: None
            }));
        }
        if matches!(
            request,
            Request::AuthRefresh | Request::Account | Request::Library { .. }
        ) {
            return Err(Error::AuthenticationRequired);
        }
    }
    if let Some(port) = cli.attestation_browser_port {
        let video_id = match &request {
            Request::Player { video_id }
            | Request::Stream { video_id, .. }
            | Request::StreamRefresh { video_id, .. }
            | Request::DashManifest { video_id }
            | Request::SabrOpen { video_id, .. } => video_id,
            _ => return Err(Error::InvalidInput(
                "browser attestation requires a player, stream, dash-manifest or SABR open request"
                    .into(),
            )),
        };
        config.po_tokens = vec![browser::capture_po_token(port, video_id)?];
    }
    let refresh = matches!(request, Request::AuthRefresh);
    let client = MusicClient::new(config)?;
    let mut result = if let Command::Sabr {
        video_id,
        output,
        format,
    } = &cli.command
    {
        save_sabr(&client, video_id, *format, output)?
    } else {
        client.execute(request)?
    };
    let mut saved = false;
    if result.get("state").and_then(serde_json::Value::as_str) != Some("rejected") {
        if let Some(expected) = loaded_session {
            let maintenance = (|| -> youtube_music_core::Result<bool> {
                if let Some(session) = client.browser_session()? {
                    let next = Session::Browser(session);
                    if next != expected {
                        let saved = store.save_if_unchanged(&expected, &next)?;
                        if !saved {
                            eprintln!("Profile changed during this request; skipped saving older session updates.");
                        }
                        return Ok(saved);
                    }
                }
                Ok(false)
            })();
            match maintenance {
                Ok(value) => saved = value,
                Err(error) if refresh => return Err(error),
                Err(error) => eprintln!(
                    "Session update was not saved ({}); run auth refresh to retry. The requested result is still available.",
                    error.info().code
                ),
            }
        }
    }
    if refresh {
        result["saved"] = saved.into();
    }
    Ok(result)
}

fn save_sabr(
    client: &MusicClient,
    video_id: &str,
    format: youtube_music_core::model::AudioFormat,
    output: &std::path::Path,
) -> youtube_music_core::Result<serde_json::Value> {
    if output.exists() {
        return Err(Error::InvalidInput(
            "output already exists; choose a new file".into(),
        ));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| Error::InvalidInput("cannot create SABR output file".into()))?;
    let opened = client.sabr_open(video_id, format)?;
    let result = (|| {
        let mut bytes = 0u64;
        let mut segments = 0u64;
        loop {
            let chunk = client.sabr_read(opened.handle)?;
            if !chunk.data.is_empty() {
                temp.write_all(&chunk.data)
                    .map_err(|_| Error::InvalidInput("cannot write SABR output".into()))?;
                bytes += chunk.data.len() as u64;
                segments += 1;
            }
            if chunk.finished {
                break;
            }
        }
        temp.as_file()
            .sync_all()
            .map_err(|_| Error::InvalidInput("cannot flush SABR output".into()))?;
        temp.persist_noclobber(output).map_err(|_| {
            Error::InvalidInput(
                "cannot publish SABR output without overwriting an existing file".into(),
            )
        })?;
        Ok(
            serde_json::json!({"video_id":video_id,"itag":opened.itag,"mime_type":opened.mime_type,
            "duration_ms":opened.duration_ms,"bytes":bytes,"segments":segments,"output":output,"source_client":"WEB_REMIX"}),
        )
    })();
    let _ = client.sabr_close(opened.handle);
    result
}

fn main() {
    let cli = Cli::parse();
    let result = (|| {
        let operation = youtube_music_core::operation::OperationContext::new(
            youtube_music_core::operation::OperationOptions {
                timeout_ms: cli.timeout_ms,
            },
        )?;
        operation.run(|| run(&cli))
    })();
    let failed = result.is_err();
    let output = youtube_music_core::envelope(result);
    println!(
        "{}",
        if cli.pretty {
            serde_json::to_string_pretty(&output).expect("JSON value")
        } else {
            output.to_string()
        }
    );
    if failed {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_import_replaces_legacy_config_credentials_but_preserves_network_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "oauth":{"access_token":"private-access"},
                "music_oauth":{"refresh_token":"private-refresh"},
                "cookie":"SAPISID=old", "auth_user":9, "delegated_session_id":"old-account",
                "client_version":"configured-web-version", "country":"TW", "timeout_seconds":12
            }))
            .unwrap(),
        )
        .unwrap();
        for operation in [
            vec!["auth", "import", "--stdin"],
            vec!["auth", "login", "--browser-port", "9222"],
        ] {
            let cli = Cli::try_parse_from(
                std::iter::once("ytmusic".to_owned())
                    .chain(["--config".into(), path.to_string_lossy().into_owned()])
                    .chain(operation.into_iter().map(str::to_owned)),
            )
            .unwrap();
            let mut config = read_config(&cli).unwrap();
            assert!(config.cookie.is_none());
            assert_eq!(config.auth_user, 0);
            assert!(config.delegated_session_id.is_none());
            assert_eq!(config.country, "TW");
            assert_eq!(config.timeout_seconds, 12);
            assert_eq!(
                config.client_version.as_deref(),
                Some("configured-web-version")
            );
            BrowserSession {
                cookie: "SAPISID=new".into(),
                cookie_expirations: Default::default(),
                auth_user: 2,
                delegated_session_id: None,
            }
            .apply_to(&mut config)
            .unwrap();
            assert_eq!(config.cookie.as_deref(), Some("SAPISID=new"));
            assert_eq!(config.auth_user, 2);
        }
        let cli = Cli::try_parse_from([
            "ytmusic",
            "--config",
            path.to_str().unwrap(),
            "auth",
            "status",
        ])
        .unwrap();
        assert!(read_config(&cli)
            .err()
            .unwrap()
            .to_string()
            .contains("OAuth profiles are no longer supported"));
    }
}
