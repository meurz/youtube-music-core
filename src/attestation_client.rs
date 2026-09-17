//! Per-client attestation storage; the host explicitly supplies freshly minted tokens.
use crate::{
    attestation::{session_binding, PoTokenBundle, PoTokenContext},
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
                let binding = session_binding(
                    state
                        .cookie
                        .as_deref()
                        .ok_or(Error::AuthenticationRequired)?,
                    self.config.auth_user,
                    self.config.delegated_session_id.as_deref(),
                )?;
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
        let binding = session_binding(
            state
                .cookie
                .as_deref()
                .ok_or(Error::AuthenticationRequired)?,
            self.config.auth_user,
            self.config.delegated_session_id.as_deref(),
        )?;
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
