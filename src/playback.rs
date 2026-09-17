use crate::transport::Response;
use crate::{
    client::validate_video_id,
    model::{AudioFormat, AudioStream, Player, StreamVerification},
    parse, Error, MusicClient, Result,
};
use reqwest::Url;
use serde::Serialize;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::{
    collections::{BTreeSet, VecDeque},
    io::Read,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const WEB_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const PROBE_BYTES: u64 = 4096;

const STREAM_TTL: Duration = Duration::from_secs(5 * 60);
const EXPIRY_MARGIN_SECONDS: u64 = 90;
const MAX_STREAM_CACHE: usize = 8;
const MAX_PREFETCH: usize = 3;

struct CachedStream {
    video_id: String,
    format: u8,
    loaded_at: Instant,
    audio: AudioStream,
}

/// Per-client cache. Contains signed media URLs, so never serialize or log it.
#[derive(Default)]
pub(crate) struct PlaybackCache {
    generation: u64,
    streams: VecDeque<CachedStream>,
}

impl PlaybackCache {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.streams.clear();
    }

    pub(crate) fn invalidate_token_streams(&mut self, changed: &BTreeSet<String>) {
        if changed.is_empty() {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.streams
            .retain(|entry| !changed.contains(&entry.video_id));
        // The upstream public script cache is independent of video proofs.
    }

    fn stream(
        &mut self,
        video_id: &str,
        format: AudioFormat,
        now: Instant,
        unix: u64,
    ) -> Option<AudioStream> {
        self.streams.retain(|entry| {
            now.saturating_duration_since(entry.loaded_at) < STREAM_TTL
                && entry
                    .audio
                    .expires_at
                    .is_some_and(|expiry| expiry > unix.saturating_add(EXPIRY_MARGIN_SECONDS))
        });
        let index = self
            .streams
            .iter()
            .position(|entry| entry.video_id == video_id && entry.format == format_key(format))?;
        let entry = self.streams.remove(index)?;
        let audio = entry.audio.clone();
        self.streams.push_back(entry);
        Some(audio)
    }

    fn insert(&mut self, video_id: &str, format: AudioFormat, audio: AudioStream) {
        // Missing expiry is legal for a one-shot stream, never for cached reuse.
        if audio.expires_at.is_none() {
            return;
        }
        self.streams
            .retain(|entry| entry.video_id != video_id || entry.format != format_key(format));
        if self.streams.len() >= MAX_STREAM_CACHE {
            self.streams.pop_front();
        }
        self.streams.push_back(CachedStream {
            video_id: video_id.into(),
            format: format_key(format),
            loaded_at: Instant::now(),
            audio,
        });
    }
}

fn format_key(format: AudioFormat) -> u8 {
    match format {
        AudioFormat::Any => 0,
        AudioFormat::Mp4 => 1,
        AudioFormat::Webm => 2,
    }
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Error::MediaValidation("system clock before Unix epoch".into()))
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackWarmup {
    pub ready: bool,
    pub signature_timestamp: u32,
    pub generation: u64,
}

fn must_propagate(error: &Error) -> bool {
    matches!(
        error,
        Error::Cancelled
            | Error::Timeout
            | Error::RateLimited { .. }
            | Error::Network(_)
            | Error::Http(429 | 500..=599)
    )
}

fn replace_query(url: &mut Url, key: &str, value: &str) {
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair(key, value);
}

fn parse_web_player(raw: Value, video_id: &str) -> Result<Player> {
    crate::operation::check()?;
    let failed = raw.get("_rustypipeResolutionError").is_some();
    let mut player = if failed {
        parse::player(&raw)?
    } else {
        parse::resolved_web_player(&raw)?
    };
    if player
        .track
        .as_ref()
        .is_some_and(|track| track.video_id != video_id)
        || (raw["streamingData"].is_object() && player.track.is_none())
    {
        return Err(Error::Protocol(
            "player track does not match the requested video".into(),
        ));
    }
    if failed {
        player.resolution_error = Some("Web player challenge resolution failed".into());
    }
    let before = player.audio_streams.len();
    player
        .audio_streams
        .retain(|audio| media_url(&audio.url).is_ok());
    player.unresolved_audio_formats += before - player.audio_streams.len();
    player.source_client = Some("WEB_REMIX".into());
    for audio in &mut player.audio_streams {
        audio.source_client = player.source_client.clone();
        audio
            .http_headers
            .insert("User-Agent".into(), WEB_UA.into());
    }
    Ok(player)
}

impl MusicClient {
    fn playback_cache(&self) -> Result<std::sync::MutexGuard<'_, PlaybackCache>> {
        crate::operation::lock(&self.playback)
    }

    /// Fetch and actually prepare the current official player ahead of playback.
    /// Schedule this off the UI thread. No media URL or credentials are returned.
    pub fn prewarm(&self) -> Result<PlaybackWarmup> {
        crate::operation::ensure(|| self.prewarm_inner())
    }

    fn prewarm_inner(&self) -> Result<PlaybackWarmup> {
        let generation = self.playback_cache()?.generation;
        let signature_timestamp = self.upstream_prewarm_player()?;
        crate::operation::check()?;
        Ok(PlaybackWarmup {
            ready: true,
            signature_timestamp,
            generation,
        })
    }

    /// Forget script and media caches. In-flight older results cannot repopulate them.
    pub fn invalidate_playback(&self) -> Result<()> {
        // Clear transforms before advancing the media generation. A request
        // crossing this boundary retains the older generation and cannot fill
        // the new media cache. Never hold playback while query() takes session.
        self.upstream_invalidate_player()?;
        self.playback_cache()?.invalidate();
        Ok(())
    }

    fn invalidate_generation(&self, generation: u64) -> Result<()> {
        if self.playback_cache()?.generation != generation {
            return Ok(());
        }
        self.upstream_invalidate_player()?;
        let mut cache = self.playback_cache()?;
        if cache.generation == generation {
            cache.invalidate();
        }
        Ok(())
    }

    fn player_once(&self, video_id: &str) -> Result<(Player, u64)> {
        self.player_once_with_reload(video_id, None)
    }
    pub(crate) fn player_with_reload(&self, video_id: &str, reload_token: &str) -> Result<Player> {
        validate_video_id(video_id)?;
        if reload_token.is_empty() || reload_token.len() > 65536 {
            return Err(Error::Protocol("invalid SABR reload context".into()));
        }
        self.player_once_with_reload(video_id, Some(reload_token))
            .map(|(player, _)| player)
    }
    fn player_once_with_reload(
        &self,
        video_id: &str,
        reload_token: Option<&str>,
    ) -> Result<(Player, u64)> {
        crate::operation::check()?;
        let generation = self.playback_cache()?.generation;
        let raw = self.upstream_player_raw(video_id, reload_token)?;
        let mut player = parse_web_player(raw, video_id)?;
        if let Some((token, token_expiry)) =
            self.po_token_with_expiry(video_id, crate::attestation::PoTokenContext::Gvs)?
        {
            for audio in &mut player.audio_streams {
                let mut url = media_url(&audio.url)?;
                replace_query(&mut url, "pot", &token);
                audio.url = url.into();
                audio.expires_at = audio.expires_at.map(|expiry| expiry.min(token_expiry));
            }
        }
        Ok((player, generation))
    }

    /// Inspect the Web player and resolve signature/n challenges without CDN probing.
    /// Rebootstrap once on transform failure to recover from stale player scripts.
    pub fn player(&self, video_id: &str) -> Result<Player> {
        crate::operation::ensure(|| self.player_inner(video_id))
    }

    fn player_inner(&self, video_id: &str) -> Result<Player> {
        validate_video_id(video_id)?;
        let (player, generation) = self.player_once(video_id)?;
        if player.resolution_error.is_some() {
            self.invalidate_generation(generation)?;
            return self.player_once(video_id).map(|(player, _)| player);
        }
        Ok(player)
    }

    /// Return the highest-bitrate Web audio format that passes a bounded CDN GET.
    pub fn stream(&self, video_id: &str) -> Result<AudioStream> {
        self.stream_format(video_id, AudioFormat::Any)
    }

    /// Reuse a recently verified URL only while its expiry has a safe margin.
    pub fn stream_format(&self, video_id: &str, format: AudioFormat) -> Result<AudioStream> {
        self.stream_format_with_options(video_id, format, false)
    }

    /// Force a new player response after a host sees an expired/rejected media URL.
    pub fn stream_format_with_options(
        &self,
        video_id: &str,
        format: AudioFormat,
        force_refresh: bool,
    ) -> Result<AudioStream> {
        crate::operation::ensure(|| {
            self.stream_format_with_options_inner(video_id, format, force_refresh)
        })
    }

    fn stream_format_with_options_inner(
        &self,
        video_id: &str,
        format: AudioFormat,
        force_refresh: bool,
    ) -> Result<AudioStream> {
        validate_video_id(video_id)?;
        crate::operation::check()?;
        {
            let mut cache = self.playback_cache()?;
            if force_refresh {
                cache.streams.retain(|entry| entry.video_id != video_id);
            } else if let Some(audio) = cache.stream(video_id, format, Instant::now(), unix_now()?)
            {
                return Ok(audio);
            }
        }
        for attempt in 0..2 {
            let (player, generation) = self.player_once(video_id)?;
            crate::operation::phase("verifying_stream");
            match select_verified(player, format, |audio| self.probe_audio(audio)) {
                Ok(audio) => {
                    crate::operation::check()?;
                    let mut cache = self.playback_cache()?;
                    if cache.generation == generation {
                        cache.insert(video_id, format, audio.clone());
                    }
                    return Ok(audio);
                }
                Err(error)
                    if attempt == 0
                        && matches!(
                            error,
                            Error::StreamUnavailable(_)
                                | Error::StreamResolutionRequired
                                | Error::MediaValidation(_)
                                | Error::Http(403 | 410)
                        ) =>
                {
                    self.invalidate_generation(generation)?;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("second stream resolution always returns")
    }

    /// Resolve up to three upcoming tracks serially. The host owns queue scheduling.
    pub fn prefetch(&self, video_ids: &[String], format: AudioFormat) -> Result<Vec<AudioStream>> {
        crate::operation::ensure(|| self.prefetch_inner(video_ids, format))
    }

    fn prefetch_inner(
        &self,
        video_ids: &[String],
        format: AudioFormat,
    ) -> Result<Vec<AudioStream>> {
        if video_ids.is_empty() || video_ids.len() > MAX_PREFETCH {
            return Err(Error::InvalidInput(
                "prefetch requires between one and three video IDs".into(),
            ));
        }
        for id in video_ids {
            validate_video_id(id)?;
        }
        video_ids
            .iter()
            .map(|id| self.stream_format(id, format))
            .collect()
    }

    fn probe_audio(&self, audio: &mut AudioStream) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::MediaValidation("system clock before Unix epoch".into()))?
            .as_secs();
        if audio
            .expires_at
            .is_some_and(|expiry| expiry <= now.saturating_add(30))
        {
            return Err(Error::MediaValidation(
                "audio URL has expired or expires within 30 seconds".into(),
            ));
        }
        let mut url = media_url(&audio.url)?;
        for redirect in 0..=3 {
            let mut request = self.http.get(url.clone()).header("Range", "bytes=0-4095");
            for (key, value) in &audio.http_headers {
                request = request.header(key, value);
            }
            crate::operation::check()?;
            let response = self.send(request, PROBE_BYTES as usize, true, true)?;
            if response.status().is_redirection() {
                if redirect == 3 {
                    return Err(Error::MediaValidation("too many media redirects".into()));
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|h| h.to_str().ok())
                    .ok_or_else(|| {
                        Error::MediaValidation("media redirect has no valid location".into())
                    })?;
                let next = url
                    .join(location)
                    .map_err(|_| Error::MediaValidation("invalid media redirect".into()))?;
                url = media_url(next.as_str())?;
                continue;
            }
            audio.verification = Some(check_media_response(response, &audio.mime_type)?);
            // Retain the original signed URL and its expiry; the host may follow
            // the same validated redirect chain during actual playback.
            return Ok(());
        }
        unreachable!("redirect loop returns on its final iteration")
    }
}

fn select_verified(
    mut player: Player,
    format: AudioFormat,
    mut probe: impl FnMut(&mut AudioStream) -> Result<()>,
) -> Result<AudioStream> {
    if player.status != "OK" {
        return Err(Error::Unplayable {
            status: player.status,
            reason: player.reason.unwrap_or_default(),
        });
    }
    if player.audio_streams.is_empty() {
        if player.sabr.is_some() {
            return Err(Error::SabrRequired);
        }
        return Err(player
            .resolution_error
            .map(Error::StreamUnavailable)
            .unwrap_or(Error::StreamResolutionRequired));
    }
    player
        .audio_streams
        .retain(|audio| format.matches(&audio.mime_type));
    if player.audio_streams.is_empty() {
        return Err(Error::MediaValidation(
            "no audio format matches the requested container".into(),
        ));
    }
    player
        .audio_streams
        .sort_by_key(|audio| std::cmp::Reverse(audio.bitrate.unwrap_or(0)));
    let mut errors = Vec::new();
    // Bound CDN requests if a malformed upstream response contains many formats.
    for mut audio in player.audio_streams.into_iter().take(8) {
        crate::operation::check()?;
        match probe(&mut audio) {
            Ok(()) => return Ok(audio),
            Err(error) if must_propagate(&error) => return Err(error),
            Err(error) => errors.push(format!("itag {}: {error}", audio.itag)),
        }
    }
    Err(Error::MediaValidation(errors.join("; ")))
}

pub(crate) fn media_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| Error::MediaValidation("invalid media URL".into()))?;
    let allowed = url
        .host_str()
        .is_some_and(|host| host == "googlevideo.com" || host.ends_with(".googlevideo.com"));
    if url.scheme() != "https"
        || !allowed
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.path() != "/videoplayback"
    {
        return Err(Error::MediaValidation(
            "media URL must use HTTPS on the Google video CDN".into(),
        ));
    }
    Ok(url)
}

