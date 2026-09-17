use crate::{model::*, parse, Error, Result};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) const ORIGIN: &str = "https://music.youtube.com";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;

/// Credentials are deliberately not Debug. Never persist this struct in logs.
#[derive(Clone, Serialize)]
pub struct Config {
    pub language: String,
    pub country: String,
    pub client_version: Option<String>,
    pub visitor_data: Option<String>,
    pub cookie: Option<String>,
    pub cookie_expirations: BTreeMap<String, i64>,
    pub po_token: Option<String>,
    pub po_tokens: Vec<crate::attestation::PoTokenBundle>,
    pub proxy: Option<String>,
    pub timeout_seconds: u64,
    pub auth_user: u32,
    pub delegated_session_id: Option<String>,
    pub playback_client: PlaybackClient,
}

#[derive(Deserialize)]
#[serde(remote = "Config", default, deny_unknown_fields)]
struct ConfigFields {
    pub language: String,
    pub country: String,
    pub client_version: Option<String>,
    pub visitor_data: Option<String>,
    pub cookie: Option<String>,
    pub cookie_expirations: BTreeMap<String, i64>,
    pub po_token: Option<String>,
    pub po_tokens: Vec<crate::attestation::PoTokenBundle>,
    pub proxy: Option<String>,
    pub timeout_seconds: u64,
    pub auth_user: u32,
    pub delegated_session_id: Option<String>,
    pub playback_client: PlaybackClient,
}

impl Default for ConfigFields {
    fn default() -> Self {
        let config = Config::default();
        Self {
            language: config.language,
            country: config.country,
            client_version: config.client_version,
            visitor_data: config.visitor_data,
            cookie: config.cookie,
            cookie_expirations: config.cookie_expirations,
            po_token: config.po_token,
            po_tokens: config.po_tokens,
            proxy: config.proxy,
            timeout_seconds: config.timeout_seconds,
            auth_user: config.auth_user,
            delegated_session_id: config.delegated_session_id,
            playback_client: config.playback_client,
        }
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        if let Some(fields) = value.as_object_mut() {
            for key in ["oauth", "music_oauth"] {
                if fields
                    .remove(key)
                    .is_some_and(|credential| !credential.is_null())
                {
                    return Err(serde::de::Error::custom(crate::auth::LEGACY_AUTH_MESSAGE));
                }
            }
        }
        ConfigFields::deserialize(value).map_err(serde::de::Error::custom)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "en".into(),
            country: "US".into(),
            client_version: None,
            visitor_data: None,
            cookie: None,
            cookie_expirations: BTreeMap::new(),
            po_token: None,
            po_tokens: Vec::new(),
            proxy: None,
            timeout_seconds: 30,
            auth_user: 0,
            delegated_session_id: None,
            playback_client: PlaybackClient::Auto,
        }
    }
}

pub struct MusicClient {
    pub(crate) http: Client,
    pub(crate) config: Config,
    pub(crate) playback: Mutex<crate::playback::PlaybackCache>,
    pub(crate) sabr: Mutex<crate::delivery::Sessions>,
    pub(crate) po_tokens: Mutex<Vec<crate::attestation::PoTokenBundle>>,
    pub(crate) session: Mutex<crate::session::CookieState>,
}

pub(crate) fn network(e: reqwest::Error) -> Error {
    // Strip URLs, which can include continuation tokens or proxy credentials.
    crate::transport::network(e)
}

pub(crate) fn header(headers: &mut HeaderMap, key: &'static str, value: &str) -> Result<()> {
    let mut value = HeaderValue::from_str(value)
        .map_err(|_| Error::InvalidInput(format!("invalid {key} header")))?;
    if key == "cookie" || key == "authorization" || key == "x-goog-visitor-id" {
        value.set_sensitive(true);
    }
    headers.insert(key, value);
    Ok(())
}

