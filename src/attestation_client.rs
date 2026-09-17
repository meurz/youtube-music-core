//! Per-client attestation storage; the host explicitly supplies freshly minted tokens.
use crate::{
    attestation::{session_binding, visitor_binding, PoTokenBundle, PoTokenContext},
    Error, MusicClient, Result,
};
impl MusicClient {
    /// Secret host descriptor for official-page attestation. Never log its result.
    pub fn attestation_context(&self, video_id: &str) -> Result<serde_json::Value> {
        crate::operation::ensure(|| {
            let script = crate::attestation::browser_script(video_id)?;
            let state = self.lock_session()?;
            let binding = session_binding(
                state
                    .cookie
                    .as_deref()
                    .ok_or(Error::AuthenticationRequired)?,
                self.config.auth_user,
                self.config.delegated_session_id.as_deref(),
            )?;
            Ok(
                serde_json::json!({"video_id":video_id,"script":script,"session_binding":binding,
                "auth_user":self.config.auth_user,"delegated_session_id":self.config.delegated_session_id,
                "max_expires_at":crate::session::now()?.saturating_add(3600)}),
            )
        })
    }

    /// Secret descriptor for a host's anonymous attestation provider.
    /// The visitor fingerprint protects local ownership; the provider independently
    /// follows the official protocol to select its token minting identifier.
    pub fn anonymous_attestation_context(&self, video_id: &str) -> Result<serde_json::Value> {
        crate::operation::ensure(|| {
            crate::client::validate_video_id(video_id)?;
            let state = self.lock_session()?;
            if state.cookie.is_some() {
                return Err(Error::InvalidInput(
                    "anonymous attestation requires a client without a browser session".into(),
                ));
            }
            let binding = self.po_binding(&state)?;
            Ok(serde_json::json!({
                "video_id": video_id,
                "client": "WEB_REMIX",
                "client_version": self.config.client_version,
                "visitor_data": self.config.visitor_data,
                "session_binding": binding,
                "binding_kind": "anonymous_visitor_v1",
                "max_expires_at": crate::session::now()?.saturating_add(3600)
            }))
        })
    }

    fn po_binding(&self, state: &crate::session::CookieState) -> Result<String> {
        match state.cookie.as_deref() {
            // Never downgrade an invalid, expired or malformed signing session.
            Some(cookie) => session_binding(
                cookie,
                self.config.auth_user,
                self.config.delegated_session_id.as_deref(),
            ),
            None => {
                if self.config.auth_user != 0 || self.config.delegated_session_id.is_some() {
                    return Err(Error::InvalidInput(
                        "anonymous attestation cannot select an authenticated account".into(),
                    ));
                }
                visitor_binding(self.config.visitor_data.as_deref().ok_or_else(|| {
                    Error::InvalidInput("anonymous attestation requires visitor data".into())
                })?)
            }
        }
    }

