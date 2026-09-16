use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackClient {
    #[default]
    Auto,
    AndroidVr,
    WebRemix,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    #[default]
    Any,
    Mp4,
    Webm,
}

impl AudioFormat {
    pub(crate) fn matches(self, mime: &str) -> bool {
        let container = mime.split(';').next().unwrap_or("").trim();
        match self {
            Self::Any => matches!(container, "audio/mp4" | "audio/webm"),
            Self::Mp4 => container == "audio/mp4",
            Self::Webm => container == "audio/webm",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub(crate) fn params(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Songs => Some("EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D"),
            Self::Videos => Some("EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D"),
            Self::Albums => Some("EgWKAQIYAWoKEAkQChAFEAMQBA%3D%3D"),
            Self::Artists => Some("EgWKAQIgAWoKEAkQChAFEAMQBA%3D%3D"),
            Self::Playlists => Some("EgWKAQIoAWoKEAkQChAFEAMQBA%3D%3D"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Link {
    pub name: String,
    pub browse_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thumbnail {
    pub url: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub title: String,
    pub kind: String,
    pub video_id: Option<String>,
    pub browse_id: Option<String>,
    pub artists: Vec<Link>,
    pub album: Option<Link>,
    pub duration_seconds: Option<u64>,
    pub thumbnails: Vec<Thumbnail>,
    pub explicit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub title: Option<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub author: Option<String>,
    pub channel_id: Option<String>,
    pub duration_seconds: Option<u64>,
    pub thumbnails: Vec<Thumbnail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStream {
    pub itag: u64,
    pub url: String,
    pub mime_type: String,
    pub bitrate: Option<u64>,
    pub content_length: Option<u64>,
    pub audio_quality: Option<String>,
    /// Unix timestamp from the signed URL. Resolve again after expiry.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Public request headers for the media host; never contains account cookies.
    #[serde(default)]
    pub http_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub source_client: Option<String>,
    /// Present only after a bounded media GET succeeds and matches the container.
    #[serde(default)]
    pub verification: Option<StreamVerification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamVerification {
    pub status: u16,
    pub bytes_read: usize,
    pub content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    #[serde(default)]
    pub source_client: Option<String>,
    pub track: Option<Track>,
    pub status: String,
    pub reason: Option<String>,
    pub expires_in_seconds: Option<u64>,
    pub audio_streams: Vec<AudioStream>,
    pub unresolved_audio_formats: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lyrics {
    pub browse_id: String,
    pub text: String,
    pub source: Option<String>,
}