pub(crate) fn cookie_hash(cookie: &str, timestamp: u64) -> Option<String> {
    let pairs: Vec<_> = cookie
        .split(';')
        .filter_map(|s| s.trim().split_once('='))
        .collect();
    // A request may legitimately contain duplicate partitioned preferences,
    // but conflicting signing cookies must never select an arbitrary identity.
    for name in ["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"] {
        let mut values = pairs
            .iter()
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| value);
        if let Some(first) = values.next() {
            if values.any(|value| value != first) {
                return None;
            }
        }
    }
    let value = ["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"]
        .iter()
        .find_map(|name| {
            pairs
                .iter()
                .find(|(k, v)| k == name && !v.is_empty())
                .map(|(_, v)| *v)
        })?;
    let digest = Sha1::digest(format!("{timestamp} {value} {ORIGIN}"));
    Some(format!("SAPISIDHASH {timestamp}_{digest:x}"))
}

pub(crate) fn config_string(html: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let rest = html.split_once(&needle)?.1.trim_start();
    serde_json::Deserializer::from_str(rest)
        .into_iter::<String>()
        .next()?
        .ok()
}

pub(crate) fn checked_response(response: crate::transport::Response) -> Result<String> {
    let status = response.status();
    if !status.is_success() {
        return Err(crate::transport::status_error(&response));
    }
    let mut body = String::new();
    response
        .take(MAX_RESPONSE + 1)
        .read_to_string(&mut body)
        .map_err(|_| Error::Protocol("response is not readable UTF-8".into()))?;
    if body.len() as u64 > MAX_RESPONSE {
        return Err(Error::Protocol("response exceeds 16 MiB".into()));
    }
    Ok(body)
}

impl MusicClient {
    /// Blocking client. Async hosts should call it on a dedicated blocking worker.
    /// Unless client_version is supplied, bootstraps the current web client first.
    pub fn new(config: Config) -> Result<Self> {
        crate::operation::ensure(|| Self::new_inner(config))
    }

