//! Experimental authorization-code + PKCE flow using a candidate Music iOS client.
//! The identifier was recovered from a modified IPA; its original provenance and
//! compatibility with Android Music are not verified. Google displays an
//! unverified-app warning. This flow is not used by the released CLI login.
//! This module does not open a browser, register URI handlers, or store credentials.
use crate::{client::network, Config, Error, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{blocking::Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

// Public identifier from GoogleService-Info.plist in the investigated IPA.
// This candidate uses PKCE without a client secret. See the protocol investigation.
pub const CLIENT_ID: &str =
    "755973059757-ipk9n6laup0pc9a4i8gmdqmj4bqt9noj.apps.googleusercontent.com";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str =
    "https://www.googleapis.com/auth/youtube https://www.googleapis.com/auth/youtube.force-ssl";

fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::Protocol("system clock before Unix epoch".into()))
}

/// Secret material. Serialize only into host-owned secure storage.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MusicOAuthSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub client_id: String,
}

impl std::fmt::Debug for MusicOAuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MusicOAuthSession([REDACTED])")
    }
}

impl MusicOAuthSession {
    pub fn apply_to(&self, config: &mut Config) -> Result<()> {
        self.validate()?;
        config.oauth = None;
        config.cookie = None;
        config.auth_user = 0;
        config.delegated_session_id = None;
        config.music_oauth = Some(self.clone());
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        for s in [&self.access_token, &self.refresh_token, &self.client_id] {
            if s.is_empty() || s.len() > 16384 || s.bytes().any(|b| b <= 32 || b >= 127) {
                return Err(Error::InvalidInput("invalid Music OAuth session".into()));
            }
        }
        if !self.client_id.ends_with(".apps.googleusercontent.com") {
            return Err(Error::InvalidInput("invalid Music OAuth client ID".into()));
        }
        Ok(())
    }

    pub(crate) fn refresh_if_needed(&mut self, http: &Client) -> Result<()> {
        self.validate()?;
        if self.expires_at > now()?.saturating_add(60) {
            return Ok(());
        }
        let data = token_request(
            http,
            &[
                ("client_id", self.client_id.as_str()),
                ("refresh_token", self.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ],
        )?;
        *self = parse_token(&data, &self.client_id, Some(&self.refresh_token))?;
        Ok(())
    }
}

/// The host may display authorization_url and register callback_scheme as a URI handler.
/// Keep this object alive until exchange(); the PKCE verifier stays in memory.
pub struct MusicAuthorization {
    pub authorization_url: String,
    pub callback_scheme: String,
    pub state: String,
    verifier: Zeroizing<String>,
    redirect_uri: String,
    deadline: u64,
}

impl std::fmt::Debug for MusicAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MusicAuthorization([REDACTED])")
    }
}

pub struct MusicAuthClient {
    http: Client,
}

impl MusicAuthClient {
    pub fn new(config: &Config) -> Result<Self> {
        if !(1..=300).contains(&config.timeout_seconds) {
            return Err(Error::InvalidInput(
                "timeout_seconds must be between 1 and 300".into(),
            ));
        }
        let mut b = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = &config.proxy {
            b = b.proxy(
                reqwest::Proxy::all(proxy)
                    .map_err(|_| Error::InvalidInput("invalid proxy URL".into()))?,
            );
        }
        Ok(Self {
            http: b.build().map_err(network)?,
        })
    }

    pub fn begin(&self) -> Result<MusicAuthorization> {
        let mut random = [0u8; 80];
        getrandom::fill(&mut random)
            .map_err(|_| Error::OAuth("secure randomness unavailable".into()))?;
        let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(&random[..48]));
        let state = URL_SAFE_NO_PAD.encode(&random[48..]);
        let callback_scheme = format!(
            "com.googleusercontent.apps.{}",
            CLIENT_ID.trim_end_matches(".apps.googleusercontent.com")
        );
        let redirect_uri = format!("{callback_scheme}:/oauth2redirect");
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse(AUTH_URL).expect("constant URL");
        url.query_pairs_mut().extend_pairs([
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", state.as_str()),
        ]);
        Ok(MusicAuthorization {
            authorization_url: url.into(),
            callback_scheme,
            state,
            verifier,
            redirect_uri,
            deadline: now()?.saturating_add(1800),
        })
    }

    /// Consume the attempt so a code cannot be exchanged twice through this object.
    pub fn exchange(
        &self,
        auth: MusicAuthorization,
        callback_uri: &str,
    ) -> Result<MusicOAuthSession> {
        if now()? >= auth.deadline {
            return Err(Error::OAuth("authorization attempt expired".into()));
        }
        let code = validate_callback(callback_uri, &auth.callback_scheme, &auth.state)?;
        let data = token_request(
            &self.http,
            &[
                ("client_id", CLIENT_ID),
                ("code", code.as_str()),
                ("redirect_uri", auth.redirect_uri.as_str()),
                ("code_verifier", auth.verifier.as_str()),
                ("grant_type", "authorization_code"),
            ],
        )?;
        parse_token(&data, CLIENT_ID, None)
    }
}

