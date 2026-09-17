//! Optional official-browser Proof of Origin tokens, bound to one video and session.
//!
//! Token generation needs a real, signed-in official Music page. The embedded
//! player-transform sandbox is deliberately not presented as a BotGuard runtime.
use crate::{Error, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// Explicit request purpose prevents accidentally using a player token as a
/// media token. WEB_REMIX player tokens bind to the account Data Sync ID;
/// GVS binding follows the official player (currently the requested video).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoTokenContext {
    Player,
    Gvs,
}

/// Secret material. Serialize only into host-controlled secure storage.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PoTokenBundle {
    pub video_id: String,
    pub player_token: Option<String>,
    pub gvs_token: Option<String>,
    pub expires_at: u64,
    /// Hash of the signing session and account selection, never a raw cookie.
    pub session_binding: String,
}

impl std::fmt::Debug for PoTokenBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PoTokenBundle([REDACTED])")
    }
}
impl Drop for PoTokenBundle {
    fn drop(&mut self) {
        if let Some(token) = &mut self.player_token {
            token.zeroize();
        }
        if let Some(token) = &mut self.gvs_token {
            token.zeroize();
        }
        self.session_binding.zeroize();
    }
}

impl PoTokenBundle {
    pub fn validate(&self) -> Result<()> {
        crate::client::validate_video_id(&self.video_id)?;
        if self.expires_at == 0
            || self.session_binding.len() != 64
            || !self.session_binding.bytes().all(|b| b.is_ascii_hexdigit())
            || (self.player_token.is_none() && self.gvs_token.is_none())
        {
            return Err(invalid("invalid PO token metadata"));
        }
        for token in [&self.player_token, &self.gvs_token].into_iter().flatten() {
            if !(32..=8192).contains(&token.len())
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(invalid("invalid PO token encoding"));
            }
            let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token)
                .map_err(|_| invalid("invalid PO token encoding"))?;
            bytes.zeroize();
        }
        Ok(())
    }

    /// Select only a matching video, purpose and account. A token within 30
    /// seconds of expiry is unusable; callers must obtain fresh attestation.
    pub fn token_for(
        &self,
        video_id: &str,
        context: PoTokenContext,
        binding: &str,
        now: u64,
    ) -> Result<Option<&str>> {
        self.validate()?;
        if self.video_id != video_id {
            return Ok(None);
        }
        if self.session_binding != binding {
            return Err(invalid(
                "PO token belongs to a different browser session or account",
            ));
        }
        if self.expires_at <= now.saturating_add(30) {
            return Err(Error::PoTokenRequired);
        }
        if self.expires_at > now.saturating_add(3600) {
            return Err(invalid(
                "PO token expiry exceeds the one-hour retention limit",
            ));
        }
        Ok(match context {
            PoTokenContext::Player => self.player_token.as_deref(),
            PoTokenContext::Gvs => self.gvs_token.as_deref(),
        })
    }
}

/// Rotating ancillary cookies do not invalidate tokens; replacing the signing
/// session or switching the selected account does. The returned value is still
/// sensitive correlation data and must not be logged.
pub fn session_binding(
    cookie: &str,
    auth_user: u32,
    delegated_session_id: Option<&str>,
) -> Result<String> {
    let mut signing = crate::client::cookie_hash(cookie, 0).ok_or(Error::AuthenticationRequired)?;
    let mut hash = Sha256::new();
    hash.update(b"youtube-music-core/po-session/v1\0");
    for part in [
        signing.as_bytes(),
        &auth_user.to_be_bytes(),
        delegated_session_id.unwrap_or_default().as_bytes(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    signing.zeroize();
    Ok(format!("{:x}", hash.finalize()))
}

fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}

/// Official-page provider for CDP and WebView2 hosts. Execute the expression only
/// in an explicitly selected https://music.youtube.com page. It returns secret
/// tokens plus a conservative expiry; the host attaches `session_binding` after
/// checking that the account did not change during execution.
pub fn browser_script(video_id: &str) -> Result<String> {
    crate::client::validate_video_id(video_id)?;
    Ok(format!(
        "({})({})",
        include_str!("attestation_browser.js"),
        serde_json::to_string(video_id).map_err(|_| invalid("invalid video ID"))?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bundle() -> PoTokenBundle {
        PoTokenBundle {
            video_id: "4D7u5KF7SP8".into(),
            player_token: Some("p".repeat(100)),
            gvs_token: Some("g".repeat(100)),
            expires_at: 1000,
            session_binding: session_binding("SAPISID=synthetic; SIDCC=old", 0, None).unwrap(),
        }
    }
    #[test]
    fn tokens_are_scoped_to_video_purpose_session_and_expiry() {
        let b = bundle();
        assert_eq!(
            b.token_for(&b.video_id, PoTokenContext::Player, &b.session_binding, 100)
                .unwrap(),
            b.player_token.as_deref()
        );
        assert_eq!(
            b.token_for(&b.video_id, PoTokenContext::Gvs, &b.session_binding, 100)
                .unwrap(),
            b.gvs_token.as_deref()
        );
        assert!(b
            .token_for("5NV6Rdv1a3I", PoTokenContext::Gvs, &b.session_binding, 100)
            .unwrap()
            .is_none());
        assert!(b
            .token_for(&b.video_id, PoTokenContext::Gvs, "other", 100)
            .is_err());
        assert!(b
            .token_for(&b.video_id, PoTokenContext::Gvs, &b.session_binding, 970)
            .is_err());
        let mut future = bundle();
        future.expires_at = 4000;
        assert!(future
            .token_for(
                &future.video_id,
                PoTokenContext::Gvs,
                &future.session_binding,
                100
            )
            .is_err());
    }
    #[test]
    fn session_binding_survives_rotation_but_not_account_changes() {
        let a = session_binding("SAPISID=synthetic; SIDCC=old", 0, None).unwrap();
        assert_eq!(
            a,
            session_binding("SIDCC=new; SAPISID=synthetic", 0, None).unwrap()
        );
        assert_ne!(a, session_binding("SAPISID=replaced", 0, None).unwrap());
        assert_ne!(a, session_binding("SAPISID=synthetic", 1, None).unwrap());
        assert_ne!(
            a,
            session_binding("SAPISID=synthetic", 0, Some("channel")).unwrap()
        );
        assert!(session_binding("SAPISID=one; SAPISID=two", 0, None).is_err());
        assert!(!format!("{:?}", bundle()).contains("pppp"));
    }
    #[test]
    fn malformed_tokens_and_script_arguments_are_rejected() {
        let mut b = bundle();
        b.gvs_token = Some("contains space".into());
        assert!(b.validate().is_err());
        assert!(browser_script("\";malicious()").is_err());
        assert!(browser_script("4D7u5KF7SP8")
            .unwrap()
            .contains("music.youtube.com"));
    }
}
