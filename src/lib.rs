//! Native, blocking YouTube Music client. No browser, Python, or yt-dlp runtime.
pub mod auth;
mod client;
mod error;
mod ffi;
pub mod library;
pub mod model;
pub mod oauth;
pub mod parse;
mod playback;

pub use client::{Config, ContinuationEndpoint, MusicClient};
pub use error::{Error, ErrorInfo, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    AuthStatus,
    Account,
    Library {
        section: library::LibrarySection,
        #[serde(default)]
        continuation: Option<String>,
    },
    Search {
        query: String,
        #[serde(default)]
        filter: model::SearchFilter,
    },
    Browse {
        browse_id: String,
    },
    Playlist {
        playlist_id: String,
    },
    Continue {
        endpoint: ContinuationEndpoint,
        token: String,
    },
    Song {
        video_id: String,
    },
    Player {
        video_id: String,
    },
    Stream {
        video_id: String,
        #[serde(default)]
        format: model::AudioFormat,
    },
    Queue {
        video_id: String,
    },
    Lyrics {
        video_id: String,
    },
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::AuthStatus | Self::Account => Ok(()),
            Self::Library { continuation, .. } => continuation
                .as_deref()
                .map(|token| client::nonempty(token, "continuation"))
                .unwrap_or(Ok(())),
            Self::Search { query, .. } => client::nonempty(query, "query"),
            Self::Browse { browse_id } => client::nonempty(browse_id, "browse_id"),
            Self::Playlist { playlist_id } => client::nonempty(playlist_id, "playlist_id"),
            Self::Continue { token, .. } => client::nonempty(token, "token"),
            Self::Song { video_id }
            | Self::Player { video_id }
            | Self::Stream { video_id, .. }
            | Self::Queue { video_id }
            | Self::Lyrics { video_id } => client::validate_video_id(video_id),
        }
    }
}

impl MusicClient {
    pub fn execute(&self, request: Request) -> Result<Value> {
        request.validate()?;
        let value = match request {
            Request::AuthStatus => serde_json::to_value(self.auth_status()?),
            Request::Account => serde_json::to_value(self.account()?),
            Request::Library {
                section,
                continuation,
            } => serde_json::to_value(self.library(section, continuation.as_deref())?),
            Request::Search { query, filter } => serde_json::to_value(self.search(&query, filter)?),
            Request::Browse { browse_id } => serde_json::to_value(self.browse(&browse_id)?),
            Request::Playlist { playlist_id } => serde_json::to_value(self.playlist(&playlist_id)?),
            Request::Continue { endpoint, token } => {
                serde_json::to_value(self.continue_page(endpoint, &token)?)
            }
            Request::Song { video_id } => serde_json::to_value(self.song(&video_id)?),
            Request::Player { video_id } => serde_json::to_value(self.player(&video_id)?),
            Request::Stream { video_id, format } => {
                serde_json::to_value(self.stream_format(&video_id, format)?)
            }
            Request::Queue { video_id } => serde_json::to_value(self.queue(&video_id)?),
            Request::Lyrics { video_id } => serde_json::to_value(self.lyrics(&video_id)?),
        };
        value.map_err(|_| Error::Protocol("could not serialize result".into()))
    }
}

pub fn envelope(result: Result<Value>) -> Value {
    match result {
        Ok(data) => json!({"ok":true, "data":data}),
        Err(error) => json!({"ok":false, "error":error.info()}),
    }
}

/// One-shot JSON bridge. Prefer a persistent MusicClient for connection reuse.
pub fn core_call(input: &str) -> String {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Call {
        #[serde(default)]
        config: Config,
        request: Request,
    }
    let result = (|| {
        let call: Call = serde_json::from_str(input)
            .map_err(|_| Error::InvalidInput("expected {config?, request:{op,...}}".into()))?;
        call.request.validate()?;
        MusicClient::new(call.config)?.execute(call.request)
    })();
    envelope(result).to_string()
}
