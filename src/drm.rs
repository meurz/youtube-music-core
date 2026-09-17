//! DRM is played by the official Web player and its licensed browser CDM.
//!
//! A CDN response with encrypted bytes is not a playable clear audio stream.
//! Descriptors intentionally omit license URLs, DRM parameters and challenges:
//! the official page must obtain the license in its own account/browser context.
use crate::{client::validate_video_id, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrmPlayback {
    pub video_id: String,
    pub source_client: String,
    pub official_watch_url: String,
    pub families: Vec<String>,
    pub key_systems: Vec<String>,
    pub license_families: Vec<String>,
    pub encrypted_audio_formats: usize,
    pub license_acquisition: String,
}

impl DrmPlayback {
    /// A host may open this official page even when the API cannot supply media.
    /// Availability remains subject to the browser CDM and the account's rights.
    pub fn official(video_id: &str) -> Result<Self> {
        validate_video_id(video_id)?;
        Ok(Self {
            video_id: video_id.into(),
            source_client: "WEB_REMIX".into(),
            official_watch_url: format!("https://music.youtube.com/watch?v={video_id}"),
            families: Vec::new(),
            key_systems: Vec::new(),
            license_families: Vec::new(),
            encrypted_audio_formats: 0,
            license_acquisition: "official_player".into(),
        })
    }
}

fn family(value: &Value) -> Option<String> {
    let value = value.as_str()?;
    // Retain a bounded unknown family so future systems are not mistaken for
    // clear media, without echoing arbitrary server/private strings.
    if matches!(value, "WIDEVINE" | "PLAYREADY" | "FAIRPLAY" | "CLEARKEY") {
        Some(value.into())
    } else {
        Some("UNKNOWN".into())
    }
}

/// Presence of any DRM family marks the format protected, including unknown
/// families. Malformed nonempty values must never be treated as clear media.
pub(crate) fn is_encrypted_format(format: &Value) -> bool {
    match format.get("drmFamilies") {
        None | Some(Value::Null) => false,
        Some(Value::Array(values)) => !values.is_empty(),
        Some(_) => true,
    }
}

pub(crate) fn parse_player_drm(raw: &Value) -> Option<DrmPlayback> {
    let video_id = raw["videoDetails"]["videoId"].as_str()?;
    let mut descriptor = DrmPlayback::official(video_id).ok()?;
    let mut families = BTreeSet::new();
    for format in raw["streamingData"]["adaptiveFormats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|format| {
            format["mimeType"]
                .as_str()
                .is_some_and(|mime| mime.starts_with("audio/"))
                && is_encrypted_format(format)
        })
    {
        descriptor.encrypted_audio_formats += 1;
        if let Some(values) = format["drmFamilies"].as_array() {
            families.extend(
                values
                    .iter()
                    .map(|v| family(v).unwrap_or_else(|| "UNKNOWN".into())),
            );
        } else {
            families.insert("UNKNOWN".into());
        }
    }
    let license_families: BTreeSet<_> = raw["streamingData"]["licenseInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|info| family(&info["drmFamily"]))
        .collect();
    families.extend(license_families.iter().cloned());
    if families.is_empty() {
        return None;
    }
    descriptor.key_systems = families
        .iter()
        .filter_map(|name| match name.as_str() {
            "WIDEVINE" => Some("com.widevine.alpha".into()),
            "PLAYREADY" => Some("com.microsoft.playready.recommendation".into()),
            "FAIRPLAY" => Some("com.apple.fps".into()),
            "CLEARKEY" => Some("org.w3.clearkey".into()),
            _ => None,
        })
        .collect();
    descriptor.families = families.into_iter().collect();
    descriptor.license_families = license_families.into_iter().collect();
    Some(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describes_official_license_route_without_leaking_license_material() {
        let raw = json!({"videoDetails":{"videoId":"4D7u5KF7SP8"},"streamingData":{
            "licenseInfos":[{"drmFamily":"WIDEVINE","url":"https://example.invalid/private-license?secret=never-export"}],
            "drmParams":"never-export",
            "adaptiveFormats":[{"mimeType":"audio/mp4", "drmFamilies":["WIDEVINE"]},
                {"mimeType":"video/mp4","drmFamilies":["PLAYREADY"]}]
        }});
        let descriptor = parse_player_drm(&raw).unwrap();
        assert_eq!(descriptor.encrypted_audio_formats, 1);
        assert_eq!(descriptor.key_systems, ["com.widevine.alpha"]);
        assert_eq!(descriptor.license_acquisition, "official_player");
        assert!(!serde_json::to_string(&descriptor)
            .unwrap()
            .contains("never-export"));
    }

    #[test]
    fn unknown_or_malformed_drm_is_never_clear_audio() {
        assert!(is_encrypted_format(&json!({"drmFamilies":"WIDEVINE"})));
        assert!(is_encrypted_format(&json!({"drmFamilies":[null]})));
        assert!(!is_encrypted_format(&json!({"drmFamilies":[]})));
        assert!(!is_encrypted_format(&json!({})));
        let descriptor = parse_player_drm(&json!({"videoDetails":{"videoId":"4D7u5KF7SP8"},
            "streamingData":{"adaptiveFormats":[{"mimeType":"audio/mp4","drmFamilies":["FUTURE_SYSTEM"]}]}})).unwrap();
        assert_eq!(descriptor.families, ["UNKNOWN"]);
        assert!(descriptor.key_systems.is_empty());
    }

    #[test]
    fn validates_watch_id_and_does_not_mark_clear_audio_protected() {
        assert!(DrmPlayback::official("../not-a-video").is_err());
        assert!(
            parse_player_drm(&json!({"videoDetails":{"videoId":"4D7u5KF7SP8"},
            "streamingData":{"adaptiveFormats":[{"mimeType":"audio/mp4"}]}}))
            .is_none()
        );
    }
}
