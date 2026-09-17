//! YouTube TV device authorization using dynamically discovered public client configuration.
use crate::{
    client::{checked_response, network},
    Config, Error, Result,
};
use reqwest::{blocking::Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BASE: &str = "https://www.youtube.com";
const UA: &str = "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version";

fn protocol(message: &str) -> Error {
    Error::Protocol(message.into())
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| protocol("system clock before Unix epoch"))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub client_id: String,
    pub client_secret: String,
}
impl std::fmt::Debug for OAuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthSession([REDACTED])")
    }
}
impl OAuthSession {
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.access_token,
            &self.refresh_token,
            &self.client_id,
            &self.client_secret,
        ] {
            if value.is_empty() || value.len() > 16384 || value.bytes().any(|b| b <= 32 || b >= 127)
            {
                return Err(Error::InvalidInput("invalid OAuth session".into()));
            }
        }
        Ok(())
    }
    pub fn apply_to(&self, config: &mut Config) -> Result<()> {
        self.validate()?;
        config.cookie = None;
        config.music_oauth = None;
        config.auth_user = 0;
        config.delegated_session_id = None;
        config.oauth = Some(self.clone());
        Ok(())
    }
    pub(crate) fn refresh_if_needed(&mut self, http: &Client) -> Result<()> {
        if self.expires_at > now()?.saturating_add(60) {
            return Ok(());
        }
        let result = post(
            http,
            "/o/oauth2/token",
            json!({"client_id":self.client_id,"client_secret":self.client_secret,"refresh_token":self.refresh_token,"grant_type":"refresh_token"}),
        )?;
        if result.get("error").is_some() {
            return Err(oauth_error(&result));
        }
        let updated = token_session(
            &result,
            &self.client_id,
            &self.client_secret,
            Some(&self.refresh_token),
        )?;
        *self = updated;
        Ok(())
    }
}

/// Public fields may be shown to the user. Device code and client parameters are private.
pub struct DeviceAuthorization {
    pub verification_url: String,
    pub user_code: String,
    pub expires_in: u64,
    pub interval: u64,
    device_code: String,
    client_id: String,
    client_secret: String,
    deadline: u64,
}
impl std::fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceAuthorization([REDACTED])")
    }
}

pub enum PollResult {
    Pending,
    SlowDown,
    Authorized(OAuthSession),
}

pub struct DeviceAuthClient {
    http: Client,
}
impl DeviceAuthClient {
    pub fn new(config: &Config) -> Result<Self> {
        if !(1..=300).contains(&config.timeout_seconds) {
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
        Ok(Self {
            http: builder.build().map_err(network)?,
        })
    }
    pub fn begin(&self) -> Result<DeviceAuthorization> {
        let html = checked_response(
            self.http
                .get(format!("{BASE}/tv"))
                .header("Referer", format!("{BASE}/tv"))
                .header("Accept-Language", "en-US")
                .send()
                .map_err(network)?,
        )?;
        let path = tv_script(&html)?;
        let script = checked_response(self.http.get(script_url(&path)?).send().map_err(network)?)?;
        let (client_id, client_secret) = client_identity(&script)?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| protocol("secure randomness unavailable"))?;
        random[6] = (random[6] & 15) | 64;
        random[8] = (random[8] & 63) | 128;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let device_id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        let response = post(
            &self.http,
            "/o/oauth2/device/code",
            json!({"client_id":client_id,"scope":"http://gdata.youtube.com https://www.googleapis.com/auth/youtube-paid-content","device_id":device_id,"device_model":"ytlr::"}),
        )?;
        if response.get("error").is_some() || response.get("error_code").is_some() {
            return Err(oauth_error(&response));
        }
        parse_device(&response, client_id, client_secret)
    }
    /// Call no faster than the returned interval; SlowDown increases it by five seconds.
    pub fn poll(&self, device: &DeviceAuthorization) -> Result<PollResult> {
        if now()? >= device.deadline {
            return Err(Error::OAuth(
                "device code expired; run auth login again".into(),
            ));
        }
        let response = post(
            &self.http,
            "/o/oauth2/token",
            json!({"client_id":device.client_id,"client_secret":device.client_secret,"code":device.device_code,"grant_type":"http://oauth.net/grant_type/device/1.0"}),
        )?;
        match response["error"].as_str() {
            Some("authorization_pending") => Ok(PollResult::Pending),
            Some("slow_down") => Ok(PollResult::SlowDown),
            Some(_) => Err(oauth_error(&response)),
            None => Ok(PollResult::Authorized(token_session(
                &response,
                &device.client_id,
                &device.client_secret,
                None,
            )?)),
        }
    }
}

