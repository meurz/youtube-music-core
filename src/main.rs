use clap::{Parser, Subcommand};
use std::{
    io::{self, Read},
    path::PathBuf,
};
use youtube_music_core::{
    model::SearchFilter, Config, ContinuationEndpoint, Error, MusicClient, Request,
};

#[derive(Parser)]
#[command(
    version,
    about = "YouTube Music native core. Outputs JSON; result IDs can be passed to song, browse, queue, or lyrics."
)]
struct Cli {
    /// JSON configuration file. Store credentials here, not in command arguments.
    #[arg(long, env = "YTMUSIC_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    pretty: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    /// Return the highest-bitrate direct audio URL; fail if attestation/deciphering is needed.
    Stream { video_id: String },
    /// Fetch the playback queue and recommendations.
    Queue { video_id: String },
    /// Fetch plain lyrics when available in this region/session.
    Lyrics { video_id: String },
    /// Read a Request JSON object from stdin (same operations as subcommands).
    Call,
}

fn run(cli: &Cli) -> youtube_music_core::Result<serde_json::Value> {
    let request = match &cli.command {
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
        Command::Stream { video_id } => Request::Stream {
            video_id: video_id.clone(),
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
    let config = if let Some(path) = &cli.config {
        let bytes = std::fs::read(path)
            .map_err(|_| Error::InvalidInput("cannot read config file".into()))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::InvalidInput("invalid config JSON".into()))?
    } else {
        Config::default()
    };
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