/// Validate a host callback before accepting its authorization code. Never log the URI.
pub fn validate_callback(uri: &str, scheme: &str, state: &str) -> Result<Zeroizing<String>> {
    let bad = || Error::OAuth("invalid authorization callback".into());
    if uri.len() > 16384 {
        return Err(bad());
    }
    let u = Url::parse(uri).map_err(|_| bad())?;
    if u.scheme() != scheme
        || u.host_str().is_some()
        || u.path() != "/oauth2redirect"
        || u.fragment().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(bad());
    }
    let pairs: Vec<_> = u.query_pairs().collect();
    let one = |key: &str| -> Result<Option<String>> {
        let mut matches = pairs.iter().filter(|(k, _)| k == key);
        let first = matches.next().map(|(_, v)| v.to_string());
        if matches.next().is_some() {
            return Err(bad());
        }
        Ok(first)
    };
    if one("state")?.as_deref() != Some(state) || state.is_empty() {
        return Err(bad());
    }
    let error = one("error")?;
    let code = one("code")?;
    match (code, error) {
        (Some(code), None)
            if !code.is_empty()
                && code.len() <= 8192
                && !code.bytes().any(|b| b <= 32 || b >= 127) =>
        {
            Ok(Zeroizing::new(code))
        }
        (None, Some(error)) if error == "access_denied" => {
            Err(Error::OAuth("authorization was declined".into()))
        }
        _ => Err(bad()),
    }
}

fn token_request(http: &Client, fields: &[(&str, &str)]) -> Result<Value> {
    let r = http.post(TOKEN_URL).form(fields).send().map_err(network)?;
    let status = r.status();
    let mut bytes = Zeroizing::new(Vec::new());
    r.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::OAuth("cannot read token response".into()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::OAuth("token response exceeds 1 MiB".into()));
    }
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::OAuth("token response is not JSON".into()))?;
    if v.get("error").is_some() {
        return Err(match v["error"].as_str() {
            Some("invalid_grant" | "invalid_token") => Error::AuthenticationRejected,
            _ => Error::OAuth("Google rejected the Music token request".into()),
        });
    }
    if !status.is_success() {
        return Err(Error::Http(status.as_u16()));
    }
    Ok(v)
}

fn parse_token(v: &Value, client_id: &str, old_refresh: Option<&str>) -> Result<MusicOAuthSession> {
    let bad = || Error::OAuth("invalid Music token response".into());
    if !v["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(bad());
    }
    let ttl = v["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 86400 * 30)
        .ok_or_else(bad)?;
    let s = MusicOAuthSession {
        access_token: v["access_token"].as_str().ok_or_else(bad)?.into(),
        refresh_token: v["refresh_token"]
            .as_str()
            .or(old_refresh)
            .ok_or_else(bad)?
            .into(),
        expires_at: now()?.saturating_add(ttl),
        client_id: client_id.into(),
    };
    s.validate()?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn callback_rejects_csrf_duplicate_fields_and_wrong_destinations() {
        let good = "com.test:/oauth2redirect?code=synthetic&state=expected";
        assert_eq!(
            validate_callback(good, "com.test", "expected")
                .unwrap()
                .as_str(),
            "synthetic"
        );
        for bad in [
            "com.test:/oauth2redirect?code=secret&state=other",
            "com.test:/oauth2redirect?code=a&code=b&state=expected",
            "com.test:/oauth2redirect?code=a&state=expected&state=other",
            "com.test://host/oauth2redirect?code=a&state=expected",
            "com.other:/oauth2redirect?code=a&state=expected",
            "com.test:/oauth2redirect?code=a&state=expected#x",
            "com.test:/oauth2redirect?code=a&error=access_denied&state=expected",
        ] {
            let e = validate_callback(bad, "com.test", "expected").unwrap_err();
            assert!(!e.to_string().contains(bad));
        }
    }
    #[test]
    fn pkce_and_state_are_unique_and_verifier_is_not_in_the_authorization_url() {
        let client = MusicAuthClient::new(&Config::default()).unwrap();
        let a = client.begin().unwrap();
        let b = client.begin().unwrap();
        assert_ne!(a.state, b.state);
        assert!(!a.authorization_url.contains(a.verifier.as_str()));
        let u = Url::parse(&a.authorization_url).unwrap();
        assert_eq!(u.host_str(), Some("accounts.google.com"));
        let p: std::collections::HashMap<_, _> = u.query_pairs().collect();
        assert_eq!(
            p["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(a.verifier.as_bytes()))
        );
        assert_eq!(p["code_challenge_method"], "S256");
        assert!(!format!("{a:?}").contains(a.verifier.as_str()));
    }
    #[test]
    fn refresh_keeps_the_grant_and_invalid_responses_cannot_create_sessions() {
        let v = json!({"access_token":"synthetic-access","expires_in":3600,"token_type":"Bearer"});
        assert!(parse_token(&v, CLIENT_ID, None).is_err());
        let s = parse_token(&v, CLIENT_ID, Some("synthetic-refresh")).unwrap();
        assert_eq!(s.refresh_token, "synthetic-refresh");
        assert!(!format!("{s:?}").contains("synthetic"));
        assert!(parse_token(
            &json!({"access_token":"private","expires_in":0,"token_type":"Bearer"}),
            CLIENT_ID,
            Some("refresh")
        )
        .is_err());
    }
}