fn post(http: &Client, endpoint: &str, body: Value) -> Result<Value> {
    use std::io::Read;
    let response = http
        .post(format!("{BASE}{endpoint}"))
        .json(&body)
        .send()
        .map_err(network)?;
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| protocol("cannot read OAuth response"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(protocol("OAuth response exceeds 1 MiB"));
    }
    let data: Value =
        serde_json::from_slice(&bytes).map_err(|_| protocol("OAuth response is not JSON"))?;
    if !status.is_success() && data.get("error").is_none() {
        return Err(Error::Http(status.as_u16()));
    }
    Ok(data)
}
fn oauth_error(data: &Value) -> Error {
    match data["error"].as_str() {
        Some("invalid_grant" | "invalid_token") => Error::AuthenticationRejected,
        Some("access_denied") => Error::OAuth("authorization was declined".into()),
        Some("expired_token") => Error::OAuth("device code expired; run auth login again".into()),
        Some("invalid_client" | "unauthorized_client") => {
            Error::OAuth("YouTube rejected the TV client identity".into())
        }
        _ => Error::OAuth("YouTube rejected the device authorization request".into()),
    }
}
fn field(data: &Value, key: &str) -> Result<String> {
    data[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16384)
        .map(str::to_owned)
        .ok_or_else(|| protocol("OAuth response is missing a required field"))
}
fn token_session(
    data: &Value,
    client_id: &str,
    client_secret: &str,
    fallback_refresh: Option<&str>,
) -> Result<OAuthSession> {
    if !data["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(protocol("unsupported OAuth token type"));
    }
    let ttl = data["expires_in"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 86400 * 30)
        .ok_or_else(|| protocol("invalid OAuth token lifetime"))?;
    let session = OAuthSession {
        access_token: field(data, "access_token")?,
        refresh_token: data["refresh_token"]
            .as_str()
            .or(fallback_refresh)
            .map(str::to_owned)
            .ok_or_else(|| protocol("OAuth response has no refresh token"))?,
        expires_at: now()?.saturating_add(ttl),
        client_id: client_id.into(),
        client_secret: client_secret.into(),
    };
    session.validate()?;
    Ok(session)
}
fn parse_device(
    data: &Value,
    client_id: String,
    client_secret: String,
) -> Result<DeviceAuthorization> {
    let url = field(data, "verification_url")?;
    let parsed = Url::parse(&url).map_err(|_| protocol("invalid Google verification URL"))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("www.google.com")
        || parsed.path() != "/device"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
    {
        return Err(protocol("unexpected Google verification URL"));
    }
    let user_code = field(data, "user_code")?;
    if user_code.len() > 32
        || !user_code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(protocol("invalid device user code"));
    }
    let expires_in = data["expires_in"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 3600)
        .ok_or_else(|| protocol("invalid device code lifetime"))?;
    let interval = data["interval"].as_u64().unwrap_or(5).clamp(5, 60);
    Ok(DeviceAuthorization {
        verification_url: url,
        user_code,
        expires_in,
        interval,
        device_code: field(data, "device_code")?,
        client_id,
        client_secret,
        deadline: now()?.saturating_add(expires_in),
    })
}
fn tv_script(html: &str) -> Result<String> {
    let tag = html
        .split("<script")
        .find(|tag| {
            tag.split('>')
                .next()
                .is_some_and(|head| head.contains("id=\"base-js\""))
        })
        .ok_or_else(|| protocol("YouTube TV bootstrap script is missing"))?;
    let head = tag.split('>').next().unwrap_or_default();
    head.split_once("src=\"")
        .and_then(|(_, s)| s.split_once('"'))
        .map(|(s, _)| s.to_owned())
        .ok_or_else(|| protocol("YouTube TV script URL is missing"))
}
fn script_url(path: &str) -> Result<Url> {
    let url = Url::parse(BASE)
        .expect("constant URL")
        .join(path)
        .map_err(|_| protocol("invalid TV script URL"))?;
    if url.scheme() != "https"
        || url.host_str() != Some("www.youtube.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().starts_with("/s/")
    {
        return Err(protocol("TV script must remain on the YouTube origin"));
    }
    Ok(url)
}
fn js_string(input: &str) -> Result<(String, usize)> {
    let mut values = serde_json::Deserializer::from_str(input).into_iter::<String>();
    let value = values
        .next()
        .and_then(|v| v.ok())
        .ok_or_else(|| protocol("invalid TV client configuration"))?;
    Ok((value, values.byte_offset()))
}
fn client_identity(script: &str) -> Result<(String, String)> {
    for rest in script.split("clientId:").skip(1) {
        if let Ok((client_id, end)) = js_string(rest) {
            if let Some(next) = rest[end..]
                .strip_prefix(',')
                .and_then(|s| s.split_once(':'))
                .map(|(_, s)| s)
            {
                if let Ok((secret, _)) = js_string(next) {
                    if client_id.ends_with(".apps.googleusercontent.com")
                        && !secret.is_empty()
                        && secret.len() < 1024
                    {
                        return Ok((client_id, secret));
                    }
                }
            }
        }
    }
    Err(protocol("YouTube TV client configuration changed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tv_bootstrap_is_scoped_and_identity_is_dynamic() {
        assert_eq!(
            tv_script("<script src=\"/s/tv.js\" id=\"base-js\"></script>").unwrap(),
            "/s/tv.js"
        );
        assert!(script_url("//evil.test/s/base.js").is_err());
        assert!(script_url("https://www.youtube.com.evil.test/s/base.js").is_err());
        assert!(script_url("/s/tv.js").is_ok());
        let identity = client_identity(
            "const a={clientId:\"test.apps.googleusercontent.com\",secret:\"synthetic\"};",
        )
        .unwrap();
        assert_eq!(identity.1, "synthetic");
    }
    #[test]
    fn device_url_and_codes_are_validated_and_debug_is_redacted() {
        let mut data = json!({"verification_url":"https://www.google.com/device","user_code":"ABCD-EFGH","device_code":"private-device","expires_in":1800,"interval":2});
        let device = parse_device(&data, "client".into(), "secret".into()).unwrap();
        assert_eq!(device.interval, 5);
        assert!(!format!("{device:?}").contains("private"));
        data["verification_url"] = "https://evil.test/device".into();
        assert!(parse_device(&data, "client".into(), "secret".into()).is_err());
    }
    #[test]
    fn refresh_retains_refresh_token_and_errors_do_not_echo_secrets() {
        let session = token_session(
            &json!({"access_token":"new-access","token_type":"Bearer","expires_in":3600}),
            "client",
            "secret",
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(session.refresh_token, "old-refresh");
        assert!(!format!("{session:?}").contains("secret"));
        let error =
            oauth_error(&json!({"error":"unknown","error_description":"private-refresh-token"}));
        assert!(!error.to_string().contains("private"));
        assert!(matches!(
            oauth_error(&json!({"error":"invalid_grant"})),
            Error::AuthenticationRejected
        ));
    }
}
