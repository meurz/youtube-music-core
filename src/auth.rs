//! YouTube Music browser-session authentication. This is not Google OAuth.
use crate::{
    client::{cookie_hash, header},
    parse, Config, Error, MusicClient, Result,
};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";
const MAX_IMPORT: usize = 1024 * 1024;

/// Secret material. Serialize only into a host-provided secure credential store.
/// Debug intentionally redacts all fields.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSession {
    pub cookie: String,
    #[serde(default)]
    pub auth_user: u32,
    #[serde(default)]
    pub delegated_session_id: Option<String>,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserSession([REDACTED])")
    }
}

impl BrowserSession {
    pub fn validate(&self) -> Result<()> {
        if self.cookie.is_empty() || self.cookie.len() > 65536 || self.auth_user > 99 {
            return Err(Error::InvalidInput(
                "invalid browser session or account index (expected 0..99)".into(),
            ));
        }
        let mut headers = HeaderMap::new();
        header(&mut headers, "cookie", &self.cookie)?;
        if cookie_hash(&self.cookie, 1).is_none() {
            return Err(Error::AuthenticationRequired);
        }
        if let Some(id) = &self.delegated_session_id {
            if id.is_empty()
                || id.len() > 256
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(Error::InvalidInput("invalid delegated session ID".into()));
            }
        }
        Ok(())
    }

    /// Accept raw browser request headers, a JSON header object, or a Cookie header.
    /// Captured Authorization hashes are discarded and regenerated on each request.
    pub fn from_browser_headers(input: &str) -> Result<Self> {
        if input.len() > MAX_IMPORT {
            return Err(Error::InvalidInput("session import exceeds 1 MiB".into()));
        }
        let input = input.trim();
        let mut fields = BTreeMap::new();
        if input.starts_with('{') {
            let values: BTreeMap<String, Value> = serde_json::from_str(input).map_err(|_| {
                Error::InvalidInput("expected a browser request-header JSON object".into())
            })?;
            for (key, value) in values {
                if let Some(value) = value.as_str() {
                    insert_header(&mut fields, &key, value)?;
                }
            }
        } else if input.starts_with("# Netscape HTTP Cookie File")
            || input.starts_with("# HTTP Cookie File")
        {
            return Self::from_netscape(input);
        } else if !input.contains('\n')
            && !input.to_ascii_lowercase().starts_with("cookie:")
            && input.contains('=')
        {
            fields.insert("cookie".into(), input.into());
        } else {
            let mut pending: Option<String> = None;
            for line in input.lines().map(str::trim).filter(|l| !l.is_empty()) {
                if line.starts_with(':') {
                    pending = None;
                    continue;
                }
                if let Some(key) = pending.take() {
                    insert_header(&mut fields, &key, line)?;
                    continue;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if value.trim().is_empty() {
                        pending = Some(key.to_ascii_lowercase());
                    } else {
                        insert_header(&mut fields, key, value.trim())?;
                    }
                } else if header_name(line) {
                    pending = Some(line.to_ascii_lowercase());
                }
            }
        }
        if let Some(origin) = fields.get("origin").or_else(|| fields.get("x-origin")) {
            if origin != "https://music.youtube.com" {
                return Err(Error::InvalidInput(
                    "import request headers from music.youtube.com".into(),
                ));
            }
        }
        let session = Self {
            cookie: fields.remove("cookie").ok_or_else(|| {
                Error::InvalidInput("browser headers contain no Cookie header".into())
            })?,
            auth_user: fields
                .get("x-goog-authuser")
                .map(|v| v.parse::<u32>())
                .transpose()
                .map_err(|_| Error::InvalidInput("invalid browser account index".into()))?
                .unwrap_or(0),
            delegated_session_id: fields.remove("x-goog-pageid"),
        };
        session.validate()?;
        Ok(session)
    }

    /// Import only cookies that apply to HTTPS music.youtube.com, never other sites.
    pub fn from_netscape(input: &str) -> Result<Self> {
        if input.len() > MAX_IMPORT {
            return Err(Error::InvalidInput("session import exceeds 1 MiB".into()));
        }
        let mut cookies = BTreeMap::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Protocol("system clock before Unix epoch".into()))?
            .as_secs();
        for line in input.lines() {
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let fields: Vec<_> = line.splitn(7, '\t').collect();
            if fields.len() != 7 {
                return Err(Error::InvalidInput("invalid Netscape cookie row".into()));
            }
            let domain = fields[0].trim_start_matches('.');
            if !(domain == "music.youtube.com" || (domain == "youtube.com" && fields[1] == "TRUE"))
                || fields[2] != "/"
            {
                continue;
            }
            let expires = fields[4]
                .parse::<u64>()
                .map_err(|_| Error::InvalidInput("invalid cookie expiry".into()))?;
            if expires != 0 && expires <= now {
                continue;
            }
            let name = fields[5];
            if name.is_empty()
                || name.bytes().any(|b| b <= 32 || b == b';' || b == b'=')
                || fields[6].contains(['\r', '\n', ';'])
            {
                return Err(Error::InvalidInput("invalid cookie name or value".into()));
            }
            // Reject ambiguous same-name cookies rather than signing a different account.
            if cookies.insert(name, fields[6]).is_some() {
                return Err(Error::InvalidInput(
                    "ambiguous duplicate cookies; import the browser request headers instead"
                        .into(),
                ));
            }
        }
        let session = Self {
            cookie: cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
            auth_user: 0,
            delegated_session_id: None,
        };
        session.validate()?;
        Ok(session)
    }

    pub fn apply_to(&self, config: &mut Config) -> Result<()> {
        self.validate()?;
        config.cookie = Some(self.cookie.clone());
        config.auth_user = self.auth_user;
        config.delegated_session_id = self.delegated_session_id.clone();
        Ok(())
    }
}