fn check_media_response(response: Response, mime: &str) -> Result<StreamVerification> {
    let status = response.status().as_u16();
    if status != 200 && status != 206 {
        return Err(crate::transport::status_error(&response));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bare_type = content_type.split(';').next().unwrap_or("").trim();
    let expected_type = mime.split(';').next().unwrap_or("").trim();
    if bare_type != expected_type && bare_type != "application/octet-stream" {
        return Err(Error::MediaValidation(
            "CDN returned an unexpected content type".into(),
        ));
    }
    let mut range_bytes = None;
    if status == 206 {
        let range = response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let range = range
            .strip_prefix("bytes 0-")
            .and_then(|v| v.split_once('/'));
        range_bytes = range.and_then(|(end, total)| {
            end.parse::<u64>()
                .ok()
                .zip(total.parse::<u64>().ok())
                .filter(|(end, total)| *end < PROBE_BYTES && total > end)
                .map(|(end, _)| end + 1)
        });
        if range_bytes.is_none() {
            return Err(Error::MediaValidation(
                "CDN returned an invalid initial byte range".into(),
            ));
        }
    }
    let mut data = Vec::new();
    response
        .take(PROBE_BYTES)
        .read_to_end(&mut data)
        .map_err(|_| Error::MediaValidation("could not read audio bytes".into()))?;
    if data.len() < 16 || range_bytes.is_some_and(|bytes| bytes != data.len() as u64) {
        return Err(Error::MediaValidation(
            "CDN returned incomplete audio bytes".into(),
        ));
    }
    let valid = match expected_type {
        "audio/webm" => data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "audio/mp4" => data.len() >= 12 && &data[4..8] == b"ftyp",
        _ => false,
    };
    if !valid {
        return Err(Error::MediaValidation(
            "CDN body does not match the audio container".into(),
        ));
    }
    Ok(StreamVerification {
        status,
        bytes_read: data.len(),
        content_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, thread};

    fn media_response(
        status: u16,
        content_type: &str,
        range: Option<&str>,
        body: Vec<u8>,
    ) -> Response {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let headers = format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
            body.len(), range.map(|v| format!("Content-Range: {v}\r\n")).unwrap_or_default());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            socket.write_all(headers.as_bytes()).unwrap();
            socket.write_all(&body).unwrap();
        });
        let request = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"));
        let response = crate::transport::send(request, PROBE_BYTES as usize, true, false).unwrap();
        server.join().unwrap();
        response
    }

    fn webm(size: usize) -> Vec<u8> {
        let mut bytes = vec![0; size];
        bytes[..4].copy_from_slice(&[0x1a, 0x45, 0xdf, 0xa3]);
        bytes
    }

    fn mp4() -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[4..8].copy_from_slice(b"ftyp");
        bytes
    }

    fn player() -> Player {
        parse::player(&json!({"playabilityStatus":{"status":"OK"},"streamingData":{"adaptiveFormats":[
            {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":160000,"url":"https://rr1.googlevideo.com/videoplayback?expire=4102444800"},
            {"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":128000,"url":"https://rr1.googlevideo.com/videoplayback?expire=4102444800"}
        ]}})).unwrap()
    }

    fn unsupported_player_response() -> Value {
        json!({"playabilityStatus":{"status":"OK"},
        "_rustypipeResolutionError":"fixture",
        "videoDetails":{"videoId":"4D7u5KF7SP8","title":"Metadata survives"},
        "streamingData":{"adaptiveFormats":[
            {"itag":140,"mimeType":"audio/mp4","url":"https://rr1.googlevideo.com/videoplayback?expire=4102444800"},
            {"itag":251,"mimeType":"audio/webm","url":"https://rr1.googlevideo.com/videoplayback?n=private-challenge"}
        ]}})
    }

    #[test]
    fn unresolved_web_challenge_preserves_metadata_and_safe_direct_formats() {
        let player = parse_web_player(unsupported_player_response(), "4D7u5KF7SP8").unwrap();
        assert_eq!(player.track.as_ref().unwrap().title, "Metadata survives");
        assert_eq!(player.audio_streams.len(), 1);
        assert_eq!(player.audio_streams[0].itag, 140);
        assert_eq!(player.unresolved_audio_formats, 1);
        assert!(player.resolution_error.is_some());
        assert!(!player
            .resolution_error
            .as_ref()
            .unwrap()
            .contains("private-challenge"));
        assert_eq!(
            select_verified(player, AudioFormat::Any, |_| Ok(()))
                .unwrap()
                .itag,
            140
        );
    }

    #[test]
    fn unresolved_web_only_response_has_metadata_but_cannot_be_selected() {
        let mut response = unsupported_player_response();
        response["streamingData"]["adaptiveFormats"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let player = parse_web_player(response, "4D7u5KF7SP8").unwrap();
        assert!(player.track.is_some());
        assert!(player.audio_streams.is_empty());
        assert_eq!(player.unresolved_audio_formats, 1);
        let error = select_verified(player, AudioFormat::Any, |_| {
            panic!("unresolved URLs must never be probed")
        })
        .unwrap_err();
        assert!(matches!(error, Error::StreamUnavailable(_)));
        assert!(!error.to_string().contains("private-challenge"));
    }

    #[test]
    fn failed_web_resolution_does_not_expose_untrusted_direct_urls() {
        let mut response = unsupported_player_response();
        response["streamingData"]["adaptiveFormats"][0]["url"] =
            "https://evil.test/videoplayback".into();
        let player = parse_web_player(response, "4D7u5KF7SP8").unwrap();
        assert!(player.audio_streams.is_empty());
        assert_eq!(player.unresolved_audio_formats, 2);
        assert!(player.track.is_some());
    }

    #[test]
    fn full_and_partial_audio_responses_are_verified() {
        let partial = check_media_response(
            media_response(206, "audio/webm", Some("bytes 0-31/10000"), webm(32)),
            "audio/webm; codecs=\"opus\"",
        )
        .unwrap();
        assert_eq!(partial.bytes_read, 32);
        assert_eq!(partial.status, 206);
        let full = check_media_response(
            media_response(200, "application/octet-stream", None, mp4()),
            "audio/mp4",
        )
        .unwrap();
        assert_eq!(full.bytes_read, 32);
    }

    #[test]
    fn range_ignored_response_is_bounded() {
        let result = check_media_response(
            media_response(200, "audio/webm", None, webm(8192)),
            "audio/webm",
        )
        .unwrap();
        assert_eq!(result.bytes_read, PROBE_BYTES as usize);
    }

    #[test]
    fn html_error_body_and_wrong_container_are_rejected() {
        assert!(check_media_response(
            media_response(200, "text/html", None, b"<html>blocked</html>".to_vec()),
            "audio/mp4"
        )
        .is_err());
        assert!(
            check_media_response(media_response(200, "audio/webm", None, mp4()), "audio/webm")
                .is_err()
        );
        assert!(matches!(
            check_media_response(media_response(403, "text/html", None, vec![]), "audio/webm"),
            Err(Error::Http(403))
        ));
    }

    #[test]
    fn incorrect_or_truncated_ranges_are_rejected() {
        for range in [
            "bytes 1-32/100",
            "bytes 0-4096/10000",
            "bytes 0-31/31",
            "bytes 0-31/*",
            "bytes 0-63/100",
        ] {
            assert!(
                check_media_response(
                    media_response(206, "audio/webm", Some(range), webm(32)),
                    "audio/webm"
                )
                .is_err(),
                "{range}"
            );
        }
        assert!(check_media_response(
            media_response(206, "audio/webm", None, webm(32)),
            "audio/webm"
        )
        .is_err());
    }

    #[test]
    fn failed_high_bitrate_falls_back_to_working_audio() {
        let mut attempts = Vec::new();
        let selected = select_verified(player(), AudioFormat::Any, |audio| {
            attempts.push(audio.itag);
            if audio.itag == 251 {
                Err(Error::Http(403))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(attempts, [251, 140]);
        assert_eq!(selected.itag, 140);
    }

    #[test]
    fn requested_container_never_silently_changes() {
        let selected = select_verified(player(), AudioFormat::Mp4, |audio| {
            assert_eq!(audio.itag, 140);
            Ok(())
        })
        .unwrap();
        assert_eq!(selected.itag, 140);
        let result = select_verified(player(), AudioFormat::Webm, |_| Err(Error::Http(403)));
        assert!(result.is_err());
    }

    #[test]
    fn total_probe_failure_contains_diagnostics_but_not_signed_urls() {
        let error =
            select_verified(player(), AudioFormat::Any, |_| Err(Error::Http(403))).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("itag 251"));
        assert!(message.contains("itag 140"));
        assert!(!message.contains("googlevideo"));
        assert!(!message.contains("expire="));
    }

    #[test]
    fn expired_urls_fail_before_network() {
        let client = MusicClient::new(crate::Config {
            client_version: Some("test".into()),
            ..Default::default()
        })
        .unwrap();
        let mut audio = player().audio_streams.remove(0);
        audio.expires_at = Some(1);
        assert!(matches!(
            client.probe_audio(&mut audio),
            Err(Error::MediaValidation(_))
        ));
    }

    #[test]
    fn stream_cache_enforces_expiry_ttl_container_and_bounds() {
        let mut cache = PlaybackCache::default();
        let now = Instant::now();
        let mut audio = player().audio_streams.remove(0);
        audio.expires_at = Some(1000);
        cache.insert("test", AudioFormat::Webm, audio.clone());
        assert!(cache.stream("test", AudioFormat::Mp4, now, 100).is_none());
        assert!(cache.stream("test", AudioFormat::Webm, now, 100).is_some());
        assert!(cache.stream("test", AudioFormat::Webm, now, 910).is_none());
        cache.insert("test", AudioFormat::Webm, audio.clone());
        assert!(cache
            .stream(
                "test",
                AudioFormat::Webm,
                now + STREAM_TTL + Duration::from_secs(1),
                100
            )
            .is_none());
        for id in 0..MAX_STREAM_CACHE + 2 {
            cache.insert(&id.to_string(), AudioFormat::Any, audio.clone());
        }
        assert_eq!(cache.streams.len(), MAX_STREAM_CACHE);
        assert!(cache.stream("0", AudioFormat::Any, now, 100).is_none());
        assert!(cache.stream("9", AudioFormat::Any, now, 100).is_some());
        audio.expires_at = None;
        cache.insert("unknown", AudioFormat::Any, audio);
        assert!(cache
            .stream("unknown", AudioFormat::Any, now, 100)
            .is_none());
    }

    #[test]
    fn proof_updates_preserve_public_code_and_only_invalidate_changed_videos() {
        let client = MusicClient::new(crate::Config {
            client_version: Some("offline.fixture".into()),
            visitor_data: Some("visitor-cache-fixture".into()),
            ..Default::default()
        })
        .unwrap();
        let bundle = |id: &str| crate::attestation::PoTokenBundle {
            video_id: id.into(),
            player_token: None,
            gvs_token: Some("g".repeat(100)),
            expires_at: unix_now().unwrap() + 300,
            session_binding: crate::attestation::visitor_binding("visitor-cache-fixture").unwrap(),
        };
        let first = bundle("QoXDQa9L12A");
        let second = bundle("dQw4w9WgXcQ");
        client.set_po_tokens(vec![first.clone()]).unwrap();
        let old_generation = {
            let mut cache = client.playback_cache().unwrap();
            cache.insert(
                &first.video_id,
                AudioFormat::Any,
                player().audio_streams.remove(0),
            );
            cache.insert(
                &second.video_id,
                AudioFormat::Any,
                player().audio_streams.remove(0),
            );
            cache.generation
        };
        client
            .set_po_tokens(vec![first.clone(), second.clone()])
            .unwrap();
        let new_generation = {
            let cache = client.playback_cache().unwrap();
            assert_eq!(cache.streams.len(), 1);
            assert_eq!(cache.streams[0].video_id, first.video_id);
            assert_ne!(cache.generation, old_generation);
            cache.generation
        };
        client
            .set_po_tokens(vec![second.clone(), first.clone()])
            .unwrap();
        client.invalidate_generation(old_generation).unwrap();
        assert_eq!(client.playback_cache().unwrap().generation, new_generation);
        let mut renewed = first.clone();
        renewed.expires_at += 60;
        client.set_po_tokens(vec![renewed, second]).unwrap();
        assert!(client.playback_cache().unwrap().streams.is_empty());
        {
            let mut cache = client.playback_cache().unwrap();
            cache.insert(
                &first.video_id,
                AudioFormat::Any,
                player().audio_streams.remove(0),
            );
        }
        client.set_po_tokens(vec![]).unwrap();
        let cache = client.playback_cache().unwrap();
        assert!(cache.streams.is_empty());
    }

    #[test]
    fn invalidation_removes_signed_urls_and_advances_generation() {
        let mut cache = PlaybackCache::default();
        cache.insert("test", AudioFormat::Any, player().audio_streams.remove(0));
        cache.invalidate();
        assert_eq!(cache.generation, 1);
        assert!(cache.streams.is_empty());
    }

    #[test]
    fn cancellation_and_rate_limits_never_try_another_format() {
        for kind in 0..3 {
            let mut calls = 0;
            let error = select_verified(player(), AudioFormat::Any, |_| {
                calls += 1;
                Err(match kind {
                    0 => Error::Cancelled,
                    1 => Error::Timeout,
                    _ => Error::RateLimited {
                        retry_after_seconds: Some(60),
                    },
                })
            })
            .unwrap_err();
            assert!(must_propagate(&error));
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn cancelled_resolution_does_not_become_metadata_success() {
        let context = crate::operation::OperationContext::new(Default::default()).unwrap();
        let result = context.run(|| {
            context.cancel();
            parse_web_player(unsupported_player_response(), "4D7u5KF7SP8")
        });
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn prefetch_validates_entire_batch_before_network() {
        let client = MusicClient::new(crate::Config {
            client_version: Some("test".into()),
            ..Default::default()
        })
        .unwrap();
        for ids in [
            vec![],
            vec!["4D7u5KF7SP8".into(); 4],
            vec!["4D7u5KF7SP8".into(), "invalid!".into()],
        ] {
            assert!(matches!(
                client.prefetch(&ids, AudioFormat::Any),
                Err(Error::InvalidInput(_))
            ));
        }
    }
    #[test]
    fn untrusted_cdn_urls_and_redirect_targets_are_rejected() {
        assert!(media_url("https://rr1.googlevideo.com/videoplayback?id=test").is_ok());
        for url in [
            "http://rr1.googlevideo.com/videoplayback",
            "https://googlevideo.com.evil.test/videoplayback",
            "https://notgooglevideo.com/videoplayback",
            "https://127.0.0.1/videoplayback",
            "https://user:pass@rr1.googlevideo.com/videoplayback",
            "https://rr1.googlevideo.com:8443/videoplayback",
            "https://rr1.googlevideo.com/other",
        ] {
            assert!(media_url(url).is_err(), "{url}");
        }
    }
}
