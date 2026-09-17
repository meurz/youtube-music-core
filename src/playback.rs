use crate::{
    client::{checked_response, header, network, validate_video_id},
    model::{AudioFormat, AudioStream, PlaybackClient, Player, StreamVerification},
    parse, Error, MusicClient, Result,
};
use reqwest::{blocking::Response, header::HeaderMap, Url};
use serde_json::{json, Value};
use std::{
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};

const VR_VERSION: &str = "1.65.10";
const VR_UA: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const WEB_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const PROBE_BYTES: u64 = 4096;

fn profiles(mode: PlaybackClient) -> &'static [PlaybackClient] {
    match mode {
        PlaybackClient::Auto => &[PlaybackClient::AndroidVr, PlaybackClient::WebRemix],
        PlaybackClient::AndroidVr => &[PlaybackClient::AndroidVr],
        PlaybackClient::AndroidMusic => &[PlaybackClient::AndroidMusic],
        PlaybackClient::WebRemix => &[PlaybackClient::WebRemix],
    }
}

fn profile_name(profile: PlaybackClient) -> &'static str {
    match profile {
        PlaybackClient::AndroidVr => "ANDROID_VR",
        PlaybackClient::AndroidMusic => "ANDROID_MUSIC",
        PlaybackClient::WebRemix => "WEB_REMIX",
        PlaybackClient::Auto => "AUTO",
    }
}

fn vr_body(video_id: &str, config: &crate::Config) -> Value {
    let mut client = json!({
        "clientName":"ANDROID_VR", "clientVersion":VR_VERSION,
        "deviceMake":"Oculus", "deviceModel":"Quest 3", "androidSdkVersion":32,
        "userAgent":VR_UA, "osName":"Android", "osVersion":"12L",
        "hl":config.language, "gl":config.country,
    });
    if let Some(visitor) = &config.visitor_data {
        client["visitorData"] = visitor.clone().into();
    }
    json!({"context":{"client":client}, "videoId":video_id, "contentCheckOk":true, "racyCheckOk":true})
}

fn vr_headers(config: &crate::Config) -> Result<HeaderMap> {
    // This profile is anonymous even when catalog requests use account cookies.
    let mut headers = HeaderMap::new();
    header(&mut headers, "user-agent", VR_UA)?;
    header(&mut headers, "x-youtube-client-name", "28")?;
    header(&mut headers, "x-youtube-client-version", VR_VERSION)?;
    if let Some(visitor) = &config.visitor_data {
        header(&mut headers, "x-goog-visitor-id", visitor)?;
    }
    Ok(headers)
}

fn timestamp(html: &str) -> Option<u64> {
    let rest = html.split_once("\"STS\":")?.1.trim_start();
    serde_json::Deserializer::from_str(rest)
        .into_iter::<u64>()
        .next()?
        .ok()
        .filter(|n| *n > 0)
}