    fn new_inner(mut config: Config) -> Result<Self> {
        if config.auth_user > 99 {
            return Err(Error::InvalidInput(
                "auth_user must be between 0 and 99".into(),
            ));
        }
        if let Some(id) = &config.delegated_session_id {
            if id.is_empty()
                || id.len() > 256
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(Error::InvalidInput("invalid delegated session ID".into()));
            }
        }
        if config.timeout_seconds == 0 || config.timeout_seconds > 300 {
            return Err(Error::InvalidInput(
                "timeout_seconds must be between 1 and 300".into(),
            ));
        }
        let mut builder = Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = &config.proxy {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy)
                    .map_err(|_| Error::InvalidInput("invalid proxy URL".into()))?,
            );
        }
        let http = builder.build().map_err(network)?;
        let session = crate::session::CookieState::new(
            config.cookie.take(),
            std::mem::take(&mut config.cookie_expirations),
        )?;
        let po_tokens = std::mem::take(&mut config.po_tokens);
        let mut client = Self {
            http,
            config,
            session: Mutex::new(session),
            playback: Mutex::default(),
            sabr: Mutex::default(),
            po_tokens: Mutex::default(),
        };
        // Preconfigured anonymous bundles must supply their exact visitor_data.
        // Never bootstrap a different visitor and attach an existing proof to it.
        client.set_po_tokens(po_tokens)?;
        if client.config.client_version.is_none() {
            let html = client.music_page("/")?;
            client.config.client_version = Some(config_string(&html, "INNERTUBE_CLIENT_VERSION").ok_or_else(|| Error::Protocol("web client bootstrap failed; provide client_version if consent or regional restrictions block the homepage".into()))?);
            if client.config.visitor_data.is_none() {
                client.config.visitor_data = config_string(&html, "VISITOR_DATA");
            }
        }
        Ok(client)
    }

    pub(crate) fn lock_session(&self) -> Result<MutexGuard<'_, crate::session::CookieState>> {
        let mut state = crate::operation::lock(&self.session)?;
        state.purge(crate::session::now()?);
        Ok(state)
    }

    /// Session-bearing HTML requests stay on the exact Music origin. Script and
    /// media downloads continue using the credential-free HTTP client directly.
    pub(crate) fn music_page(&self, path: &str) -> Result<String> {
        if !(path == "/" || path.starts_with("/watch?v=")) {
            return Err(Error::InvalidInput("unsupported Music page".into()));
        }
        let url = reqwest::Url::parse(&format!("{ORIGIN}{path}"))
            .map_err(|_| Error::InvalidInput("invalid Music page".into()))?;
        let mut state = self.lock_session()?;
        let mut headers = HeaderMap::new();
        if let Some(cookie) = &state.cookie {
            header(&mut headers, "cookie", cookie)?;
        }
        let response = self.send(
            self.http.get(url.clone()).headers(headers),
            MAX_RESPONSE as usize,
            false,
            true,
        )?;
        if matches!(response.status().as_u16(), 401 | 403) {
            state.verified = false;
        }
        let headers = response.headers().clone();
        let body = checked_response(response)?;
        state.observe(&url, &headers, crate::session::now()?)?;
        Ok(body)
    }

    #[cfg(test)]
    fn web_request(&self, endpoint: &str, body: Value) -> Result<reqwest::RequestBuilder> {
        let state = self.lock_session()?;
        self.web_request_with_cookie(endpoint, body, state.cookie.as_deref())
    }

    fn web_request_with_cookie(
        &self,
        endpoint: &str,
        mut body: Value,
        cookie: Option<&str>,
    ) -> Result<reqwest::RequestBuilder> {
        let mut client = json!({"clientName":"WEB_REMIX", "clientVersion":self.config.client_version, "hl":self.config.language, "gl":self.config.country});
        let mut headers = HeaderMap::new();
        header(&mut headers, "origin", ORIGIN)?;
        header(&mut headers, "referer", &format!("{ORIGIN}/"))?;
        header(&mut headers, "x-youtube-client-name", "67")?;
        header(
            &mut headers,
            "x-youtube-client-version",
            self.config.client_version.as_deref().unwrap_or_default(),
        )?;
        if let Some(visitor) = &self.config.visitor_data {
            client["visitorData"] = visitor.clone().into();
            header(&mut headers, "x-goog-visitor-id", visitor)?;
        }
        if let Some(cookie) = cookie {
            header(&mut headers, "cookie", cookie)?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::Protocol("system clock before Unix epoch".into()))?
                .as_secs();
            if let Some(auth) = cookie_hash(cookie, now) {
                header(&mut headers, "authorization", &auth)?;
                header(
                    &mut headers,
                    "x-goog-authuser",
                    &self.config.auth_user.to_string(),
                )?;
                header(&mut headers, "x-origin", ORIGIN)?;
                if let Some(id) = &self.config.delegated_session_id {
                    header(&mut headers, "x-goog-pageid", id)?;
                }
            }
        }
        body["context"] = json!({"client":client, "user":{"lockedSafetyMode":false}});
        if let Some(id) = &self.config.delegated_session_id {
            body["context"]["user"]["onBehalfOfUser"] = id.clone().into();
        }
        Ok(self
            .http
            .post(format!("{ORIGIN}/youtubei/v1/{endpoint}?prettyPrint=false"))
            .headers(headers)
            .json(&body))
    }

    pub(crate) fn send(
        &self,
        request: reqwest::RequestBuilder,
        limit: usize,
        truncate: bool,
        retry: bool,
    ) -> Result<crate::transport::Response> {
        crate::transport::send(request, limit, truncate, retry)
    }

    pub(crate) fn post_write(&self, endpoint: &str, body: Value) -> Result<Value> {
        crate::operation::check()?;
        self.post_validated_policy(endpoint, body, false, false, Ok)
            .map(|(value, _)| value)
    }

    pub(crate) fn post(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.post_validated(endpoint, body, false, Ok)
            .map(|(value, _)| value)
    }

    // Hold the session lock through request signing and response application so
    // older concurrent responses cannot replace newer server-issued cookies.
    pub(crate) fn post_validated<T>(
        &self,
        endpoint: &str,
        body: Value,
        verify_session: bool,
        parse: impl FnOnce(Value) -> Result<T>,
    ) -> Result<(T, Option<crate::auth::BrowserSession>)> {
        self.post_validated_policy(endpoint, body, verify_session, true, parse)
    }

    fn post_validated_policy<T>(
        &self,
        endpoint: &str,
        body: Value,
        verify_session: bool,
        retry: bool,
        parse: impl FnOnce(Value) -> Result<T>,
    ) -> Result<(T, Option<crate::auth::BrowserSession>)> {
        let mut state = self.lock_session()?;
        let request = self.web_request_with_cookie(endpoint, body, state.cookie.as_deref())?;
        crate::operation::check()?;
        let result = (|| {
            let response = self.send(request, MAX_RESPONSE as usize, false, retry)?;
            if matches!(response.status().as_u16(), 401 | 403) {
                state.verified = false;
            }
            let url = response.url().clone();
            let headers = response.headers().clone();
            let value: Value = serde_json::from_str(&checked_response(response)?)
                .map_err(|_| Error::Protocol("API returned invalid JSON".into()))?;
            if state.cookie.is_some() && crate::auth::explicitly_signed_out(&value) {
                state.verified = false;
                return Err(Error::AuthenticationRejected);
            }
            if let Some(error) = value.get("error") {
                return Err(Error::Protocol(match error["code"].as_u64() {
                    Some(code) => format!("API error {code}"),
                    None => "API returned an error".into(),
                }));
            }
            let parsed = parse(value).inspect_err(|_| {
                if verify_session {
                    state.verified = false;
                }
            })?;
            state.observe(&url, &headers, crate::session::now()?)?;
            let snapshot = if verify_session {
                let snapshot = state
                    .snapshot(&self.config)
                    .map_err(|_| Error::AuthenticationRejected)?;
                state.verified = true;
                snapshot
            } else {
                None
            };
            Ok((parsed, snapshot))
        })();
        if retry {
            result
        } else {
            result.map_err(|error| match error {
                Error::Network(_)
                | Error::Timeout
                | Error::Cancelled
                | Error::Protocol(_)
                | Error::Http(500..=599) => Error::MutationUncertain(Box::new(error)),
                other => other,
            })
        }
    }

    pub fn search(&self, query: &str, filter: SearchFilter) -> Result<Page> {
        crate::operation::ensure(|| {
            nonempty(query, "query")?;
            let mut body = json!({"query":query});
            if let Some(params) = filter.params() {
                body["params"] = params.into();
            }
            let value = self.post("search", body)?;
            parse::page(&value)
        })
    }

    /// A browse ID can identify an album, artist, playlist, or home feed.
    pub fn browse(&self, browse_id: &str) -> Result<Page> {
        crate::operation::ensure(|| {
            nonempty(browse_id, "browse_id")?;
            parse::page(&self.post("browse", json!({"browseId":browse_id}))?)
        })
    }

    pub fn playlist(&self, playlist_id: &str) -> Result<Page> {
        crate::operation::ensure(|| {
            nonempty(playlist_id, "playlist_id")?;
            let id = if playlist_id.starts_with("VL") {
                playlist_id.into()
            } else {
                format!("VL{playlist_id}")
            };
            self.browse(&id)
        })
    }

    pub fn continue_page(&self, endpoint: ContinuationEndpoint, token: &str) -> Result<Page> {
        crate::operation::ensure(|| {
            nonempty(token, "continuation")?;
            parse::page(&self.post(endpoint.as_str(), json!({"continuation":token}))?)
        })
    }

    fn next_raw(&self, video_id: &str) -> Result<Value> {
        validate_video_id(video_id)?;
        self.post(
            "next",
            json!({"videoId":video_id, "enablePersistentPlaylistPanel":true, "isAudioOnly":true}),
        )
    }

    pub fn queue(&self, video_id: &str) -> Result<Page> {
        crate::operation::ensure(|| {
            let next = self.next_raw(video_id)?;
            if let Some(endpoint) = parse::find(&next, "automixPreviewVideoRenderer")
                .and_then(|v| parse::find(v, "watchPlaylistEndpoint"))
            {
                if let Some(playlist_id) = endpoint["playlistId"].as_str() {
                    let mut body = json!({"videoId":video_id, "playlistId":playlist_id, "isAudioOnly":true, "enablePersistentPlaylistPanel":true});
                    if let Some(params) = endpoint.get("params") {
                        body["params"] = params.clone();
                    }
                    return parse::page(&self.post("next", body)?);
                }
            }
            parse::page(&next)
        })
    }

    pub fn song(&self, video_id: &str) -> Result<Track> {
        crate::operation::ensure(|| {
            let p = self.player(video_id)?;
            p.track.ok_or_else(|| Error::Unplayable {
                status: p.status,
                reason: p.reason.unwrap_or_else(|| "no track metadata".into()),
            })
        })
    }

    pub fn lyrics(&self, video_id: &str) -> Result<Lyrics> {
        crate::operation::ensure(|| {
            let next = self.next_raw(video_id)?;
            let tabs = parse::find(&next, "watchNextTabbedResultsRenderer")
                .and_then(|v| v["tabs"].as_array())
                .ok_or(Error::LyricsUnavailable)?;
            let id = tabs
                .iter()
                .filter(|v| v["tabRenderer"]["unselectable"] != true)
                .find_map(|v| {
                    let ep = &v["tabRenderer"]["endpoint"]["browseEndpoint"];
                    let id = ep["browseId"].as_str()?;
                    (id.starts_with("MPLY")
                        || parse::find(ep, "pageType").and_then(Value::as_str)
                            == Some("MUSIC_PAGE_TYPE_TRACK_LYRICS"))
                    .then_some(id)
                })
                .ok_or(Error::LyricsUnavailable)?;
            parse::lyrics(&self.post("browse", json!({"browseId":id}))?, id)
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationEndpoint {
    Search,
    Browse,
    Next,
}
impl ContinuationEndpoint {
    fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Browse => "browse",
            Self::Next => "next",
        }
    }
}

pub(crate) fn nonempty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 65536 {
        return Err(Error::InvalidInput(format!(
            "{name} must be nonempty and at most 65536 bytes"
        )));
    }
    Ok(())
}

