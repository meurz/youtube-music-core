//! Cookies are sent only to the fixed Music origin. No browser-wide cookie jar.
use crate::{auth::BrowserSession, Config, Error, Result};
use cookie::Cookie;
use reqwest::{header::HeaderMap, Url};
use std::collections::BTreeMap;

const MAX_COOKIES: usize = 512;
const MAX_COOKIE_BYTES: usize = 65_536;

pub(crate) struct CookieState {
    pub cookie: Option<String>,
    pub expirations: BTreeMap<String, i64>,
    pub verified: bool,
}

pub(crate) fn now() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .map_err(|_| Error::Protocol("system clock before Unix epoch".into()))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

pub(crate) fn validate_expirations(expirations: &BTreeMap<String, i64>) -> Result<()> {
    if expirations.len() > MAX_COOKIES || expirations.keys().any(|name| !valid_name(name)) {
        return Err(Error::InvalidInput(
            "invalid cookie expiration metadata".into(),
        ));
    }
    Ok(())
}

impl CookieState {
    pub fn new(cookie: Option<String>, expirations: BTreeMap<String, i64>) -> Result<Self> {
        validate_expirations(&expirations)?;
        if cookie.as_ref().is_some_and(|c| c.len() > MAX_COOKIE_BYTES) {
            return Err(Error::InvalidInput(
                "browser cookies exceed size limit".into(),
            ));
        }
        Ok(Self {
            cookie,
            expirations,
            verified: false,
        })
    }

    pub fn snapshot(&self, config: &Config) -> Result<Option<BrowserSession>> {
        self.cookie
            .as_ref()
            .map(|cookie| {
                let session = BrowserSession {
                    cookie: cookie.clone(),
                    cookie_expirations: self.expirations.clone(),
                    auth_user: config.auth_user,
                    delegated_session_id: config.delegated_session_id.clone(),
                };
                session.validate()?;
                Ok(session)
            })
            .transpose()
    }

    pub fn purge(&mut self, now: i64) {
        let expired: Vec<_> = self
            .expirations
            .iter()
            .filter(|(_, expiry)| **expiry <= now)
            .map(|(name, _)| name.clone())
            .collect();
        if expired.is_empty() {
            return;
        }
        if let Some(cookie) = &mut self.cookie {
            *cookie = cookie
                .split(';')
                .filter(|part| {
                    part.trim()
                        .split_once('=')
                        .is_none_or(|(name, _)| !expired.iter().any(|e| e == name))
                })
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("; ");
        }
        for name in expired {
            self.expirations.remove(&name);
        }
        self.verified = false;
    }

