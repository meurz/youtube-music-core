//! Runtime and session boundary around the pinned RustyPipe implementation.
//!
//! RustyPipe keeps only transient caches here. The host remains responsible for
//! validating, refreshing and securely persisting the original browser session.

pub(crate) mod music;
pub(crate) mod player;

use crate::{operation, Config, Error, MusicClient, Result};
use rustypipe::{
    client::{RustyPipe, RustyPipeQuery},
    error::{AuthError, Error as UpstreamError, ExtractionError, UnavailabilityReason},
    param::{Country, Language},
};
use std::{
    future::Future,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

/// Each cookie/identity generation owns a separate upstream client. A query
/// already in flight can never start using a subsequently selected account.
pub(crate) struct Backend {
    proxy: Option<String>,
    timeout: Duration,
    configured_visitor: Option<String>,
    state: Mutex<Option<SessionBackend>>,
}

struct SessionBackend {
    key: SessionKey,
    client: RustyPipe,
}

// Secret-bearing state deliberately has no Debug implementation.
#[derive(PartialEq, Eq)]
struct SessionKey {
    cookie: Option<String>,
    auth_user: u32,
    delegated_session_id: Option<String>,
    language: Language,
    country: Country,
    visitor_data: Option<String>,
    client_version: Option<String>,
}

impl Backend {
    /// Validate local configuration without reading files or contacting YouTube.
    pub(crate) fn new(config: &Config) -> Result<Self> {
        validate_options(config)?;
        Ok(Self {
            proxy: config.proxy.clone(),
            timeout: Duration::from_secs(config.timeout_seconds),
            configured_visitor: config.visitor_data.clone(),
            state: Mutex::new(None),
        })
    }

    pub(crate) fn current_binding(&self, owner: &MusicClient) -> Result<String> {
        let config = &owner.config;
        if let Some(session) = owner.lock_session()?.snapshot(config)? {
            return crate::attestation::session_binding(
                &session.cookie,
                session.auth_user,
                session.delegated_session_id.as_deref(),
            );
        }
        if config.auth_user != 0 || config.delegated_session_id.is_some() {
            return Err(Error::AuthenticationRequired);
        }
        self.configured_visitor
            .as_deref()
            .map(crate::attestation::visitor_binding)
            // CLI pagination creates a new client in each process. The cursor
            // carries its upstream visitor; no configured identity means the
            // same anonymous scope, not a newly randomized account binding.
            .unwrap_or_else(|| Ok("anonymous".into()))
    }

    pub(crate) fn query(&self, owner: &MusicClient) -> Result<RustyPipeQuery> {
        operation::check()?;
        let key = SessionKey::read(owner)?;
        let mut state = operation::lock(&self.state)?;
        if state.as_ref().is_none_or(|state| state.key != key) {
            let mut http = reqwest::Client::builder();
            if let Some(proxy) = &self.proxy {
                http = http.proxy(
                    reqwest::Proxy::all(proxy)
                        .map_err(|_| Error::InvalidInput("invalid proxy URL".into()))?,
                );
            }
            let client = RustyPipe::builder()
                .no_storage()
                .no_reporter()
                .no_botguard()
                .unauthenticated()
                .timeout(self.timeout)
                .n_http_retries(1)
                .lang(key.language)
                .country(key.country)
                .visitor_data_opt(key.visitor_data.clone())
                .response_observer(session_observer(owner, key.cookie.clone()))
                .build_with_client(http)
                .map_err(map_error)?;
            if let Some(cookie) = &key.cookie {
                client
                    .set_web_session(
                        signing_cookie(cookie),
                        key.auth_user,
                        key.delegated_session_id.clone(),
                    )
                    .map_err(map_error)?;
            }
            if let Some(version) = &key.client_version {
                run(client.set_music_client_version(version.clone()))?;
            }
            operation::check()?;
            *state = Some(SessionBackend { key, client });
        }
        let state = state.as_ref().expect("upstream session initialized");
        Ok(if state.key.cookie.is_some() {
            state.client.query().authenticated()
        } else {
            state.client.query().unauthenticated()
        })
    }
}

impl SessionKey {
    fn read(owner: &MusicClient) -> Result<Self> {
        let config = &owner.config;
        let session = owner.lock_session()?.snapshot(config)?;
        if session.is_none() && (config.auth_user != 0 || config.delegated_session_id.is_some()) {
            return Err(Error::AuthenticationRequired);
        }
        let cookie = session.map(|session| session.cookie);
        Ok(Self {
            cookie,
            auth_user: config.auth_user,
            delegated_session_id: config.delegated_session_id.clone(),
            language: parse_language(&config.language)?,
            country: parse_country(&config.country)?,
            visitor_data: config.visitor_data.clone(),
            client_version: config.client_version.clone(),
        })
    }
}

struct SessionObserver {
    state: Weak<Mutex<crate::session::CookieState>>,
    original_cookie: Option<String>,
    outgoing_cookie: Option<String>,
}

fn session_observer(
    owner: &MusicClient,
    original_cookie: Option<String>,
) -> rustypipe::client::ResponseObserver {
    let observer = SessionObserver {
        state: Arc::downgrade(&owner.session),
        outgoing_cookie: original_cookie.as_deref().map(signing_cookie),
        original_cookie,
    };
    Arc::new(move |request, response| {
        observer
            .observe(
                request,
                response.url(),
                response.status(),
                response.headers(),
            )
            .map_err(|_| UpstreamError::Other("browser session update failed".into()))
    })
}

impl SessionObserver {
    fn observe(
        &self,
        request: &reqwest::Request,
        response_url: &reqwest::Url,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<()> {
        fn music_origin(url: &reqwest::Url) -> bool {
            url.scheme() == "https"
                && url.host_str() == Some("music.youtube.com")
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
        }
        let Some(expected) = self.outgoing_cookie.as_deref() else {
            return Ok(());
        };
        if !music_origin(request.url())
            || !music_origin(response_url)
            || request
                .headers()
                .get(reqwest::header::COOKIE)
                .map(|h| h.as_bytes())
                != Some(expected.as_bytes())
        {
            return Ok(());
        }
        let Some(state) = self.state.upgrade() else {
            return Ok(());
        };
        let mut state = operation::lock(&state)?;
        let now = crate::session::now()?;
        state.purge(now);
        // Ignore responses from queries started before rotation, replacement or
        // sign-out. Upstream authentication is immutable within each generation.
        if state.cookie != self.original_cookie {
            return Ok(());
        }
        if matches!(status.as_u16(), 401 | 403) {
            state.verified = false;
        }
        let mut bytes = 0usize;
        for (index, value) in headers
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .enumerate()
        {
            bytes = bytes.saturating_add(value.as_bytes().len());
            if index >= 512 || bytes > 65_536 {
                return Err(Error::Protocol("response cookies exceed size limit".into()));
            }
        }
        state.observe(response_url, headers, now)
    }
}

fn validate_options(config: &Config) -> Result<()> {
    if !(1..=300).contains(&config.timeout_seconds) {
        return Err(Error::InvalidInput(
            "timeout_seconds must be between 1 and 300".into(),
        ));
    }
    if config.auth_user > 99 {
        return Err(Error::InvalidInput(
            "auth_user must be between 0 and 99".into(),
        ));
    }
    if config.delegated_session_id.as_ref().is_some_and(|id| {
        id.is_empty()
            || id.len() > 256
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) {
        return Err(Error::InvalidInput("invalid delegated session ID".into()));
    }
    parse_language(&config.language)?;
    parse_country(&config.country)?;
    if config.client_version.as_ref().is_some_and(|version| {
        version.is_empty()
            || version.len() > 128
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }) {
        return Err(Error::InvalidInput("invalid Music client version".into()));
    }
    if let Some(visitor) = &config.visitor_data {
        crate::attestation::visitor_binding(visitor)?;
    }
    if let Some(proxy) = &config.proxy {
        reqwest::Proxy::all(proxy).map_err(|_| Error::InvalidInput("invalid proxy URL".into()))?;
    }
    Ok(())
}

fn parse_language(language: &str) -> Result<Language> {
    language
        .parse()
        .map_err(|_| Error::InvalidInput("unsupported content language".into()))
}

fn parse_country(country: &str) -> Result<Country> {
    country
        .parse()
        .map_err(|_| Error::InvalidInput("unsupported content country".into()))
}

/// RustyPipe signs SAPISID; the host also accepts its secure aliases. Normalize
/// only the transient upstream copy after BrowserSession has checked ambiguity.
fn signing_cookie(cookie: &str) -> String {
    let pairs: Vec<_> = cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .collect();
    if pairs
        .iter()
        .any(|(name, value)| *name == "SAPISID" && !value.is_empty())
    {
        return cookie.into();
    }
    let alias = ["__Secure-3PAPISID", "__Secure-1PAPISID"]
        .iter()
        .find_map(|name| {
            pairs
                .iter()
                .find(|(key, value)| key == name && !value.is_empty())
                .map(|(_, value)| *value)
        });
    match alias {
        Some(value) => {
            let mut normalized = pairs
                .iter()
                .filter(|(name, _)| *name != "SAPISID")
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>();
            normalized.push(format!("SAPISID={value}"));
            normalized.join("; ")
        }
        None => cookie.into(),
    }
}

fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| Error::Protocol("cannot initialize upstream runtime".into()))
}

/// Poll the request on the calling blocking worker. Cancellation drops the
/// pending future before returning; no detached operation is spawned.
pub(crate) fn run<T>(
    future: impl Future<Output = std::result::Result<T, UpstreamError>>,
) -> Result<T> {
    run_core(async { future.await.map_err(map_error) })
}

pub(crate) fn run_core<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    operation::ensure(|| {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(Error::InvalidInput(
                "call the blocking core from a blocking worker".into(),
            ));
        }
        let context = operation::current().expect("operation context installed");
        operation::phase("upstream");
        let result = runtime()?.block_on(async {
            tokio::select! {
                biased;
                error = async {
                    loop {
                        if let Err(error) = context.check() { break error; }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => Err(error),
                result = future => result,
            }
        });
        context.check()?;
        result
    })
}

/// Upstream errors may contain signed URLs, cookies or raw response snippets.
/// Preserve useful categories without forwarding uncontrolled error strings.
fn map_error(error: UpstreamError) -> Error {
    match error {
        UpstreamError::Auth(AuthError::NoLogin) => Error::AuthenticationRequired,
        UpstreamError::Auth(_) => Error::AuthenticationRejected,
        UpstreamError::HttpStatus(401, _) => Error::AuthenticationRejected,
        UpstreamError::HttpStatus(429, _) => Error::RateLimited {
            retry_after_seconds: None,
        },
        UpstreamError::HttpStatus(status, _) => Error::Http(status),
        UpstreamError::Http(message)
            if message.contains("timed out") || message.contains("timeout") =>
        {
            Error::Timeout
        }
        UpstreamError::Http(_) => Error::Network("upstream transport failed".into()),
        UpstreamError::Extraction(ExtractionError::Botguard(_)) => Error::PoTokenRequired,
        UpstreamError::Extraction(ExtractionError::Deobfuscation(_)) => {
            Error::StreamResolutionRequired
        }
        UpstreamError::Extraction(ExtractionError::BadRequest(_)) => {
            Error::InvalidInput("upstream rejected request parameters".into())
        }
        UpstreamError::Extraction(ExtractionError::Unavailable {
            reason: UnavailabilityReason::TryAgain,
            ..
        }) => Error::RateLimited {
            retry_after_seconds: None,
        },
        UpstreamError::Extraction(ExtractionError::Unavailable { reason, .. }) => {
            Error::Unplayable {
                status: "UNAVAILABLE".into(),
                reason: reason.to_string(),
            }
        }
        UpstreamError::Extraction(ExtractionError::NotFound { .. }) => {
            Error::Protocol("requested resource was not found".into())
        }
        UpstreamError::Extraction(_) | UpstreamError::Other(_) => {
            Error::Protocol("upstream response could not be processed".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{OperationContext, OperationOptions};
    use rustypipe::client::ClientType;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn anonymous_pagination_binding_survives_client_recreation() {
        let config = Config {
            client_version: Some("fixture".into()),
            ..Default::default()
        };
        let mut first = MusicClient::new(config.clone()).unwrap();
        let mut second = MusicClient::new(config.clone()).unwrap();
        first.config.visitor_data = Some("bootstrap-one".into());
        second.config.visitor_data = Some("bootstrap-two".into());
        let bound = MusicClient::new(Config {
            visitor_data: Some("configured-visitor".into()),
            ..config
        })
        .unwrap();
        let binding = first.upstream.current_binding(&first).unwrap();
        assert_eq!(binding, second.upstream.current_binding(&second).unwrap());
        assert_ne!(binding, bound.upstream.current_binding(&bound).unwrap());
    }

    #[test]
    fn cancellation_drops_pending_work_before_returning() {
        struct Pending(Arc<AtomicBool>, Arc<AtomicBool>);
        impl Future for Pending {
            type Output = std::result::Result<(), UpstreamError>;
            fn poll(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                self.0.store(true, Ordering::Release);
                std::task::Poll::Pending
            }
        }
        impl Drop for Pending {
            fn drop(&mut self) {
                self.1.store(true, Ordering::Release);
            }
        }
        let context = OperationContext::new(OperationOptions::default()).unwrap();
        let cancel = context.clone();
        let polled = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let other = polled.clone();
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !other.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            cancel.cancel();
        });
        assert!(matches!(
            context.run(|| run(Pending(polled, dropped.clone()))),
            Err(Error::Cancelled)
        ));
        worker.join().unwrap();
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn timeout_bounds_pending_work() {
        let context = OperationContext::new(OperationOptions { timeout_ms: 15 }).unwrap();
        assert!(matches!(
            context.run(|| run(std::future::pending::<std::result::Result<(), UpstreamError>>())),
            Err(Error::Timeout)
        ));
    }

    #[test]
    fn sessions_remain_isolated_after_cookie_replacement_and_signout() {
        let owner = MusicClient::new(Config {
            client_version: Some("test-version".into()),
            visitor_data: Some("test-visitor".into()),
            cookie: Some("SAPISID=first-test-value".into()),
            ..Config::default()
        })
        .unwrap();
        let backend = Backend::new(&owner.config).unwrap();
        let first = backend.query(&owner).unwrap();
        assert!(first.auth_enabled(ClientType::DesktopMusic));
        owner.lock_session().unwrap().cookie = Some("SAPISID=second-test-value".into());
        let second = backend.query(&owner).unwrap();
        assert!(second.auth_enabled(ClientType::DesktopMusic));
        owner.lock_session().unwrap().cookie = None;
        let anonymous = backend.query(&owner).unwrap();
        assert!(!anonymous.auth_enabled(ClientType::DesktopMusic));
        assert!(first.auth_enabled(ClientType::DesktopMusic));
        assert!(second.auth_enabled(ClientType::DesktopMusic));
        assert!(!first.auth_enabled(ClientType::Tv));
        owner.lock_session().unwrap().cookie = Some("PREF=invalid-session".into());
        assert!(matches!(
            backend.query(&owner),
            Err(Error::AuthenticationRequired)
        ));
        assert!(matches!(
            backend.current_binding(&owner),
            Err(Error::AuthenticationRequired)
        ));
    }

    #[test]
    fn local_configuration_validation_does_not_silently_change_locale_or_identity() {
        assert!(Backend::new(&Config {
            language: "not-a-language".into(),
            ..Config::default()
        })
        .is_err());
        assert!(Backend::new(&Config {
            country: "NOT-A-COUNTRY".into(),
            ..Config::default()
        })
        .is_err());
        assert!(Backend::new(&Config {
            auth_user: 100,
            ..Config::default()
        })
        .is_err());
        assert!(Backend::new(&Config {
            delegated_session_id: Some("invalid/channel".into()),
            ..Config::default()
        })
        .is_err());
        assert!(Backend::new(&Config {
            language: "zh-TW".into(),
            country: "TW".into(),
            ..Config::default()
        })
        .is_ok());
    }

    #[test]
    fn secure_signing_alias_is_only_normalized_in_the_upstream_copy() {
        assert_eq!(
            signing_cookie("__Secure-3PAPISID=test-alias; PREF=x"),
            "__Secure-3PAPISID=test-alias; PREF=x; SAPISID=test-alias"
        );
        assert_eq!(
            signing_cookie("SAPISID=test-primary; __Secure-3PAPISID=test-alias"),
            "SAPISID=test-primary; __Secure-3PAPISID=test-alias"
        );
    }

    fn observer_fixture() -> (
        Arc<Mutex<crate::session::CookieState>>,
        SessionObserver,
        reqwest::Request,
        reqwest::header::HeaderMap,
    ) {
        let cookie = "__Secure-3PAPISID=test-alias; SIDCC=old".to_owned();
        let state = Arc::new(Mutex::new(
            crate::session::CookieState::new(Some(cookie.clone()), Default::default()).unwrap(),
        ));
        let outgoing = signing_cookie(&cookie);
        let observer = SessionObserver {
            state: Arc::downgrade(&state),
            original_cookie: Some(cookie),
            outgoing_cookie: Some(outgoing.clone()),
        };
        let mut request = reqwest::Request::new(
            reqwest::Method::POST,
            "https://music.youtube.com/youtubei/v1/browse"
                .parse()
                .unwrap(),
        );
        request
            .headers_mut()
            .insert(reqwest::header::COOKIE, outgoing.parse().unwrap());
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::SET_COOKIE,
            "SIDCC=rotated; Path=/; Secure; Max-Age=3600"
                .parse()
                .unwrap(),
        );
        (state, observer, request, headers)
    }

    #[test]
    fn response_cookie_rotation_preserves_the_original_cookie_shape_and_expiry() {
        let (state, observer, request, headers) = observer_fixture();
        state.lock().unwrap().verified = true;
        let before = crate::session::now().unwrap();
        observer
            .observe(&request, request.url(), reqwest::StatusCode::OK, &headers)
            .unwrap();
        let state = state.lock().unwrap();
        assert_eq!(
            state.cookie.as_deref(),
            Some("__Secure-3PAPISID=test-alias; SIDCC=rotated")
        );
        assert!(state.expirations["SIDCC"] >= before + 3600);
        assert!(!state.verified);
    }

    #[test]
    fn response_observer_ignores_stale_sessions_signout_and_expired_cookies() {
        for replacement in [Some("SAPISID=replacement".to_owned()), None] {
            let (state, observer, request, headers) = observer_fixture();
            state.lock().unwrap().cookie = replacement.clone();
            observer
                .observe(&request, request.url(), reqwest::StatusCode::OK, &headers)
                .unwrap();
            assert_eq!(state.lock().unwrap().cookie, replacement);
        }
        let (state, observer, request, headers) = observer_fixture();
        state
            .lock()
            .unwrap()
            .expirations
            .insert("__Secure-3PAPISID".into(), 0);
        observer
            .observe(&request, request.url(), reqwest::StatusCode::OK, &headers)
            .unwrap();
        assert_eq!(state.lock().unwrap().cookie.as_deref(), Some("SIDCC=old"));
    }

    #[test]
    fn response_observer_rejects_foreign_origins_and_anonymous_requests() {
        let (state, observer, mut request, headers) = observer_fixture();
        for foreign in [
            "https://www.youtube.com/",
            "http://music.youtube.com/",
            "https://music.youtube.com:444/",
            "https://music.youtube.com.evil.example/",
            "https://user@music.youtube.com/",
        ] {
            let foreign = foreign.parse().unwrap();
            observer
                .observe(&request, &foreign, reqwest::StatusCode::OK, &headers)
                .unwrap();
            let mut other = request.try_clone().unwrap();
            *other.url_mut() = foreign;
            observer
                .observe(&other, request.url(), reqwest::StatusCode::OK, &headers)
                .unwrap();
        }
        request.headers_mut().remove(reqwest::header::COOKIE);
        observer
            .observe(&request, request.url(), reqwest::StatusCode::OK, &headers)
            .unwrap();
        assert_eq!(state.lock().unwrap().cookie, observer.original_cookie);
    }

    #[test]
    fn response_observer_does_not_keep_the_owner_alive_and_bounds_header_work() {
        let (state, observer, request, mut headers) = observer_fixture();
        assert_eq!(Arc::strong_count(&state), 1);
        for _ in 0..512 {
            headers.append(
                reqwest::header::SET_COOKIE,
                "PREF=x; Path=/".parse().unwrap(),
            );
        }
        assert!(observer
            .observe(&request, request.url(), reqwest::StatusCode::OK, &headers)
            .is_err());
        assert_eq!(state.lock().unwrap().cookie, observer.original_cookie);
        state.lock().unwrap().verified = true;
        observer
            .observe(
                &request,
                request.url(),
                reqwest::StatusCode::UNAUTHORIZED,
                &Default::default(),
            )
            .unwrap();
        assert!(!state.lock().unwrap().verified);
        drop(state);
        assert!(observer.state.upgrade().is_none());
        observer
            .observe(&request, request.url(), reqwest::StatusCode::OK, &headers)
            .unwrap();
    }

    #[test]
    fn upstream_error_details_never_escape() {
        let marker = "test-sensitive-content";
        for error in [
            UpstreamError::Http(marker.into()),
            UpstreamError::HttpStatus(403, marker.into()),
            UpstreamError::Extraction(ExtractionError::InvalidData(marker.into())),
            UpstreamError::Auth(AuthError::Other(marker.into())),
            UpstreamError::Extraction(ExtractionError::Unavailable {
                reason: UnavailabilityReason::Private,
                msg: marker.into(),
            }),
        ] {
            assert!(!map_error(error).to_string().contains(marker));
        }
    }
}
