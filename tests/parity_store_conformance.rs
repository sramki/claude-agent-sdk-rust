//! Runs the reusable SessionStore conformance suite against the built-in stores,
//! ported from upstream `test_session_store_conformance.py`.

use async_trait::async_trait;
use claude_agent_sdk_rs::testing::run_session_store_conformance;
use claude_agent_sdk_rs::types::{SessionKey, SessionStore, SessionStoreEntry};
use claude_agent_sdk_rs::{InMemorySessionStore, Result};

#[tokio::test]
async fn in_memory_store_is_conformant() {
    // InMemorySessionStore implements every optional method.
    run_session_store_conformance(InMemorySessionStore::new, &[]).await;
}

/// A minimal append/load-only store (optional methods default to Unsupported).
#[derive(Default)]
struct MinimalStore {
    data: std::sync::Mutex<std::collections::HashMap<String, Vec<SessionStoreEntry>>>,
}

fn key_str(k: &SessionKey) -> String {
    format!(
        "{}/{}/{}",
        k.project_key,
        k.session_id,
        k.subpath.as_deref().unwrap_or("")
    )
}

#[async_trait]
impl SessionStore for MinimalStore {
    async fn append(&self, k: &SessionKey, entries: &[SessionStoreEntry]) -> Result<()> {
        self.data
            .lock()
            .unwrap()
            .entry(key_str(k))
            .or_default()
            .extend_from_slice(entries);
        Ok(())
    }
    async fn load(&self, k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Ok(self.data.lock().unwrap().get(&key_str(k)).cloned())
    }
}

#[tokio::test]
async fn minimal_store_is_conformant_for_required_methods() {
    // The optional-method contracts auto-skip because MinimalStore returns
    // Error::Unsupported from them.
    run_session_store_conformance(MinimalStore::default, &[]).await;
}

#[tokio::test]
async fn skip_optional_skips_named_contracts() {
    // Explicitly skipping an optional method is honored even for a store that
    // implements it (here, list_session_summaries).
    run_session_store_conformance(InMemorySessionStore::new, &["list_session_summaries"]).await;
}
