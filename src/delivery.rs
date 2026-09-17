//! Stateful Web SABR audio delivery for native desktop hosts.
use crate::{
    model::AudioFormat,
    sabr::{SabrConfig, SabrFormat, SabrSession},
    Error, MusicClient, Result,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Signed delivery parameters. Serialize only to a trusted host, never a log.
#[derive(Clone, Serialize, Deserialize)]
pub struct SabrDescriptor {
    pub server_abr_streaming_url: String,
    pub ustreamer_config: String,
    pub formats: Vec<SabrFormat>,
}
impl std::fmt::Debug for SabrDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SabrDescriptor")
            .field("formats", &self.formats.len())
            .finish_non_exhaustive()
    }
}

pub(crate) fn parse_sabr(raw: &Value, resolved: bool) -> Option<SabrDescriptor> {
    let url = raw["streamingData"]["serverAbrStreamingUrl"].as_str()?;
    let parsed = crate::playback::media_url(url).ok()?;
    if !resolved && parsed.query_pairs().any(|(k, _)| k == "n") {
        return None;
    }
    let config = raw["playerConfig"]["mediaCommonConfig"]["mediaUstreamerRequestConfig"]
        ["videoPlaybackUstreamerConfig"]
        .as_str()?;
    if config.is_empty() || config.len() > 1024 * 1024 {
        return None;
    }
    fn number(v: &Value) -> Option<u64> {
        v.as_u64().or_else(|| v.as_str()?.parse().ok())
    }
    let mut formats = raw["streamingData"]["adaptiveFormats"]
        .as_array()?
        .iter()
        .filter_map(|v| {
            let mime = v["mimeType"].as_str()?;
            if !mime.starts_with("audio/") || crate::parse::is_encrypted_format(v) {
                return None;
            }
            Some(SabrFormat {
                itag: number(&v["itag"])?.try_into().ok()?,
                last_modified: number(&v["lastModified"])?,
                xtags: v["xtags"].as_str().map(str::to_owned),
                mime_type: mime.into(),
                duration_ms: number(&v["approxDurationMs"])?,
            })
        })
        .take(64)
        .collect::<Vec<_>>();
    formats.sort_by_key(|format| {
        std::cmp::Reverse(
            raw["streamingData"]["adaptiveFormats"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|v| v["itag"].as_u64() == Some(u64::from(format.itag)))
                })
                .and_then(|v| number(&v["bitrate"]))
                .unwrap_or(0),
        )
    });
    (!formats.is_empty()).then(|| SabrDescriptor {
        server_abr_streaming_url: url.into(),
        ustreamer_config: config.into(),
        formats,
    })
}

