use clap::{Parser, Subcommand};
use std::{
    io::{self, Read},
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    /// Authorize through Google's official device page, then save OAuth tokens securely.
    Login {
        #[arg(long)]
        no_open: bool,
        /// Connect to an explicitly enabled local Chrome/Edge debugging port.
        #[arg(long)]
        browser_port: Option<u16>,
        /// Maximum time to wait for official device authorization or browser sign-in.
        #[arg(long, default_value = "1800")]
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
    /// Remove this local profile. Does not sign other browser sessions out of Google.
    Logout,
}

fn read_config(cli: &Cli) -> youtube_music_core::Result<Config> {
    if let Some(path) = &cli.config {
        let bytes = Zeroizing::new(
            std::fs::read(path)
                .map_err(|_| Error::InvalidInput("cannot read config file".into()))?,
        );
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::InvalidInput("invalid config JSON".into()))
    } else {
        Ok(Config::default())
    }
}

fn open_url(url: &str) -> youtube_music_core::Result<()> {
    use std::process::{Command as Process, Stdio};
    let mut command;
    #[cfg(target_os = "windows")]
    {
        command = Process::new("cmd.exe");
        command.args(["/C", "start", "", url]);
    }
    #[cfg(target_os = "macos")]
    {
        command = Process::new("open");
        command.arg(url);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            command = Process::new("cmd.exe");
            command.args(["/C", "start", "", url]);
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
            if let Some(port) = browser_port {
                eprintln!("Finish sign-in in the Music browser tab.");
                return save_verified_session(
                    cli,
                    &store,
                    browser::capture(*port, (*wait_seconds).min(600))?,
                );
            }
            device_login(cli, &store, *no_open, *wait_seconds)
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
        AuthCommand::Status => unreachable!("status follows normal credential loading"),
    }
}

fn device_login(
    cli: &Cli,
    store: &SessionStore,
    no_open: bool,
    wait_seconds: u64,
) -> youtube_music_core::Result<serde_json::Value> {
    use std::time::{Duration, Instant};
    use youtube_music_core::oauth::{DeviceAuthClient, PollResult};
    if !(1..=3600).contains(&wait_seconds) {
        return Err(Error::InvalidInput(
            "wait_seconds must be between 1 and 3600".into(),
        ));
    }
    let mut config = read_config(cli)?;
    let oauth = DeviceAuthClient::new(&config)?;
    let device = oauth.begin()?;
    let lifetime = wait_seconds.min(device.expires_in);
    eprintln!("Open {} and enter code: {}\nWaiting up to {} seconds for Google authorization. Profile: {}", device.verification_url, device.user_code, lifetime, cli.profile);
    if !no_open && open_url(&device.verification_url).is_err() {
        eprintln!("Open the official verification link above manually.");
    }
    let deadline = Instant::now() + Duration::from_secs(lifetime);
    let mut interval = device.interval;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::OAuth(
                "authorization timed out; run auth login again".into(),
            ));
        }
        std::thread::sleep(Duration::from_secs(interval).min(remaining));
        if Instant::now() >= deadline {
            return Err(Error::OAuth(
                "authorization timed out; run auth login again".into(),
            ));
        }
        match oauth.poll(&device)? {
            PollResult::Pending => (),
            PollResult::SlowDown => interval = interval.saturating_add(5).min(300),
            PollResult::Authorized(session) => {
                // Google has authorized this grant. Preserve it securely even if
                // Music's private API layout changes during identity verification.
                store.save(&Session::OAuth(session.clone()))?;
                session.apply_to(&mut config)?;
                let client = MusicClient::new(config)?;
                let account = client.account()?;
                if let Some(updated) = client.oauth_session()? {
                    store.save(&Session::OAuth(updated))?;
                }
                return Ok(
                    serde_json::json!({"state":"authenticated","method":"device_oauth","profile":cli.profile,"account":account,"next":format!("ytmusic --profile {} --store {} library playlists",cli.profile,cli.store.as_str())}),
                );
            }
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
    let account = MusicClient::new(config)?.account()?;
    store.save(&Session::Browser(session))?;
    Ok(
        serde_json::json!({"state":"authenticated", "profile":cli.profile, "account":account,
        "next":format!("ytmusic --profile {} --store {} library playlists", cli.profile, cli.store.as_str())}),
    )
}

fn run(cli: &Cli) -> youtube_music_core::Result<serde_json::Value> {
    if let Command::Auth { command } = &cli.command {
        if !matches!(command, AuthCommand::Status) {
            return auth_command(cli, command);
        }
    }
    let request = match &cli.command {
        Command::Auth {
            command: AuthCommand::Status,
        } => Request::AuthStatus,
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
    let mut config = read_config(cli)?;
    let store = SessionStore::new(cli.store, &cli.profile)?;
    let mut from_store = false;
    if cli.anonymous {
        config.cookie = None;
        config.oauth = None;
        config.po_token = None;
        config.auth_user = 0;
        config.delegated_session_id = None;
    } else if config.cookie.is_none() && config.oauth.is_none() {
        if let Some(session) = store.load()? {
            session.apply_to(&mut config)?;
            from_store = true;
        }
    }
    if config.cookie.is_none() && config.oauth.is_none() {
        if matches!(request, Request::AuthStatus) {
            return Ok(serde_json::json!(AuthStatus {
                state: AuthState::SignedOut,
                account: None
            }));
        }
        if matches!(request, Request::Account | Request::Library { .. }) {
            return Err(Error::AuthenticationRequired);
        }
    }
    let client = MusicClient::new(config)?;
    let previous_oauth = client.oauth_session()?;
    let result = client.execute(request);
    if from_store {
        if let Some(session) = client.oauth_session()? {
            if previous_oauth.as_ref() != Some(&session) {
                store.save(&Session::OAuth(session))?;
            }
        }
    }
    result
}

fn main() {
    let cli = Cli::parse();
    let result = run(&cli);
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
