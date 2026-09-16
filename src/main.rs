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
use youtube_music_core::auth::{AuthState, AuthStatus, BrowserSession, LOGIN_URL};
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
    /// Open the Music sign-in page and explain how to import its browser session.
    Login {
        #[arg(long)]
        no_open: bool,
        /// Connect to an explicitly enabled local Chrome/Edge debugging port.
        #[arg(long, conflicts_with = "no_open")]
        browser_port: Option<u16>,
        /// Allow time for manual browser login/MFA when using --browser-port.
        #[arg(long, default_value = "300")]
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

fn open_login() -> youtube_music_core::Result<()> {
    use std::process::{Command as Process, Stdio};
    let mut command;
    #[cfg(target_os = "windows")]
    {
        command = Process::new("cmd.exe");
        command.args(["/C", "start", "", "https://music.youtube.com/"]);
    }
    #[cfg(target_os = "macos")]
    {
        command = Process::new("open");
        command.arg(LOGIN_URL);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if std::env::var_os("WSL_DISTRO_NAME").is_some() {
            command = Process::new("cmd.exe");
            command.args(["/C", "start", "", "https://music.youtube.com/"]);
        } else {
            command = Process::new("xdg-open");
            command.arg(LOGIN_URL);
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
            if let Some(port) = browser_port {
                if cli.anonymous {
                    return Err(Error::InvalidInput(
                        "auth login cannot be combined with --anonymous".into(),
                    ));
                }
                eprintln!("Use the Music tab in your browser to finish sign-in/account selection; waiting up to {wait_seconds} seconds.");
                return save_verified_session(cli, &store, browser::capture(*port, *wait_seconds)?);
            }
            if !no_open {
                open_login()?;
            }
            Ok(
                serde_json::json!({"state":"browser_action_required", "login_url":LOGIN_URL,
                "profile":cli.profile, "next":format!("ytmusic --profile {} --store {} auth import --headers-file /path/to/browser-headers.txt", cli.profile, cli.store.as_str()),
                "browser_debugging":"With a local Chrome/Edge debugging port enabled, run auth login --browser-port PORT to import directly.",
                "instructions":["Sign in to YouTube Music and select the intended account.",
                    "Open browser Developer Tools > Network, reload Music, then select a music.youtube.com/youtubei/v1/browse request.",
                    "Copy its request headers (Cookie and X-Goog-AuthUser) into a private local file, or pipe them to auth import --stdin.",
                    "Import verifies the selected account before saving. Delete the temporary header file afterward.",
                    "Use the same --profile and --store options for import, status, library, and logout."]}),
            )
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

fn save_verified_session(
    cli: &Cli,
    store: &SessionStore,
    session: BrowserSession,
) -> youtube_music_core::Result<serde_json::Value> {
    let mut config = read_config(cli)?;
    session.apply_to(&mut config)?;
    let account = MusicClient::new(config)?.account()?;
    store.save(&session)?;
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
    if cli.anonymous {
        config.cookie = None;
        config.auth_user = 0;
        config.delegated_session_id = None;
    } else if config.cookie.is_none() {
        if let Some(session) = SessionStore::new(cli.store, &cli.profile)?.load()? {
            session.apply_to(&mut config)?;
        }
    }
    if config.cookie.is_none() {
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
    MusicClient::new(config)?.execute(request)
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