#[derive(Default)]
pub(crate) struct Sessions {
    next_id: u64,
    entries: BTreeMap<u64, Arc<Mutex<SabrSession>>>,
}
#[derive(Debug, Serialize)]
pub struct SabrOpened {
    pub handle: u64,
    pub video_id: String,
    pub itag: u32,
    pub mime_type: String,
    pub duration_ms: u64,
}
#[derive(Serialize)]
pub struct SabrRead {
    pub data_base64: String,
    pub is_init: bool,
    pub sequence: u32,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub finished: bool,
}
impl MusicClient {
    /// Open a per-client native SABR stream. Close it when the host changes track.
    pub fn sabr_open(&self, video_id: &str, format: AudioFormat) -> Result<SabrOpened> {
        crate::operation::ensure(|| {
            crate::client::validate_video_id(video_id)?;
            if crate::operation::lock(&self.sabr)?.entries.len() >= 4 {
                return Err(Error::InvalidInput(
                    "close an existing SABR session before opening another".into(),
                ));
            }
            let player = self.player(video_id)?;
            if player.status != "OK" {
                return Err(Error::Unplayable {
                    status: player.status,
                    reason: player.reason.unwrap_or_default(),
                });
            }
            let desc = player.sabr.ok_or_else(|| {
                Error::StreamUnavailable(
                    "Web player did not provide usable SABR audio delivery".into(),
                )
            })?;
            let candidates: Vec<_> = desc
                .formats
                .iter()
                .filter(|a| format.matches(&a.mime_type))
                .take(4)
                .cloned()
                .collect();
            let mut selected = None;
            let mut failure = Error::StreamUnavailable(
                "SABR has no verified clear audio format in the requested container".into(),
            );
            for audio in candidates {
                let mut session =
                    SabrSession::new(
                        self.http.clone(),
                        SabrConfig {
                            video_id: video_id.into(),
                            server_abr_streaming_url: desc.server_abr_streaming_url.clone(),
                            ustreamer_config: desc.ustreamer_config.clone(),
                            client_version: self.config.client_version.clone().ok_or_else(
                                || Error::Protocol("missing Web client version".into()),
                            )?,
                            format: audio.clone(),
                            po_token: self
                                .po_token_for(video_id, crate::attestation::PoTokenContext::Gvs)?,
                        },
                    )?;
                let prepared = match session.prepare() {
                    Err(Error::SabrReloadRequired) => self
                        .reload_sabr(&mut session)
                        .and_then(|()| session.prepare()),
                    value => value,
                };
                match prepared {
                    Ok(()) => {
                        selected = Some((audio, session));
                        break;
                    }
                    Err(
                        error @ (Error::SabrReloadRequired
                        | Error::StreamUnavailable(_)
                        | Error::Http(403 | 404 | 410)),
                    ) => failure = error,
                    Err(error) => return Err(error),
                }
            }
            let (audio, session) = selected.ok_or(failure)?;
            let result = SabrOpened {
                handle: 0,
                video_id: video_id.into(),
                itag: audio.itag,
                mime_type: audio.mime_type,
                duration_ms: audio.duration_ms,
            };
            let mut sessions = crate::operation::lock(&self.sabr)?;
            if sessions.entries.len() >= 4 {
                return Err(Error::InvalidInput("SABR session limit reached".into()));
            }
            sessions.next_id = sessions
                .next_id
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("SABR session ID exhausted".into()))?;
            let id = sessions.next_id;
            sessions.entries.insert(id, Arc::new(Mutex::new(session)));
            Ok(SabrOpened {
                handle: id,
                ..result
            })
        })
    }
    fn sabr_handle(&self, id: u64) -> Result<Arc<Mutex<SabrSession>>> {
        crate::operation::lock(&self.sabr)?
            .entries
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::InvalidInput("unknown SABR session handle".into()))
    }
    /// Read one complete bounded segment; binary media is base64 only at the JSON boundary.
    pub fn sabr_read(&self, id: u64) -> Result<crate::sabr::SabrChunk> {
        crate::operation::ensure(|| {
            let session = self.sabr_handle(id)?;
            let mut session = crate::operation::lock(&session)?;
            if let Some(token) =
                self.po_token_for(session.video_id(), crate::attestation::PoTokenContext::Gvs)?
            {
                session.set_po_token(&token)?;
            } else {
                session.clear_po_token();
            }
            match session.next_chunk() {
                Err(Error::SabrReloadRequired) => {
                    self.reload_sabr(&mut session)?;
                    session.next_chunk()
                }
                result => result,
            }
        })
    }
    fn reload_sabr(&self, session: &mut SabrSession) -> Result<()> {
        let token = session
            .take_reload_token()
            .ok_or(Error::SabrReloadRequired)?;
        let video_id = session.video_id().to_owned();
        let itag = session.format().itag;
        let player = self.player_with_reload(&video_id, &token)?;
        if player.status != "OK" {
            return Err(Error::Unplayable {
                status: player.status,
                reason: player.reason.unwrap_or_default(),
            });
        }
        let desc = player.sabr.ok_or(Error::SabrRequired)?;
        let format = desc
            .formats
            .into_iter()
            .find(|a| a.itag == itag)
            .ok_or_else(|| {
                Error::StreamUnavailable(
                    "SABR reload no longer offers the selected clear audio format".into(),
                )
            })?;
        session.apply_reload(SabrConfig {
            video_id: video_id.clone(),
            server_abr_streaming_url: desc.server_abr_streaming_url,
            ustreamer_config: desc.ustreamer_config,
            format,
            client_version: self
                .config
                .client_version
                .clone()
                .ok_or_else(|| Error::Protocol("missing Web client version".into()))?,
            po_token: self.po_token_for(&video_id, crate::attestation::PoTokenContext::Gvs)?,
        })?;
        Ok(())
    }
    pub fn sabr_seek(&self, id: u64, position_ms: u64) -> Result<()> {
        crate::operation::ensure(|| {
            let session = self.sabr_handle(id)?;
            let result = crate::operation::lock(&session)?.seek(position_ms);
            result
        })
    }
    pub fn sabr_close(&self, id: u64) -> Result<()> {
        crate::operation::ensure(|| {
            crate::operation::lock(&self.sabr)?
                .entries
                .remove(&id)
                .ok_or_else(|| Error::InvalidInput("unknown SABR session handle".into()))?;
            Ok(())
        })
    }
    pub(crate) fn sabr_read_json(&self, id: u64) -> Result<SabrRead> {
        let chunk = self.sabr_read(id)?;
        Ok(SabrRead {
            data_base64: base64::engine::general_purpose::STANDARD.encode(chunk.data),
            is_init: chunk.is_init,
            sequence: chunk.sequence,
            start_ms: chunk.start_ms,
            duration_ms: chunk.duration_ms,
            finished: chunk.finished,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn raw() -> Value {
        json!({"streamingData":{"serverAbrStreamingUrl":"https://r1.googlevideo.com/videoplayback?n=challenge",
          "adaptiveFormats":[{"itag":140,"lastModified":"123","mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","approxDurationMs":"10000"}]},
          "playerConfig":{"mediaCommonConfig":{"mediaUstreamerRequestConfig":{"videoPlaybackUstreamerConfig":"AQID"}}}})
    }
    #[test]
    fn sabr_never_exposes_unresolved_or_foreign_delivery() {
        let mut v = raw();
        assert!(parse_sabr(&v, false).is_none());
        assert_eq!(parse_sabr(&v, true).unwrap().formats.len(), 1);
        v["streamingData"]["serverAbrStreamingUrl"] = "https://evil.test/videoplayback".into();
        assert!(parse_sabr(&v, true).is_none());
    }
    #[test]
    fn encrypted_formats_are_never_native_sabr_candidates() {
        let mut v = raw();
        v["streamingData"]["adaptiveFormats"][0]["drmFamilies"] = json!(["WIDEVINE"]);
        assert!(parse_sabr(&v, true).is_none());
        v["streamingData"]["adaptiveFormats"][0]["drmFamilies"] = json!("malformed");
        assert!(parse_sabr(&v, true).is_none());
    }
}
