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
    #[error("no verified audio stream: {0}")]
    StreamUnavailable(String),
    #[error("media validation failed: {0}")]
    MediaValidation(String),
    #[error("lyrics are unavailable for this track")]
    LyricsUnavailable,
    #[error("sign in to YouTube Music first")]
    AuthenticationRequired,
    #[error("YouTube rejected the selected session; sign in again")]
    AuthenticationRejected,
    #[error("credential storage failed: {0}")]
    CredentialStorage(String),
    #[error("device authorization failed: {0}")]
    OAuth(String),
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
            Self::StreamUnavailable(_) => "stream_unavailable",
            Self::MediaValidation(_) => "media_validation",
            Self::LyricsUnavailable => "lyrics_unavailable",
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthenticationRejected => "authentication_rejected",
            Self::CredentialStorage(_) => "credential_storage",
            Self::OAuth(_) => "oauth",
        };
        ErrorInfo {
            code,
            message: self.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
