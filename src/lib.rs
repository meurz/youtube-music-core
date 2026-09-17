//! Native, blocking YouTube Music client. No browser, Python, or yt-dlp runtime.
pub mod auth;
mod client;
mod decipher;
pub mod discovery;
mod error;
mod ffi;
pub mod library;
pub mod model;
pub mod mutations;
pub mod operation;
pub mod parse;
mod playback;
mod session;
mod transport;
pub use playback::PlaybackWarmup;

pub use client::{Config, ContinuationEndpoint, MusicClient};
pub use error::{Error, ErrorInfo, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Capabilities,
    Accounts,
    SearchSuggestions {
        query: String,
    },
    Home {
        #[serde(default)]
        params: Option<String>,
        #[serde(default)]
        continuation: Option<String>,
    },
    Explore {
        #[serde(default)]
        params: Option<String>,
        #[serde(default)]
        continuation: Option<String>,
    },
    QueueContext {
        #[serde(flatten)]
        context: discovery::QueueContext,
    },
    TimedLyrics {
        video_id: String,
    },
    Prewarm,
    PlaybackReset,
    Prefetch {
        video_ids: Vec<String>,
        #[serde(default)]
        format: model::AudioFormat,
    },
    StreamRefresh {
        video_id: String,
        #[serde(default)]
        format: model::AudioFormat,
    },
    RateSong {
        video_id: String,
        rating: mutations::Rating,
    },
    RatePlaylist {
        playlist_id: String,
        rating: mutations::Rating,
    },
    EditLibrary {
        feedback_tokens: Vec<String>,
    },
    Subscribe {
        channel_id: String,
        subscribed: bool,
    },
    CreatePlaylist {
        #[serde(flatten)]
        options: mutations::CreatePlaylist,
    },
    EditPlaylist {
        playlist_id: String,
        #[serde(flatten)]
        options: mutations::EditPlaylist,
    },
    DeletePlaylist {
        playlist_id: String,
    },
    AddPlaylistItems {
        playlist_id: String,
        video_ids: Vec<String>,
        #[serde(default)]
        allow_duplicates: bool,
    },
    RemovePlaylistItems {
        playlist_id: String,
        entries: Vec<mutations::PlaylistEntry>,
    },
    MovePlaylistItem {
        playlist_id: String,
        set_video_id: String,
        #[serde(default)]
        before_set_video_id: Option<String>,
    },
    AuthStatus,
    AuthRefresh,
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
            Self::Accounts => Ok(()),
            Self::SearchSuggestions { query } => client::nonempty(query, "query"),
            Self::Home {
                params,
                continuation,
            }
            | Self::Explore {
                params,
                continuation,
            } => {
                for (v, n) in [(params, "params"), (continuation, "continuation")] {
                    if let Some(v) = v {
                        client::nonempty(v, n)?;
                    }
                }
                Ok(())
            }
            Self::QueueContext { context } => context.validate(),
            Self::TimedLyrics { video_id } => client::validate_video_id(video_id),
            Self::Capabilities
            | Self::Prewarm
            | Self::PlaybackReset
            | Self::AuthStatus
            | Self::AuthRefresh
            | Self::Account => Ok(()),
            Self::Prefetch { video_ids, .. } => {
                if video_ids.is_empty() || video_ids.len() > 3 {
                    return Err(Error::InvalidInput(
                        "prefetch requires 1 to 3 video IDs".into(),
                    ));
                }
                video_ids
                    .iter()
                    .try_for_each(|id| client::validate_video_id(id))
            }
            Self::StreamRefresh { video_id, .. } => client::validate_video_id(video_id),
            Self::RateSong { .. }
            | Self::RatePlaylist { .. }
            | Self::EditPlaylist { .. }
            | Self::DeletePlaylist { .. }
            | Self::AddPlaylistItems { .. }
            | Self::RemovePlaylistItems { .. }
            | Self::MovePlaylistItem { .. }
            | Self::CreatePlaylist { .. }
            | Self::EditLibrary { .. }
            | Self::Subscribe { .. } => mutations::validate_request(self),
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
        operation::ensure(|| self.execute_inner(request))
    }

    fn execute_inner(&self, request: Request) -> Result<Value> {
        request.validate()?;
        let value = match request {
            Request::Accounts => serde_json::to_value(self.accounts()?),
            Request::SearchSuggestions { query } => {
                serde_json::to_value(self.search_suggestions(&query)?)
            }
            Request::Home {
                params,
                continuation,
            } => serde_json::to_value(self.home(params.as_deref(), continuation.as_deref())?),
            Request::Explore {
                params,
                continuation,
            } => serde_json::to_value(self.explore(params.as_deref(), continuation.as_deref())?),
            Request::QueueContext { context } => {
                serde_json::to_value(self.queue_context(&context)?)
            }
            Request::TimedLyrics { video_id } => {
                serde_json::to_value(self.timed_lyrics(&video_id)?)
            }
            Request::Capabilities => Ok(capabilities()),
            Request::Prewarm => serde_json::to_value(self.prewarm()?),
            Request::PlaybackReset => {
                self.invalidate_playback()?;
                Ok(json!({"invalidated":true}))
            }
            Request::Prefetch { video_ids, format } => {
                serde_json::to_value(self.prefetch(&video_ids, format)?)
            }
            Request::StreamRefresh { video_id, format } => {
                serde_json::to_value(self.stream_format_with_options(&video_id, format, true)?)
            }
            Request::RateSong { video_id, rating } => {
                serde_json::to_value(self.rate_song(&video_id, rating)?)
            }
            Request::RatePlaylist {
                playlist_id,
                rating,
            } => serde_json::to_value(self.rate_playlist(&playlist_id, rating)?),
            Request::EditLibrary { feedback_tokens } => {
                serde_json::to_value(self.edit_library(&feedback_tokens)?)
            }
            Request::Subscribe {
                channel_id,
                subscribed,
            } => serde_json::to_value(self.subscribe_artist(&channel_id, subscribed)?),
            Request::CreatePlaylist { options } => {
                serde_json::to_value(self.create_playlist(&options)?)
            }
            Request::EditPlaylist {
                playlist_id,
                options,
            } => serde_json::to_value(self.edit_playlist(&playlist_id, &options)?),
            Request::DeletePlaylist { playlist_id } => {
                serde_json::to_value(self.delete_playlist(&playlist_id)?)
            }
            Request::AddPlaylistItems {
                playlist_id,
                video_ids,
                allow_duplicates,
            } => serde_json::to_value(self.add_playlist_items(
                &playlist_id,
                &video_ids,
                allow_duplicates,
            )?),
            Request::RemovePlaylistItems {
                playlist_id,
                entries,
            } => serde_json::to_value(self.remove_playlist_items(&playlist_id, &entries)?),
            Request::MovePlaylistItem {
                playlist_id,
                set_video_id,
                before_set_video_id,
            } => serde_json::to_value(self.move_playlist_item(
                &playlist_id,
                &set_video_id,
                before_set_video_id.as_deref(),
            )?),
            Request::AuthStatus => serde_json::to_value(self.auth_status()?),
            Request::AuthRefresh => serde_json::to_value(self.refresh_session()?),
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
    let result = operation::ensure(|| {
        let call: Call = serde_json::from_str(input).map_err(|error| {
            Error::InvalidInput(if error.to_string().contains(auth::LEGACY_AUTH_MESSAGE) {
                auth::LEGACY_AUTH_MESSAGE.into()
            } else {
                "expected {config?, request:{op,...}}".into()
            })
        })?;
        call.request.validate()?;
        if matches!(call.request, Request::Capabilities) {
            return Ok(capabilities());
        }
        MusicClient::new(call.config)?.execute(call.request)
    });
    envelope(result).to_string()
}

/// Version/capability discovery is local and contains no session information.
pub fn capabilities() -> Value {
    json!({"protocol_version":"1.1", "abi_version":2, "core_version":env!("CARGO_PKG_VERSION"),
        "client":"WEB_REMIX", "authentication":"browser_cookie",
        "features":{"cancellation":true,"operation_deadline":true,"progress":true,"read_retries":true,"prewarm":true,"stream_cache":true,"stream_refresh":true,"library_writes":true,"account_selection":true,"timed_lyrics":"when_provided_by_web"},
        "operations":["capabilities","auth_status","auth_refresh","account","accounts","library","search","search_suggestions","home","explore","browse","playlist","continue","song","player","stream","stream_refresh","prewarm","prefetch","playback_reset","queue","queue_context","lyrics","timed_lyrics","rate_song","rate_playlist","edit_library","subscribe","create_playlist","edit_playlist","delete_playlist","add_playlist_items","remove_playlist_items","move_playlist_item"],
        "limits":{"prefetch_tracks":3,"operation_timeout_ms_max":600000,"native_clients":128,"native_operations":256},
        "unsupported":["po_token_generation","sabr","drm"]})
}
