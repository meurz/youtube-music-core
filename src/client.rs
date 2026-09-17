use crate::{model::*, parse, Error, Result};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::{
    io::Read,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ORIGIN: &str = "https://music.youtube.com";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;

/// Credentials are deliberately not Debug. Never persist this struct in logs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub language: String,
    pub country: String,
    pub client_version: Option<String>,
    pub visitor_data: Option<String>,
    pub cookie: Option<String>,
    pub oauth: Option<crate::oauth::OAuthSession>,
    pub music_oauth: Option<crate::music_oauth::MusicOAuthSession>,
    pub po_token: Option<String>,
    pub proxy: Option<String>,
    pub timeout_seconds: u64,
    pub auth_user: u32,
    pub delegated_session_id: Option<String>,
    pub playback_client: PlaybackClient,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: "en".into(),
            country: "US".into(),
            client_version: None,
            visitor_data: None,
            cookie: None,
            oauth: None,
            music_oauth: None,
            po_token: None,
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
    pub(crate) signature_timestamp: OnceLock<u64>,
    oauth: Mutex<Option<crate::oauth::OAuthSession>>,
    music_oauth: Mutex<Option<crate::music_oauth::MusicOAuthSession>>,
    tv_version: OnceLock<String>,
}

pub(crate) fn network(e: reqwest::Error) -> Error {
    // Strip URLs, which can include continuation tokens or proxy credentials.
    Error::Network(e.without_url().to_string())
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

pub(crate) fn checked_response(response: reqwest::blocking::Response) -> Result<String> {
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Http(status.as_u16()));
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
    pub fn new(mut config: Config) -> Result<Self> {
        if let Some(session) = &config.music_oauth {
            session.validate()?;
            if config.oauth.is_some()
                || config.cookie.is_some()
                || config.auth_user != 0
                || config.delegated_session_id.is_some()
            {
                return Err(Error::InvalidInput(
                    "Music OAuth cannot be combined with other credentials or account overrides"
                        .into(),
                ));
            }
            if config.client_version.is_none() {
                config.client_version = Some(crate::android::VERSION.into());
            }
        }
        if let Some(oauth) = &config.oauth {
            oauth.validate()?;
            if config.cookie.is_some()
                || config.auth_user != 0
                || config.delegated_session_id.is_some()
            {
                return Err(Error::InvalidInput("OAuth selects its own account; do not combine it with browser credentials or account overrides".into()));
            }
        }
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
        let mut headers = HeaderMap::new();
        header(&mut headers, "origin", ORIGIN)?;
        header(&mut headers, "referer", &format!("{ORIGIN}/"))?;
        if let Some(cookie) = &config.cookie {
            header(&mut headers, "cookie", cookie)?;
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
        if config.client_version.is_none() {
            let html =
                checked_response(http.get(ORIGIN).headers(headers).send().map_err(network)?)?;
            config.client_version = Some(config_string(&html, "INNERTUBE_CLIENT_VERSION").ok_or_else(|| Error::Protocol("web client bootstrap failed; provide client_version if consent or regional restrictions block the homepage".into()))?);
            if config.visitor_data.is_none() {
                config.visitor_data = config_string(&html, "VISITOR_DATA");
            }
        }
        Ok(Self {
            http,
            oauth: Mutex::new(config.oauth.clone()),
            music_oauth: Mutex::new(config.music_oauth.clone()),
            tv_version: OnceLock::new(),
            config,
            signature_timestamp: OnceLock::new(),
        })
    }

    /// Return updated OAuth state for host-owned secure persistence after calls.
    pub fn oauth_session(&self) -> Result<Option<crate::oauth::OAuthSession>> {
        self.oauth
            .lock()
            .map(|session| session.clone())
            .map_err(|_| Error::Protocol("OAuth session lock failed".into()))
    }

    /// Updated native Music grant for host-owned secure persistence.
    pub fn music_oauth_session(&self) -> Result<Option<crate::music_oauth::MusicOAuthSession>> {
        self.music_oauth
            .lock()
            .map(|s| s.clone())
            .map_err(|_| Error::Protocol("Music OAuth session lock failed".into()))
    }

    pub(crate) fn is_android_music(&self) -> bool {
        self.config.music_oauth.is_some()
    }

    pub(crate) fn music_access_token(&self) -> Result<String> {
        let mut state = self
            .music_oauth
            .lock()
            .map_err(|_| Error::Protocol("Music OAuth session lock failed".into()))?;
        let s = state.as_mut().ok_or(Error::AuthenticationRequired)?;
        s.refresh_if_needed(&self.http)?;
        Ok(s.access_token.clone())
    }

    fn oauth_access_token(&self) -> Result<Option<String>> {
        let mut state = self
            .oauth
            .lock()
            .map_err(|_| Error::Protocol("OAuth session lock failed".into()))?;
        if let Some(session) = state.as_mut() {
            session.refresh_if_needed(&self.http)?;
            return Ok(Some(session.access_token.clone()));
        }
        Ok(None)
    }

    fn web_request(
        &self,
        endpoint: &str,
        mut body: Value,
    ) -> Result<reqwest::blocking::RequestBuilder> {
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
        if let Some(cookie) = &self.config.cookie {
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

    fn tv_request(
        &self,
        endpoint: &str,
        mut body: Value,
    ) -> Result<reqwest::blocking::RequestBuilder> {
        const TV: &str = "https://www.youtube.com";
        const TV_UA: &str = "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version";
        let token = self
            .oauth_access_token()?
            .ok_or(Error::AuthenticationRequired)?;
        if self.tv_version.get().is_none() {
            let html = checked_response(
                self.http
                    .get(format!("{TV}/tv"))
                    .header("User-Agent", TV_UA)
                    .send()
                    .map_err(network)?,
            )?;
            let version = config_string(&html, "INNERTUBE_CLIENT_VERSION")
                .ok_or_else(|| Error::Protocol("TV client version is missing".into()))?;
            let _ = self.tv_version.set(version);
        }
        body["context"] = json!({"client":{"clientName":"TVHTML5","clientVersion":self.tv_version.get(),"hl":self.config.language,"gl":self.config.country},"user":{"lockedSafetyMode":false}});
        let mut headers = HeaderMap::new();
        header(&mut headers, "authorization", &format!("Bearer {token}"))?;
        header(&mut headers, "user-agent", TV_UA)?;
        Ok(self
            .http
            .post(format!("{TV}/youtubei/v1/{endpoint}?prettyPrint=false"))
            .headers(headers)
            .json(&body))
    }

    pub(crate) fn account_post(&self, endpoint: &str, body: Value) -> Result<Value> {
        if self.is_android_music() {
            return self.post(endpoint, body);
        }
        if self.config.oauth.is_some() {
            let response = self.tv_request(endpoint, body)?.send().map_err(network)?;
            return serde_json::from_str(&checked_response(response)?)
                .map_err(|_| Error::Protocol("TV API returned invalid JSON".into()));
        }
        self.post(endpoint, body)
    }

    pub(crate) fn account_page(&self, value: &Value) -> Result<Page> {
        if self.is_android_music() {
            crate::android::page(value)
        } else if self.config.oauth.is_some() {
            parse::tv_page(value)
        } else {
            parse::page(value)
        }
    }

    pub(crate) fn post(&self, endpoint: &str, body: Value) -> Result<Value> {
        if self.is_android_music() {
            return self.android_post(
                endpoint,
                body,
                self.config
                    .client_version
                    .as_deref()
                    .unwrap_or(crate::android::VERSION),
                36,
            );
        }
        let response = self.web_request(endpoint, body)?.send().map_err(network)?;
        let value: Value = serde_json::from_str(&checked_response(response)?)
            .map_err(|_| Error::Protocol("API returned invalid JSON".into()))?;
        if let Some(error) = value.get("error") {
            return Err(Error::Protocol(format!(
                "API error {}: {}",
                error["code"],
                error["message"].as_str().unwrap_or("unknown")
            )));
        }
        Ok(value)
    }

    pub fn search(&self, query: &str, filter: SearchFilter) -> Result<Page> {
        nonempty(query, "query")?;
        let mut body = json!({"query":query});
        if let Some(params) = filter.params() {
            body["params"] = params.into();
        }
        let value = self.post("search", body)?;
        self.catalog_page(&value)
    }

    /// A browse ID can identify an album, artist, playlist, or home feed.
    pub fn browse(&self, browse_id: &str) -> Result<Page> {
        nonempty(browse_id, "browse_id")?;
        self.account_page(&self.account_post("browse", json!({"browseId":browse_id}))?)
    }

    pub fn playlist(&self, playlist_id: &str) -> Result<Page> {
        nonempty(playlist_id, "playlist_id")?;
        let id = if playlist_id.starts_with("VL") {
            playlist_id.into()
        } else {
            format!("VL{playlist_id}")
        };
        self.browse(&id)
    }

    pub fn continue_page(&self, endpoint: ContinuationEndpoint, token: &str) -> Result<Page> {
        nonempty(token, "continuation")?;
        if matches!(endpoint, ContinuationEndpoint::Browse) {
            self.account_page(&self.account_post(endpoint.as_str(), json!({"continuation":token}))?)
        } else {
            self.catalog_page(&self.post(endpoint.as_str(), json!({"continuation":token}))?)
        }
    }

    fn next_raw(&self, video_id: &str) -> Result<Value> {
        validate_video_id(video_id)?;
        self.post(
            "next",
            json!({"videoId":video_id, "enablePersistentPlaylistPanel":true, "isAudioOnly":true}),
        )
    }

    pub fn queue(&self, video_id: &str) -> Result<Page> {
        let next = self.next_raw(video_id)?;
        if let Some(endpoint) = parse::find(&next, "automixPreviewVideoRenderer")
            .and_then(|v| parse::find(v, "watchPlaylistEndpoint"))
        {
            if let Some(playlist_id) = endpoint["playlistId"].as_str() {
                let mut body = json!({"videoId":video_id, "playlistId":playlist_id, "isAudioOnly":true, "enablePersistentPlaylistPanel":true});
                if let Some(params) = endpoint.get("params") {
                    body["params"] = params.clone();
                }
                return self.catalog_page(&self.post("next", body)?);
            }
        }
        self.catalog_page(&next)
    }

    pub fn song(&self, video_id: &str) -> Result<Track> {
        let p = self.player(video_id)?;
        p.track.ok_or_else(|| Error::Unplayable {
            status: p.status,
            reason: p.reason.unwrap_or_else(|| "no track metadata".into()),
        })
    }

    pub fn lyrics(&self, video_id: &str) -> Result<Lyrics> {
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
    fn oauth_bearer_is_music_scoped_and_never_combined_with_cookies() {
        let session = crate::oauth::OAuthSession {
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            expires_at: u64::MAX,
            client_id: "synthetic-client".into(),
            client_secret: "synthetic-secret".into(),
        };
        let mut config = Config {
            client_version: Some("test".into()),
            cookie: Some("SAPISID=old".into()),
            ..Default::default()
        };
        session.apply_to(&mut config).unwrap();
        assert!(config.cookie.is_none());
        let client = MusicClient::new(config).unwrap();
        let _ = client.tv_version.set("test".into());
        let request = client
            .tv_request("account/accounts_list", json!({}))
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            request.headers()["authorization"],
            "Bearer synthetic-access"
        );
        assert!(request.headers()["authorization"].is_sensitive());
        assert!(!request.headers().contains_key("cookie"));
        let anonymous = client.http.get("https://www.youtube.com/").build().unwrap();
        assert!(!anonymous.headers().contains_key("authorization"));
        assert_eq!(client.oauth_session().unwrap().unwrap(), session);
        let conflicting = Config {
            client_version: Some("test".into()),
            cookie: Some("SAPISID=old".into()),
            oauth: Some(session),
            ..Default::default()
        };
        assert!(MusicClient::new(conflicting).is_err());
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
        let anonymous = client.http.get("https://www.youtube.com/").build().unwrap();
        assert!(!anonymous.headers().contains_key("cookie"));
        assert!(!anonymous.headers().contains_key("authorization"));
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