    /// Replace the in-memory token set. Empty input clears it. Never persists tokens.
    pub fn set_po_tokens(&self, tokens: Vec<PoTokenBundle>) -> Result<()> {
        crate::operation::ensure(|| {
            if tokens.len() > 16 {
                return Err(Error::InvalidInput(
                    "at most 16 PO token bundles are allowed".into(),
                ));
            }
            let state = self.lock_session()?;
            if !tokens.is_empty() {
                let binding = self.po_binding(&state)?;
                let now = crate::session::now()?
                    .try_into()
                    .map_err(|_| Error::Protocol("invalid system time".into()))?;
                let mut ids = std::collections::BTreeSet::new();
                for token in &tokens {
                    if !ids.insert(&token.video_id) {
                        return Err(Error::InvalidInput("duplicate PO token video ID".into()));
                    }
                    token.token_for(&token.video_id, PoTokenContext::Player, &binding, now)?;
                }
            }
            *crate::operation::lock(&self.po_tokens)? = tokens;
            drop(state);
            self.invalidate_playback()?;
            Ok(())
        })
    }
    pub(crate) fn po_token_for(
        &self,
        video_id: &str,
        context: PoTokenContext,
    ) -> Result<Option<String>> {
        Ok(self
            .po_token_with_expiry(video_id, context)?
            .map(|(token, _)| token))
    }
    pub(crate) fn po_token_with_expiry(
        &self,
        video_id: &str,
        context: PoTokenContext,
    ) -> Result<Option<(String, u64)>> {
        let state = self.lock_session()?;
        let tokens = crate::operation::lock(&self.po_tokens)?;
        if !tokens.iter().any(|t| t.video_id == video_id) {
            return Ok(None);
        }
        let binding = self.po_binding(&state)?;
        let now = crate::session::now()?
            .try_into()
            .map_err(|_| Error::Protocol("invalid system time".into()))?;
        for token in tokens.iter() {
            if let Some(value) = token.token_for(video_id, context, &binding, now)? {
                return Ok(Some((value.to_owned(), token.expires_at)));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn config() -> Config {
        Config {
            client_version: Some("offline.fixture".into()),
            visitor_data: Some("visitor-fixture-A".into()),
            ..Config::default()
        }
    }

    fn bundle(binding: String) -> PoTokenBundle {
        PoTokenBundle {
            video_id: "QoXDQa9L12A".into(),
            player_token: None,
            gvs_token: Some("g".repeat(100)),
            expires_at: crate::session::now().unwrap() as u64 + 300,
            session_binding: binding,
        }
    }

    #[test]
    fn anonymous_context_uses_exact_visitor_and_preserves_browser_requirement() {
        let client = MusicClient::new(config()).unwrap();
        let context = client.anonymous_attestation_context("QoXDQa9L12A").unwrap();
        assert_eq!(context["visitor_data"], "visitor-fixture-A");
        assert_eq!(context["client"], "WEB_REMIX");
        assert_eq!(context["binding_kind"], "anonymous_visitor_v1");
        assert_eq!(context["client_version"], "offline.fixture");
        assert_eq!(
            context["session_binding"],
            visitor_binding("visitor-fixture-A").unwrap()
        );
        assert!(context.get("script").is_none());
        assert!(matches!(
            client.attestation_context("QoXDQa9L12A"),
            Err(Error::AuthenticationRequired)
        ));
        assert!(client.anonymous_attestation_context("invalid").is_err());
    }

    #[test]
    fn anonymous_gvs_only_tokens_are_scoped_and_failed_replace_is_atomic() {
        let client = MusicClient::new(config()).unwrap();
        client
            .set_po_tokens(vec![bundle(visitor_binding("visitor-fixture-A").unwrap())])
            .unwrap();
        assert!(client
            .po_token_for("QoXDQa9L12A", PoTokenContext::Player)
            .unwrap()
            .is_none());
        assert_eq!(
            client
                .po_token_for("QoXDQa9L12A", PoTokenContext::Gvs)
                .unwrap(),
            Some("g".repeat(100))
        );
        assert!(client
            .po_token_for("4D7u5KF7SP8", PoTokenContext::Gvs)
            .unwrap()
            .is_none());
        assert!(client
            .set_po_tokens(vec![bundle(visitor_binding("visitor-fixture-B").unwrap())])
            .is_err());
        assert_eq!(
            client
                .po_token_for("QoXDQa9L12A", PoTokenContext::Gvs)
                .unwrap(),
            Some("g".repeat(100))
        );
        client.set_po_tokens(Vec::new()).unwrap();
        assert!(client
            .po_token_for("QoXDQa9L12A", PoTokenContext::Gvs)
            .unwrap()
            .is_none());
    }

    #[test]
    fn anonymous_config_tokens_require_explicit_matching_visitor_data() {
        let mut configured = config();
        configured.po_tokens = vec![bundle(visitor_binding("visitor-fixture-A").unwrap())];
        assert!(MusicClient::new(configured.clone()).is_ok());
        configured.visitor_data = Some("visitor-fixture-B".into());
        assert!(MusicClient::new(configured.clone()).is_err());
        configured.visitor_data = None;
        assert!(MusicClient::new(configured).is_err());
        let missing = MusicClient::new(Config {
            visitor_data: None,
            ..config()
        })
        .unwrap();
        assert!(missing
            .anonymous_attestation_context("QoXDQa9L12A")
            .is_err());
        missing.set_po_tokens(Vec::new()).unwrap();
    }

    #[test]
    fn present_invalid_cookie_never_downgrades_to_anonymous_proof() {
        for cookie in ["", "PREF=only", "SAPISID=one; SAPISID=two"] {
            let client = MusicClient::new(Config {
                cookie: Some(cookie.into()),
                ..config()
            })
            .unwrap();
            assert!(client.anonymous_attestation_context("QoXDQa9L12A").is_err());
            assert!(client
                .set_po_tokens(vec![bundle(visitor_binding("visitor-fixture-A").unwrap())])
                .is_err());
        }
    }

    #[test]
    fn anonymous_context_rejects_authenticated_account_selectors() {
        for configured in [
            Config {
                auth_user: 1,
                ..config()
            },
            Config {
                delegated_session_id: Some("channel-fixture".into()),
                ..config()
            },
        ] {
            let client = MusicClient::new(configured).unwrap();
            assert!(client.anonymous_attestation_context("QoXDQa9L12A").is_err());
            assert!(client
                .set_po_tokens(vec![bundle(visitor_binding("visitor-fixture-A").unwrap())])
                .is_err());
        }
    }

    #[test]
    fn authenticated_binding_still_rejects_guest_tokens_and_account_changes() {
        let cookie = "SAPISID=synthetic-session";
        let client = MusicClient::new(Config {
            cookie: Some(cookie.into()),
            auth_user: 1,
            ..config()
        })
        .unwrap();
        assert!(client.anonymous_attestation_context("QoXDQa9L12A").is_err());
        let context = client.attestation_context("QoXDQa9L12A").unwrap();
        let binding = session_binding(cookie, 1, None).unwrap();
        assert_eq!(context["session_binding"], binding);
        assert!(context["script"].as_str().unwrap().contains("LOGGED_IN"));
        client.set_po_tokens(vec![bundle(binding)]).unwrap();
        assert!(client
            .set_po_tokens(vec![bundle(visitor_binding("visitor-fixture-A").unwrap())])
            .is_err());
        assert!(client
            .set_po_tokens(vec![bundle(session_binding(cookie, 0, None).unwrap())])
            .is_err());
        client.lock_session().unwrap().cookie = Some("SAPISID=replaced-session".into());
        assert!(client
            .po_token_for("QoXDQa9L12A", PoTokenContext::Gvs)
            .is_err());
    }
}
