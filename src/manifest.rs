//! A DASH descriptor for the official fragmented MP4, without media rewriting.
use crate::{
    client::validate_video_id,
    model::{AudioFormat, AudioStream, ByteRange},
    Error, MusicClient, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Contains the same signed media URL as an AudioStream. Do not log the manifest.
#[derive(Clone, Serialize, Deserialize)]
pub struct DashManifest {
    pub video_id: String,
    pub itag: u64,
    pub manifest: String,
    pub mime_type: String,
    pub source_client: String,
    pub expires_at: Option<u64>,
    /// Public CDN headers, never Music authentication cookies.
    pub http_headers: BTreeMap<String, String>,
}

fn metadata_error() -> Error {
    Error::StreamUnavailable("verified MP4 is missing valid DASH segment metadata".into())
}

fn ranges(audio: &AudioStream) -> Result<(ByteRange, ByteRange)> {
    let init = audio.init_range.ok_or_else(metadata_error)?;
    let index = audio.index_range.ok_or_else(metadata_error)?;
    let length = audio.content_length.ok_or_else(metadata_error)?;
    // The initialization covers the file header. Index and media must fit inside
    // the same resource and cannot overlap the initialization or each other.
    if init.start != 0
        || init.end < init.start
        || index.end < index.start
        || index.start <= init.end
        || index.end >= length.saturating_sub(1)
        || init.end >= length
    {
        return Err(metadata_error());
    }
    Ok((init, index))
}

fn xml(value: &str) -> Result<String> {
    if value.chars().any(|c| {
        !(matches!(c, '\t' | '\n' | '\r')
            || ('\u{20}'..='\u{d7ff}').contains(&c)
            || ('\u{e000}'..='\u{fffd}').contains(&c)
            || ('\u{10000}'..='\u{10ffff}').contains(&c))
    }) {
        return Err(metadata_error());
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn codec(mime: &str) -> Option<&str> {
    let mut parts = mime.split(';');
    if parts.next()?.trim() != "audio/mp4" {
        return None;
    }
    let codec = parts.find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key.trim() == "codecs").then(|| value.trim().trim_matches('"'))
    })?;
    (codec.starts_with("mp4a.")
        && codec.len() <= 64
        && codec
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric())))
    .then_some(codec)
}

fn build(video_id: &str, audio: &AudioStream) -> Result<DashManifest> {
    validate_video_id(video_id)?;
    if audio.source_client.as_deref() != Some("WEB_REMIX") || audio.verification.is_none() {
        return Err(metadata_error());
    }
    let codec = codec(&audio.mime_type).ok_or_else(metadata_error)?;
    let (init, index) = ranges(audio)?;
    let duration = audio
        .duration_ms
        .filter(|n| *n > 0)
        .ok_or_else(metadata_error)?;
    let rate = audio
        .audio_sample_rate
        .filter(|n| (1..=384000).contains(n))
        .ok_or_else(metadata_error)?;
    let channels = audio
        .audio_channels
        .filter(|n| (1..=32).contains(n))
        .ok_or_else(metadata_error)?;
    let bandwidth = audio
        .bitrate
        .filter(|n| *n > 0)
        .ok_or_else(metadata_error)?;
    // Validation/probing already checked the CDN origin. Escaping is still
    // necessary: signed URL query separators must remain valid XML text.
    let url = xml(&audio.url)?;
    let manifest = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" profiles=\"urn:mpeg:dash:profile:isoff-on-demand:2011\" minBufferTime=\"PT1.500S\" mediaPresentationDuration=\"PT{}.{:03}S\">\n\
  <Period start=\"PT0S\">\n\
    <AdaptationSet mimeType=\"audio/mp4\" contentType=\"audio\" codecs=\"{codec}\">\n\
      <Representation id=\"{}\" bandwidth=\"{bandwidth}\" audioSamplingRate=\"{rate}\">\n\
        <AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{channels}\"/>\n\
        <BaseURL>{url}</BaseURL>\n\
        <SegmentBase indexRange=\"{}-{}\" indexRangeExact=\"true\">\n\
          <Initialization range=\"{}-{}\"/>\n\
        </SegmentBase>\n\
      </Representation>\n\
    </AdaptationSet>\n\
  </Period>\n\
</MPD>\n",
        duration / 1000, duration % 1000, audio.itag, index.start, index.end, init.start, init.end,
    );
    Ok(DashManifest {
        video_id: video_id.into(),
        itag: audio.itag,
        manifest,
        mime_type: "application/dash+xml".into(),
        source_client: "WEB_REMIX".into(),
        expires_at: audio.expires_at,
        http_headers: audio.http_headers.clone(),
    })
}

