use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("write outcome is unknown; read the current state before retrying")]
    MutationUncertain(Box<Error>),
    #[error("operation cancelled")]
    Cancelled,
    #[error("operation timed out")]
    Timeout,
    #[error("YouTube rate limit reached")]
    RateLimited { retry_after_seconds: Option<u64> },
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
        "no usable Web audio URL; the player may require additional attestation or unsupported delivery"
    )]
    StreamResolutionRequired,
    #[error("protected audio requires the official player and a compatible licensed browser CDM")]
    DrmRequired,
    #[error("Web audio uses SABR; open a native SABR session to read its media segments")]
    SabrRequired,
    #[error("SABR requires a fresh official Web playback context")]
    SabrReloadRequired,
    #[error("fresh official-browser Proof of Origin attestation is required for this video")]
    PoTokenRequired,
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
}

#[derive(Serialize)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

impl Error {
    pub fn info(&self) -> ErrorInfo {
        let code = match self {
            Self::MutationUncertain(_) => "mutation_outcome_unknown",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::RateLimited { .. } => "rate_limited",
            Self::InvalidInput(_) => "invalid_input",
            Self::Network(_) => "network",
            Self::Http(_) => "http",
            Self::Protocol(_) => "protocol",
            Self::Unplayable { .. } => "unplayable",
            Self::StreamResolutionRequired => "stream_resolution_required",
            Self::DrmRequired => "drm_required",
            Self::SabrRequired => "sabr_required",
            Self::SabrReloadRequired => "sabr_reload_required",
            Self::PoTokenRequired => "po_token_required",
            Self::StreamUnavailable(_) => "stream_unavailable",
            Self::MediaValidation(_) => "media_validation",
            Self::LyricsUnavailable => "lyrics_unavailable",
            Self::AuthenticationRequired => "authentication_required",
            Self::AuthenticationRejected => "authentication_rejected",
            Self::CredentialStorage(_) => "credential_storage",
        };
        ErrorInfo {
            code,
            message: self.to_string(),
            cause_code: match self {
                Self::MutationUncertain(error) => Some(error.info().code),
                _ => None,
            },
            retryable: matches!(
                self,
                Self::Network(_)
                    | Self::Timeout
                    | Self::RateLimited { .. }
                    | Self::Http(429 | 502 | 503 | 504)
            ),
            http_status: match self {
                Self::MutationUncertain(error) => error.info().http_status,
                Self::Http(status) => Some(*status),
                Self::RateLimited { .. } => Some(429),
                _ => None,
            },
            retry_after_seconds: match self {
                Self::MutationUncertain(error) => error.info().retry_after_seconds,
                Self::RateLimited {
                    retry_after_seconds,
                } => *retry_after_seconds,
                _ => None,
            },
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
