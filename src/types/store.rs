//! Session-store types and the [`SessionStore`] adapter trait.
//!
//! Ported from the session-store section of the Python `types.py`. The trait
//! mirrors the `SessionStore` Protocol: `append`/`load` are required, and the
//! remaining methods are optional — their default implementations return
//! [`Error::Unsupported`], and call sites fall back accordingly (the upstream
//! equivalent is the `NotImplementedError`-raising Protocol defaults that call
//! sites probe for). Transcript entries are opaque pass-through JSON objects.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// One JSONL transcript line, as an opaque pass-through JSON object. Mirrors
/// `SessionStoreEntry` — adapters must round-trip it without interpreting it.
pub type SessionStoreEntry = Map<String, Value>;

/// Identifies a session (or subagent) transcript in a store. Mirrors
/// `SessionKey`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    /// Caller-defined scope (default: sanitized cwd).
    pub project_key: String,
    /// Session id.
    pub session_id: String,
    /// Subpath for subagent transcripts (omit for the main transcript).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
}

/// Entry returned by [`SessionStore::list_sessions`]. Mirrors
/// `SessionStoreListEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStoreListEntry {
    /// Session id.
    pub session_id: String,
    /// Last-modified time in Unix epoch milliseconds.
    pub mtime: i64,
}

/// Incrementally-maintained session summary. Mirrors `SessionSummaryEntry`.
/// The `data` field is opaque SDK-owned state — stores persist it verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummaryEntry {
    /// Session id.
    pub session_id: String,
    /// Storage write time of the sidecar, in Unix epoch milliseconds.
    pub mtime: i64,
    /// Opaque SDK-owned summary state.
    pub data: Map<String, Value>,
}

/// Key argument to [`SessionStore::list_subkeys`]. Mirrors
/// `SessionListSubkeysKey`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionListSubkeysKey {
    /// Project key.
    pub project_key: String,
    /// Session id.
    pub session_id: String,
}

/// Controls when transcript-mirror entries are flushed to a [`SessionStore`].
/// Mirrors `SessionStoreFlushMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStoreFlushMode {
    /// Buffer entries and flush once per turn (or at size thresholds).
    #[default]
    Batched,
    /// Trigger a background flush after every frame.
    Eager,
}

/// Adapter for mirroring session transcripts to external storage. Mirrors the
/// `SessionStore` Protocol.
///
/// Only [`append`](Self::append) and [`load`](Self::load) are required. The
/// remaining methods are optional; their defaults return
/// [`Error::Unsupported`], and higher-level session APIs fall back when a method
/// is unsupported.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Mirrors a batch of transcript entries. Called after the local write
    /// succeeds.
    async fn append(&self, key: &SessionKey, entries: &[SessionStoreEntry]) -> Result<()>;

    /// Loads a full session for resume. Returns `None` for a key that was
    /// never written.
    async fn load(&self, key: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>>;

    /// Lists sessions for a `project_key` (ids + modification times). Optional.
    async fn list_sessions(&self, _project_key: &str) -> Result<Vec<SessionStoreListEntry>> {
        Err(Error::Unsupported("SessionStore::list_sessions"))
    }

    /// Returns incrementally-maintained summaries for all sessions. Optional.
    async fn list_session_summaries(&self, _project_key: &str) -> Result<Vec<SessionSummaryEntry>> {
        Err(Error::Unsupported("SessionStore::list_session_summaries"))
    }

    /// Deletes a session (cascading to subkeys for a main-transcript key).
    /// Optional.
    async fn delete(&self, _key: &SessionKey) -> Result<()> {
        Err(Error::Unsupported("SessionStore::delete"))
    }

    /// Lists all subpath keys under a session. Optional.
    async fn list_subkeys(&self, _key: &SessionListSubkeysKey) -> Result<Vec<String>> {
        Err(Error::Unsupported("SessionStore::list_subkeys"))
    }
}