impl MusicClient {
    fn player_profiles(&self) -> &'static [PlaybackClient] {
        if self.is_android_music() {
            &[PlaybackClient::AndroidMusic]
        } else {
            profiles(self.config.playback_client)
        }
    }
    fn web_timestamp(&self, video_id: &str) -> Result<u64> {
        if let Some(timestamp) = self.signature_timestamp.get() {
            return Ok(*timestamp);
        }
        // Fixed YouTube host and validated ID; no account headers on this bootstrap.
        let html = checked_response(
            self.http
                .get(format!("https://www.youtube.com/watch?v={video_id}"))
                .send()
                .map_err(network)?,
        )?;
        let timestamp = timestamp(&html).ok_or_else(|| {
            Error::Protocol("watch page is missing the current signature timestamp".into())
        })?;
        let _ = self.signature_timestamp.set(timestamp);
        Ok(timestamp)
    }

    fn profile_player(&self, video_id: &str, profile: PlaybackClient) -> Result<Player> {
        let raw = match profile {
            PlaybackClient::AndroidMusic => self.android_post(
                "player",
                json!({"videoId":video_id,"contentCheckOk":true,"racyCheckOk":true}),
                crate::android::AUDIO_VERSION,
                34,
            )?,
            PlaybackClient::AndroidVr => {
                let response = self
                    .http
                    .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
                    .headers(vr_headers(&self.config)?)
                    .json(&vr_body(video_id, &self.config))
                    .send()
                    .map_err(network)?;
                serde_json::from_str(&checked_response(response)?)
                    .map_err(|_| Error::Protocol("player returned invalid JSON".into()))?
            }
            PlaybackClient::WebRemix => {
                let mut body = json!({"videoId":video_id, "contentCheckOk":true, "racyCheckOk":true,
                    "playbackContext":{"contentPlaybackContext":{"signatureTimestamp":self.web_timestamp(video_id)?, "html5Preference":"HTML5_PREF_WANTS"}}});
                if let Some(token) = &self.config.po_token {
                    body["serviceIntegrityDimensions"] = json!({"poToken":token});
                }
                self.post("player", body)?
            }
            PlaybackClient::Auto => unreachable!("auto expands to concrete profiles"),
        };
        let mut player = parse::player(&raw)?;
        if player
            .track
            .as_ref()
            .is_some_and(|t| t.video_id != video_id)
            || (!player.audio_streams.is_empty() && player.track.is_none())
        {
            return Err(Error::Protocol(
                "player track does not match the requested video".into(),
            ));
        }
        player.source_client = Some(profile_name(profile).into());
        let ua = match profile {
            PlaybackClient::AndroidVr => VR_UA,
            PlaybackClient::AndroidMusic => crate::android::AUDIO_UA,
            _ => WEB_UA,
        };
        for audio in &mut player.audio_streams {
            audio.source_client = player.source_client.clone();
            audio.http_headers.insert("User-Agent".into(), ua.into());
        }
        Ok(player)
    }

    /// Inspect the first usable player profile, preserving unavailable metadata.
    /// URLs here have not been probed; use stream() for a verified media URL.
    pub fn player(&self, video_id: &str) -> Result<Player> {
        validate_video_id(video_id)?;
        let mut fallback: Option<Player> = None;
        let mut errors = Vec::new();
        for &profile in self.player_profiles() {
            match self.profile_player(video_id, profile) {
                Ok(player) => {
                    if player.status == "OK" && !player.audio_streams.is_empty() {
                        return Ok(player);
                    }
                    if fallback.is_none() || player.status == "OK" {
                        fallback = Some(player);
                    }
                }
                Err(error) => errors.push(format!("{}: {error}", profile_name(profile))),
            }
        }
        fallback.ok_or_else(|| Error::StreamUnavailable(errors.join("; ")))
    }

    /// Return the highest-bitrate audio format that passes a bounded CDN GET.
    /// Failed formats are skipped, then the next configured client is attempted.
    pub fn stream(&self, video_id: &str) -> Result<AudioStream> {
        self.stream_format(video_id, AudioFormat::Any)
    }

    /// Resolve and validate audio in a host-compatible container.
    pub fn stream_format(&self, video_id: &str, format: AudioFormat) -> Result<AudioStream> {
        validate_video_id(video_id)?;
        let mut errors = Vec::new();
        for &profile in self.player_profiles() {
            let result = self.profile_player(video_id, profile).and_then(|player| {
                select_verified(player, format, |audio| self.probe_audio(audio))
            });
            match result {
                Ok(audio) => return Ok(audio),
                Err(error) => errors.push(format!("{}: {error}", profile_name(profile))),
            }
        }
        Err(Error::StreamUnavailable(errors.join("; ")))
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
            let response = request.send().map_err(network)?;
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
        return Err(Error::StreamResolutionRequired);
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
        match probe(&mut audio) {
            Ok(()) => return Ok(audio),
            Err(error) => errors.push(format!("itag {}: {error}", audio.itag)),
        }
    }
    Err(Error::MediaValidation(errors.join("; ")))
}

fn media_url(raw: &str) -> Result<Url> {
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
        return Err(Error::Http(status));
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
        let response = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .unwrap();
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

    #[test]
    fn vr_profile_is_anonymous_and_has_matching_identity() {
        let config = crate::Config {
            cookie: Some("SAPISID=private".into()),
            po_token: Some("private".into()),
            ..Default::default()
        };
        let headers = vr_headers(&config).unwrap();
        let body = vr_body("4D7u5KF7SP8", &config);
        assert!(!headers.contains_key("cookie"));
        assert!(!headers.contains_key("authorization"));
        assert!(!body.to_string().contains("private"));
        assert_eq!(headers["x-youtube-client-name"], "28");
        assert_eq!(
            headers["user-agent"],
            body["context"]["client"]["userAgent"].as_str().unwrap()
        );
        assert_eq!(
            headers["x-youtube-client-version"],
            body["context"]["client"]["clientVersion"].as_str().unwrap()
        );
        assert_eq!(body["context"]["client"]["osVersion"], "12L");
        assert!(body.get("playbackContext").is_none());
        let client = MusicClient::new(crate::Config {
            client_version: Some("test".into()),
            ..config
        })
        .unwrap();
        let media_request = client
            .http
            .get("https://rr1.googlevideo.com/videoplayback")
            .build()
            .unwrap();
        assert!(!media_request.headers().contains_key("cookie"));
        assert!(!media_request.headers().contains_key("authorization"));
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

    #[test]
    fn player_timestamp_is_discovered_without_javascript_execution() {
        assert_eq!(
            timestamp(r#"ytcfg.set({"STS":20702,"next":true});"#),
            Some(20702)
        );
        assert_eq!(timestamp(r#"{"STS":0}"#), None);
        assert_eq!(timestamp("consent page"), None);
    }
}