fn header_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cookie" | "x-goog-authuser" | "x-goog-pageid" | "origin" | "x-origin"
    )
}

fn insert_header(fields: &mut BTreeMap<String, String>, name: &str, value: &str) -> Result<()> {
    let name = name.trim().to_ascii_lowercase();
    if !header_name(&name) {
        return Ok(());
    }
    if fields.insert(name, value.trim().into()).is_some() {
        return Err(Error::InvalidInput(
            "duplicate authentication header".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub name: String,
    pub channel_handle: Option<String>,
    pub photo_url: Option<String>,
    pub auth_user: u32,
    pub delegated_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    SignedOut,
    Authenticated,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatus {
    pub state: AuthState,
    pub account: Option<AccountInfo>,
}

pub(crate) fn explicitly_signed_out(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("loggedOut").and_then(Value::as_bool) == Some(true)
                || (map.get("key").and_then(Value::as_str) == Some("logged_in")
                    && map.get("value").and_then(Value::as_str) == Some("0"))
                || map.values().any(explicitly_signed_out)
        }
        Value::Array(array) => array.iter().any(explicitly_signed_out),
        _ => false,
    }
}

pub(crate) fn parse_account(value: &Value, config: &Config) -> Result<AccountInfo> {
    if explicitly_signed_out(value) {
        return Err(Error::AuthenticationRejected);
    }
    let header = parse::find(value, "activeAccountHeaderRenderer").ok_or_else(|| {
        if parse::find(value, "signInEndpoint").is_some() {
            Error::AuthenticationRejected
        } else {
            Error::Protocol("account menu is missing the active account header".into())
        }
    })?;
    let name = parse::text(&header["accountName"]);
    if name.is_empty() {
        return Err(Error::Protocol(
            "account menu is missing the account name".into(),
        ));
    }
    let handle = parse::text(&header["channelHandle"]);
    Ok(AccountInfo {
        name,
        channel_handle: (!handle.is_empty()).then_some(handle),
        photo_url: header["accountPhoto"]["thumbnails"]
            .as_array()
            .and_then(|a| a.last())
            .and_then(|v| v["url"].as_str())
            .map(str::to_owned),
        auth_user: config.auth_user,
        delegated_session_id: config.delegated_session_id.clone(),
    })
}

impl MusicClient {
    pub(crate) fn require_session(&self) -> Result<()> {
        if self
            .config
            .cookie
            .as_deref()
            .and_then(|cookie| cookie_hash(cookie, 1))
            .is_none()
        {
            return Err(Error::AuthenticationRequired);
        }
        Ok(())
    }

    /// Verify the selected Music account remotely. Presence of a cookie is not proof.
    pub fn account(&self) -> Result<AccountInfo> {
        self.require_session()?;
        let value = self.post("account/account_menu", json!({})).map_err(|e| {
            if matches!(e, Error::Http(401) | Error::Http(403)) {
                Error::AuthenticationRejected
            } else {
                e
            }
        })?;
        parse_account(&value, &self.config)
    }

    pub fn auth_status(&self) -> Result<AuthStatus> {
        if self.require_session().is_err() {
            return Ok(AuthStatus {
                state: AuthState::SignedOut,
                account: None,
            });
        }
        match self.account() {
            Ok(account) => Ok(AuthStatus {
                state: AuthState::Authenticated,
                account: Some(account),
            }),
            Err(Error::AuthenticationRejected) => Ok(AuthStatus {
                state: AuthState::Rejected,
                account: None,
            }),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_discards_captured_hash_and_unrelated_headers() {
        let session = BrowserSession::from_browser_headers("POST /youtubei/v1/browse HTTP/2\nCookie: SID=one; SAPISID=two\nAuthorization: obsolete-private-value\nX-Goog-AuthUser: 2\nX-Goog-PageId: 12345\nOrigin: https://music.youtube.com\n").unwrap();
        assert_eq!(session.auth_user, 2);
        assert_eq!(session.delegated_session_id.as_deref(), Some("12345"));
        assert!(!serde_json::to_string(&session)
            .unwrap()
            .contains("obsolete-private-value"));
        assert!(!format!("{session:?}").contains("SAPISID"));
        let mut config = Config::default();
        session.apply_to(&mut config).unwrap();
        assert_eq!(config.auth_user, 2);
        assert_eq!(config.cookie.as_deref(), Some("SID=one; SAPISID=two"));
    }

    #[test]
    fn chrome_split_headers_and_json_headers_are_supported() {
        for text in [
            "cookie:\nSAPISID=synthetic\nx-goog-authuser:\n1",
            "cookie\nSAPISID=synthetic\nx-goog-authuser\n1",
            r#"{"Cookie":"SAPISID=synthetic","X-Goog-AuthUser":"1"}"#,
        ] {
            let session = BrowserSession::from_browser_headers(text).unwrap();
            assert_eq!(session.auth_user, 1);
            assert_eq!(session.cookie, "SAPISID=synthetic");
        }
        assert_eq!(
            BrowserSession::from_browser_headers("SAPISID=synthetic; SID=other")
                .unwrap()
                .auth_user,
            0
        );
    }

    #[test]
    fn invalid_imports_do_not_echo_credentials() {
        for text in [
            r#"{"cookie":"secret","x-goog-authuser":"oops"}"#,
            "Cookie: private\nOrigin: https://evil.test",
            "Cookie: SAPISID=one\nCookie: SAPISID=two",
            "cookie\nSID=private",
            "Cookie: SAPISID=private\nX-Goog-AuthUser: 100",
        ] {
            let error = BrowserSession::from_browser_headers(text).unwrap_err();
            assert!(!error.to_string().contains("private"));
            assert!(!error.to_string().contains("secret"));
        }
    }

    #[test]
    fn netscape_import_filters_domains_paths_and_expired_cookies() {
        let text = "# Netscape HTTP Cookie File\n#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t4102444800\tSAPISID\tsynthetic\n.google.com\tTRUE\t/\tTRUE\t4102444800\tunrelated\tprivate\n.youtube.com\tTRUE\t/\tTRUE\t1\texpired\tprivate\n.youtube.com\tTRUE\t/other\tTRUE\t0\tpath\tprivate\n";
        let session = BrowserSession::from_browser_headers(text).unwrap();
        assert_eq!(session.cookie, "SAPISID=synthetic");
        let duplicate = format!("{text}.youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tother\n");
        assert!(BrowserSession::from_netscape(&duplicate).is_err());
    }

    #[test]
    fn partitioned_preferences_are_retained_but_conflicting_signers_fail() {
        let header =
            "SAPISID=synthetic; VISITOR_PRIVACY_METADATA=one; VISITOR_PRIVACY_METADATA=two";
        assert_eq!(
            BrowserSession::from_browser_headers(header).unwrap().cookie,
            header
        );
        assert!(BrowserSession::from_browser_headers("SAPISID=one; SAPISID=two").is_err());
        assert!(BrowserSession::from_browser_headers(
            "SAPISID=one; __Secure-3PAPISID=two; __Secure-3PAPISID=three"
        )
        .is_err());
    }

    #[test]
    fn account_identity_requires_a_real_active_header() {
        let config = Config {
            auth_user: 2,
            delegated_session_id: Some("123".into()),
            ..Default::default()
        };
        let value = json!({"actions":[{"openPopupAction":{"popup":{"multiPageMenuRenderer":{"header":{"activeAccountHeaderRenderer":{
            "accountName":{"runs":[{"text":"Test account"}]},"channelHandle":{"simpleText":"@test"},
            "accountPhoto":{"thumbnails":[{"url":"https://example.com/photo"}]}
        }}}}}}]});
        let account = parse_account(&value, &config).unwrap();
        assert_eq!(account.name, "Test account");
        assert_eq!(account.auth_user, 2);
        assert_eq!(account.channel_handle.as_deref(), Some("@test"));
        assert!(matches!(
            parse_account(
                &json!({"responseContext":{"serviceTrackingParams":[{"params":[{"key":"logged_in","value":"0"}]}]}}),
                &config
            ),
            Err(Error::AuthenticationRejected)
        ));
        assert!(matches!(
            parse_account(&json!({"actions":[]}), &config),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn library_requires_authentication_before_a_request() {
        let client = MusicClient::new(Config {
            client_version: Some("test".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(client.auth_status().unwrap().state, AuthState::SignedOut);
        assert!(matches!(
            client.library(crate::library::LibrarySection::Playlists, None),
            Err(Error::AuthenticationRequired)
        ));
    }
}
