//! Thin playback adapter; RustyPipe owns player discovery and URL transforms.
use std::sync::Arc;

use rustypipe::client::ClientType;
use serde_json::Value;

use crate::{attestation::PoTokenContext, Error, MusicClient, Result};

fn interrupt() -> Arc<dyn Fn() -> bool + Send + Sync> {
    let operation = crate::operation::current();
    Arc::new(move || {
        operation
            .as_ref()
            .is_some_and(|operation| operation.check().is_err())
    })
}

// FFI and Windows callers may have small native stacks. The JavaScript
// stack budget must stay below an explicitly reserved, joined worker stack.
fn player_worker<T: Send>(work: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    crate::operation::ensure(|| {
        let context = crate::operation::current().expect("installed operation context");
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("ytmusic-upstream-player".into())
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, move || context.run(work))
                .map_err(|_| Error::Protocol("cannot initialize player worker".into()))?
                .join()
                .map_err(|_| Error::Protocol("player worker failed".into()))?
        })
    })
}

impl MusicClient {
    pub(crate) fn upstream_player_raw(
        &self,
        video_id: &str,
        reload_token: Option<&str>,
    ) -> Result<Value> {
        crate::operation::check()?;
        let query = self.upstream.query(self)?;
        let player_token = self
            .po_token_for(video_id, PoTokenContext::Player)?
            .or_else(|| self.config.po_token.clone());
        crate::operation::phase("requesting_stream");
        let result = player_worker(|| {
            super::run(query.player_raw_resolved(
                video_id,
                ClientType::DesktopMusic,
                player_token.as_deref(),
                reload_token,
                interrupt(),
            ))
        });
        // A QuickJS interruption can surface as an extraction failure or a raw
        // metadata response. Cancellation must win over either representation.
        crate::operation::check()?;
        crate::operation::phase("resolving_stream");
        result
    }

    pub(crate) fn upstream_prewarm_player(&self) -> Result<u32> {
        let query = self.upstream.query(self)?;
        crate::operation::phase("solving_challenges");
        let result = player_worker(|| super::run(query.prewarm_player(interrupt())));
        crate::operation::check()?;
        result
    }

    pub(crate) fn upstream_invalidate_player(&self) -> Result<()> {
        let query = self.upstream.query(self)?;
        super::run(async {
            query.invalidate_player().await;
            Ok(())
        })
    }
}