    /// Only root-scoped first-party Music cookies can be represented by the
    /// imported request-header format. Narrow-path/partitioned cookies are ignored.
    pub fn observe(&mut self, url: &Url, headers: &HeaderMap, now: i64) -> Result<()> {
        if self.cookie.is_none()
            || url.scheme() != "https"
            || url.host_str() != Some("music.youtube.com")
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Ok(());
        }
        let mut pairs: Vec<(String, String)> = self
            .cookie
            .as_deref()
            .unwrap_or_default()
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        let mut expirations = self.expirations.clone();
        let mut changed = false;
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok(raw) = value.to_str() else {
                continue;
            };
            if raw.len() > MAX_COOKIE_BYTES {
                continue;
            }
            let Ok(cookie) = Cookie::parse(raw) else {
                continue;
            };
            let name = cookie.name();
            if !valid_name(name)
                || !cookie.value().bytes().all(|b| {
                    b == 0x21
                        || (0x23..=0x2b).contains(&b)
                        || (0x2d..=0x3a).contains(&b)
                        || (0x3c..=0x5b).contains(&b)
                        || (0x5d..=0x7e).contains(&b)
                })
            {
                continue;
            }
            if cookie.domain().is_some_and(|domain| {
                let domain = domain.strip_prefix('.').unwrap_or(domain);
                !domain.eq_ignore_ascii_case("youtube.com")
                    && !domain.eq_ignore_ascii_case("music.youtube.com")
            }) {
                continue;
            }
            let root_path = cookie
                .path()
                .map(|p| p == "/")
                .unwrap_or_else(|| url.path().rfind('/').is_none_or(|i| i == 0));
            if !root_path || cookie.partitioned() == Some(true) {
                continue;
            }
            if (name.starts_with("__Secure-")
                || name.starts_with("__Host-")
                || name.starts_with("__Http-"))
                && cookie.secure() != Some(true)
            {
                continue;
            }
            if name.starts_with("__Host-")
                && (cookie.domain().is_some() || cookie.path() != Some("/"))
            {
                continue;
            }
            if (name.starts_with("__Http-") || name.starts_with("__Host-Http-"))
                && cookie.http_only() != Some(true)
            {
                continue;
            }
            let expiry = cookie
                .max_age()
                .map(|age| now.saturating_add(age.whole_seconds()))
                .or_else(|| cookie.expires_datetime().map(|date| date.unix_timestamp()));
            let delete = expiry.is_some_and(|e| e <= now);
            let existing: Vec<_> = pairs.iter().filter(|(key, _)| key == name).collect();
            // Imported headers omit each cookie's original domain/partition.
            // Do not guess which same-name cookie a response would replace.
            if existing.len() > 1 {
                continue;
            }
            if (!delete && (existing.len() != 1 || existing[0].1 != cookie.value()))
                || (delete && !existing.is_empty())
                || expirations.get(name).copied() != expiry.filter(|_| !delete)
            {
                changed = true;
            }
            pairs.retain(|(key, _)| key != name);
            expirations.remove(name);
            if !delete {
                pairs.push((name.to_owned(), cookie.value().to_owned()));
                if let Some(expiry) = expiry {
                    expirations.insert(name.to_owned(), expiry);
                }
            }
        }
        if changed {
            let cookie = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            if pairs.len() > MAX_COOKIES || cookie.len() > MAX_COOKIE_BYTES {
                return Err(Error::Protocol(
                    "updated browser cookies exceed size limit".into(),
                ));
            }
            self.cookie = Some(cookie);
            self.expirations = expirations;
            self.verified = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> CookieState {
        CookieState::new(
            Some("SAPISID=original; SIDCC=old; PREF=one; PREF=two".into()),
            BTreeMap::new(),
        )
        .unwrap()
    }
    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(reqwest::header::SET_COOKIE, value.parse().unwrap());
        }
        headers
    }
    #[test]
    fn rotations_deletions_and_expiry_survive_a_snapshot_round_trip() {
        let mut state = state();
        state.verified = true;
        state
            .observe(
                &Url::parse("https://music.youtube.com/").unwrap(),
                &headers(&[
                    "SAPISID=rotated; Domain=.youtube.com; Path=/; Secure; HttpOnly; Max-Age=60",
                    "SIDCC=; Path=/; Max-Age=0; Expires=Wed, 01 Jan 2098 00:00:00 GMT",
                    "PAST=removed; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
                    "PREF=ambiguous-update; Path=/",
                ]),
                100,
            )
            .unwrap();
        assert!(!state.verified);
        let snapshot = state.snapshot(&Config::default()).unwrap().unwrap();
        assert_eq!(snapshot.cookie_expirations["SAPISID"], 160);
        assert!(snapshot.cookie.contains("SAPISID=rotated"));
        assert!(snapshot.cookie.contains("PREF=one; PREF=two"));
        assert!(!snapshot.cookie.contains("SIDCC="));
        assert!(!snapshot.cookie.contains("PAST="));
        let mut restored =
            CookieState::new(Some(snapshot.cookie), snapshot.cookie_expirations).unwrap();
        restored.purge(160);
        assert!(!restored.cookie.as_ref().unwrap().contains("SAPISID="));
        assert!(restored.snapshot(&Config::default()).is_err());
    }

    #[test]
    fn foreign_sources_scopes_prefix_violations_and_partitioned_updates_are_ignored() {
        let updates = headers(&["SAPISID=attacker; Path=/; Secure"]);
        let mut state = state();
        let initial = state.cookie.clone();
        for url in [
            "https://accounts.google.com/",
            "https://www.youtube.com/",
            "https://music.youtube.com.evil.test/",
            "http://music.youtube.com/",
            "https://music.youtube.com:444/",
            "https://user@music.youtube.com/",
        ] {
            state
                .observe(&Url::parse(url).unwrap(), &updates, 100)
                .unwrap();
            assert_eq!(state.cookie, initial);
        }
        state
            .observe(
                &Url::parse("https://music.youtube.com/youtubei/v1/browse").unwrap(),
                &headers(&[
                    "SAPISID=foreign; Domain=google.com; Path=/",
                    "SAPISID=publicsuffix; Domain=com; Path=/",
                    "SAPISID=narrow; Path=/youtubei",
                    "SAPISID=default-narrow",
                    "SAPISID=partition; Secure; Partitioned; Path=/",
                    "__Secure-SID=bad; Path=/",
                    "__Host-SID=bad; Secure; Domain=youtube.com; Path=/",
                    "__Host-SID=bad; Secure",
                    "__Http-SID=bad; Secure; Path=/",
                    "__Host-Http-SID=bad; Secure; Path=/",
                ]),
                100,
            )
            .unwrap();
        assert_eq!(state.cookie, initial);
        let mut anonymous = CookieState::new(None, BTreeMap::new()).unwrap();
        anonymous
            .observe(
                &Url::parse("https://music.youtube.com/").unwrap(),
                &updates,
                100,
            )
            .unwrap();
        assert!(anonymous.cookie.is_none());
    }

    #[test]
    fn root_default_path_valid_prefixes_and_max_age_precedence_work() {
        let mut state = state();
        state
            .observe(
                &Url::parse("https://music.youtube.com/watch?v=test").unwrap(),
                &headers(&[
                    "SIDCC=fresh; Max-Age=120; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
                    "__Host-Http-Token=good; Path=/; Secure; HttpOnly",
                    "__Secure-Token=good; Domain=.YOUTUBE.COM; Path=/; Secure",
                ]),
                100,
            )
            .unwrap();
        assert!(state.cookie.as_ref().unwrap().contains("SIDCC=fresh"));
        assert_eq!(state.expirations["SIDCC"], 220);
        assert!(state
            .cookie
            .as_ref()
            .unwrap()
            .contains("__Host-Http-Token=good"));
        assert!(state
            .cookie
            .as_ref()
            .unwrap()
            .contains("__Secure-Token=good"));
    }

    #[test]
    fn oversized_response_is_rejected_without_partial_mutation() {
        let mut state = state();
        let initial = state.cookie.clone();
        let mut updates = headers(&["SIDCC=changed; Path=/"]);
        for i in 0..513 {
            updates.append(
                reqwest::header::SET_COOKIE,
                format!("C{i}=v; Path=/").parse().unwrap(),
            );
        }
        assert!(state
            .observe(
                &Url::parse("https://music.youtube.com/").unwrap(),
                &updates,
                100
            )
            .is_err());
        assert_eq!(state.cookie, initial);
    }
}