impl MusicClient {
    /// Resolve and verify MP4, then describe its existing DASH segmentation for
    /// an adaptive media source. No audio download/remux or local server occurs.
    pub fn dash_manifest(&self, video_id: &str) -> Result<DashManifest> {
        crate::operation::ensure(|| {
            build(video_id, &self.stream_format(video_id, AudioFormat::Mp4)?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn audio() -> AudioStream {
        serde_json::from_value(json!({
            "itag":141,"url":"https://r1.googlevideo.com/videoplayback?x=1&y=2",
            "mime_type":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":285630,
            "content_length":11898839,"audio_quality":"AUDIO_QUALITY_HIGH",
            "init_range":{"start":0,"end":758},"index_range":{"start":759,"end":1246},
            "duration_ms":369626,"audio_sample_rate":44100,"audio_channels":2,
            "source_client":"WEB_REMIX","verification":{"status":206,"bytes_read":4096,"content_type":"audio/mp4"}
        })).unwrap()
    }
    #[test]
    fn descriptor_preserves_millisecond_duration_ranges_and_xml_url() {
        let result = build("4D7u5KF7SP8", &audio()).unwrap();
        assert!(result.manifest.contains("PT369.626S"));
        assert!(result.manifest.contains("indexRange=\"759-1246\""));
        assert!(result.manifest.contains("range=\"0-758\""));
        assert!(result.manifest.contains("?x=1&amp;y=2"));
        assert!(result.manifest.contains("audioSamplingRate=\"44100\""));
        assert_eq!(result.mime_type, "application/dash+xml");
    }
    #[test]
    fn descriptor_rejects_missing_or_overlapping_out_of_bounds_ranges() {
        for change in 0..6 {
            let mut a = audio();
            match change {
                0 => a.init_range = None,
                1 => {
                    a.index_range = Some(ByteRange {
                        start: 500,
                        end: 1000,
                    })
                }
                2 => {
                    a.index_range = Some(ByteRange {
                        start: 759,
                        end: u64::MAX,
                    })
                }
                3 => a.content_length = Some(1247),
                4 => a.init_range = Some(ByteRange { start: 1, end: 758 }),
                _ => {
                    a.index_range = Some(ByteRange {
                        start: 1000,
                        end: 759,
                    })
                }
            }
            assert!(build("4D7u5KF7SP8", &a).is_err());
        }
    }
    #[test]
    fn descriptor_requires_verified_aac_and_complete_audio_metadata() {
        for change in 0..6 {
            let mut a = audio();
            match change {
                0 => a.verification = None,
                1 => a.mime_type = "audio/webm; codecs=\"opus\"".into(),
                2 => a.duration_ms = None,
                3 => a.audio_sample_rate = Some(0),
                4 => a.audio_channels = None,
                _ => a.bitrate = None,
            }
            assert!(build("4D7u5KF7SP8", &a).is_err());
        }
        assert!(xml("x\0y").is_err());
        assert_eq!(xml("<&\"'>").unwrap(), "&lt;&amp;&quot;&apos;&gt;");
    }
    #[test]
    fn official_player_fields_keep_numeric_string_precision() {
        let v = json!({"playabilityStatus":{"status":"OK"},"streamingData":{"adaptiveFormats":[{
            "itag":141,"url":"https://r1.googlevideo.com/videoplayback?x=1",
            "mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","initRange":{"start":"0","end":"758"},
            "indexRange":{"start":"759","end":"1246"},"approxDurationMs":"369626",
            "audioSampleRate":"44100","audioChannels":2
        }]}});
        let p = crate::parse::player(&v).unwrap();
        let a = &p.audio_streams[0];
        assert_eq!(a.duration_ms, Some(369626));
        assert_eq!(a.audio_sample_rate, Some(44100));
        assert_eq!(a.audio_channels, Some(2));
        assert_eq!(
            a.index_range,
            Some(ByteRange {
                start: 759,
                end: 1246
            })
        );
    }
}