pub(crate) fn validate_video_id(id: &str) -> Result<()> {
    if id.len() != 11
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::InvalidInput(
            "video_id must be an 11-character YouTube ID".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_cancelled_while_waiting_for_session_was_not_dispatched() {
        let client = std::sync::Arc::new(
            MusicClient::new(Config {
                client_version: Some("fixture".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        let held = client.session.lock().unwrap();
        let other = client.clone();
        let context =
            crate::operation::OperationContext::new(crate::operation::OperationOptions::default())
                .unwrap();
        let worker_context = context.clone();
        let worker = std::thread::spawn(move || {
            worker_context.run(|| other.post_write("playlist/create", json!({})))
        });
        std::thread::sleep(Duration::from_millis(20));
        context.cancel();
        assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
        drop(held);
    }

    #[test]
    fn legacy_oauth_config_is_rejected_without_exposing_credentials() {
        for key in ["oauth", "music_oauth"] {
            let input =
                json!({key:{"access_token":"private-access","refresh_token":"private-refresh"}});
            let error = Config::deserialize(input).err().unwrap().to_string();
            assert!(error.contains(crate::auth::LEGACY_AUTH_MESSAGE));
            assert!(!error.contains("private"));
        }
        // Old browser configs serialized unused OAuth slots as null.
        let config: Config =
            serde_json::from_value(json!({"oauth":null,"music_oauth":null})).unwrap();
        assert!(config.cookie.is_none());
    }

    #[test]
    fn web_account_selection_is_signed_and_transport_has_no_default_credentials() {
        let client = MusicClient::new(Config {
            client_version: Some("test".into()),
            cookie: Some("SAPISID=synthetic".into()),
            auth_user: 2,
            delegated_session_id: Some("12345".into()),
            ..Default::default()
        })
        .unwrap();
        let request = client
            .web_request("browse", json!({"browseId":"FEmusic_liked_playlists"}))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().host_str(), Some("music.youtube.com"));
        assert_eq!(request.headers()["x-goog-authuser"], "2");
        assert_eq!(request.headers()["x-goog-pageid"], "12345");
        assert!(request.headers()["cookie"].is_sensitive());
        assert!(request.headers()["authorization"].is_sensitive());
        assert!(request.headers()["authorization"]
            .to_str()
            .unwrap()
            .starts_with("SAPISIDHASH "));
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["context"]["user"]["onBehalfOfUser"], "12345");
        assert_eq!(body["context"]["client"]["clientName"], "WEB_REMIX");
        assert_eq!(request.headers()["x-youtube-client-name"], "67");
        let anonymous = client.http.get("https://www.youtube.com/").build().unwrap();
        assert!(!anonymous.headers().contains_key("cookie"));
        assert!(!anonymous.headers().contains_key("authorization"));
    }

    #[test]
    fn anonymous_catalog_uses_the_same_web_client_without_account_headers() {
        let client = MusicClient::new(Config {
            client_version: Some("test".into()),
            ..Default::default()
        })
        .unwrap();
        let request = client
            .web_request("search", json!({"query":"music"}))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().host_str(), Some("music.youtube.com"));
        assert_eq!(request.headers()["x-youtube-client-name"], "67");
        for name in [
            "cookie",
            "authorization",
            "x-goog-authuser",
            "x-goog-pageid",
        ] {
            assert!(!request.headers().contains_key(name));
        }
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["context"]["client"]["clientName"], "WEB_REMIX");
    }

    #[test]
    fn cookie_signing_has_known_digest_and_fallback() {
        let expected = format!(
            "SAPISIDHASH 123_{:x}",
            Sha1::digest(b"123 abc https://music.youtube.com")
        );
        assert_eq!(
            cookie_hash("other=x; SAPISID=abc; __Secure-3PAPISID=wrong", 123),
            Some(expected.clone())
        );
        assert_eq!(cookie_hash("__Secure-3PAPISID=abc", 123), Some(expected));
        assert_eq!(cookie_hash("SAPISID=; other=x", 123), None);
    }

    #[test]
    fn rotated_signer_is_used_on_the_next_request_and_never_on_static_downloads() {
        let client = MusicClient::new(Config {
            client_version: Some("test".into()),
            cookie: Some("SAPISID=old".into()),
            ..Default::default()
        })
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::SET_COOKIE,
            "SAPISID=new; Path=/; Domain=.youtube.com; Secure"
                .parse()
                .unwrap(),
        );
        client
            .lock_session()
            .unwrap()
            .observe(
                &reqwest::Url::parse(ORIGIN).unwrap(),
                &headers,
                crate::session::now().unwrap(),
            )
            .unwrap();
        let request = client
            .web_request("browse", json!({}))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.headers()["cookie"], "SAPISID=new");
        let signed = request.headers()["authorization"].to_str().unwrap();
        let timestamp: u64 = signed
            .strip_prefix("SAPISIDHASH ")
            .unwrap()
            .split('_')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            Some(signed.to_owned()),
            cookie_hash("SAPISID=new", timestamp)
        );
        let static_request = client
            .http
            .get("https://music.youtube.com/s/player/test/base.js")
            .build()
            .unwrap();
        assert!(!static_request.headers().contains_key("cookie"));
        assert!(!static_request.headers().contains_key("authorization"));
    }

    #[test]
    fn expired_signing_cookie_reports_reauthentication_without_exporting_secrets() {
        let client = MusicClient::new(Config {
            client_version: Some("test".into()),
            cookie: Some("SAPISID=private-expired".into()),
            cookie_expirations: BTreeMap::from([("SAPISID".into(), 1)]),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            client.auth_status().unwrap().state,
            crate::auth::AuthState::Rejected
        );
        let error = client.browser_session().unwrap_err();
        assert!(matches!(error, Error::AuthenticationRejected));
        assert!(!error.to_string().contains("private-expired"));
        assert!(matches!(
            client.refresh_session(),
            Err(Error::AuthenticationRejected)
        ));
    }

    #[test]
    fn bootstrap_decodes_json_strings() {
        let html = r#"ytcfg.set({"INNERTUBE_CLIENT_VERSION": "1.20260913.16.00", "VISITOR_DATA":"a\u003db"});"#;
        assert_eq!(
            config_string(html, "INNERTUBE_CLIENT_VERSION").as_deref(),
            Some("1.20260913.16.00")
        );
        assert_eq!(config_string(html, "VISITOR_DATA").as_deref(), Some("a=b"));
        assert!(config_string("consent page", "VISITOR_DATA").is_none());
    }
}
