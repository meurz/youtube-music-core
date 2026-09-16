use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid request: {0}")]
    InvalidInput(String),
    #[error("network request failed: {0}")]
    Network(String),
    #[error("YouTube returned HTTP {0}")]
    Http(u16),
    #[error("unexpected YouTube response: {0}")]
    Protocol(String),
    #[error("playback unavailable ({status}): {reason}")]
    Unplayable { status: String, reason: String },
    #[error(
        "no direct audio URL; signature/n deciphering or additional player attestation is required"
    )]
    StreamResolutionRequired,
    #[error("lyrics are unavailable for this track")]
    LyricsUnavailable,
}

#[derive(Serialize)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn info(&self) -> ErrorInfo {
        let code = match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::Network(_) => "network",
            Self::Http(_) => "http",
            Self::Protocol(_) => "protocol",
            Self::Unplayable { .. } => "unplayable",
            Self::StreamResolutionRequired => "stream_resolution_required",
            Self::LyricsUnavailable => "lyrics_unavailable",
        };
        ErrorInfo {
            code,
            message: self.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
