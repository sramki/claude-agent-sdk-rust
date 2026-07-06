//! Batching layer between `transcript_mirror` stdout frames and a [`SessionStore`].
//!
//! Faithful port of `_internal/transcript_mirror_batcher.py`. The runtime read
//! loop peels `transcript_mirror` frames off stdout and calls [`enqueue`]; the
//! batcher accumulates and flushes to [`SessionStore::append`] on `result`
//! (explicit [`flush`]) or when the pending buffer crosses size thresholds
//! (eager background flush). Adapter failures are retried (3 attempts, short
//! backoff), then dropped and reported via `on_error`; failures never raise —
//! the local-disk transcript is already durable.
//!
//! [`enqueue`]: TranscriptMirrorBatcher::enqueue
//! [`flush`]: TranscriptMirrorBatcher::flush

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;

use crate::store::file_path_to_session_key;
use crate::store_import::{MAX_PENDING_BYTES, MAX_PENDING_ENTRIES};
use crate::types::{BoxFuture, SessionKey, SessionStore, SessionStoreEntry, SessionStoreFlushMode};

const SEND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: usize = 3;
const BACKOFF: [Duration; 2] = [Duration::from_millis(200), Duration::from_millis(800)];

/// Callback invoked when a batch is permanently dropped after retries.
pub type MirrorOnError =
    Arc<dyn Fn(Option<SessionKey>, String) -> BoxFuture<'static, ()> + Send + Sync>;

struct MirrorEntry {
    file_path: String,
    entries: Vec<SessionStoreEntry>,
}

#[derive(Default)]
struct Pending {
    items: Vec<MirrorEntry>,
    entries: usize,
    bytes: usize,
}

struct Inner {
    store: Arc<dyn SessionStore>,
    projects_dir: String,
    on_error: MirrorOnError,
    flush_mode: SessionStoreFlushMode,
    max_pending_entries: usize,
    max_pending_bytes: usize,
    pending: Mutex<Pending>,
    drain_lock: AsyncMutex<()>,
}

/// Accumulates `transcript_mirror` frames and flushes them to a store.
#[derive(Clone)]
pub struct TranscriptMirrorBatcher {
    inner: Arc<Inner>,
}

impl TranscriptMirrorBatcher {
    /// Creates a batcher for `store`, keying frame file paths against
    /// `projects_dir`. `on_error` surfaces a dropped batch (e.g. as a
    /// `MirrorErrorMessage`).
    pub fn new(
        store: Arc<dyn SessionStore>,
        projects_dir: String,
        flush_mode: SessionStoreFlushMode,
        on_error: MirrorOnError,
    ) -> Self {
        TranscriptMirrorBatcher {
            inner: Arc::new(Inner {
                store,
                projects_dir,
                on_error,
                flush_mode,
                max_pending_entries: MAX_PENDING_ENTRIES,
                max_pending_bytes: MAX_PENDING_BYTES,
                pending: Mutex::new(Pending::default()),
                drain_lock: AsyncMutex::new(()),
            }),
        }
    }

    /// Buffers a frame (fire-and-forget); schedules an eager background flush in
    /// `eager` mode or when the pending buffer crosses thresholds.
    pub fn enqueue(&self, file_path: String, entries: Vec<SessionStoreEntry>) {
        let size = serde_json::to_string(&entries).map(|s| s.len()).unwrap_or(0);
        let over = {
            let mut p = self.inner.pending.lock().unwrap();
            p.entries += entries.len();
            p.bytes += size;
            p.items.push(MirrorEntry { file_path, entries });
            p.entries > self.inner.max_pending_entries || p.bytes > self.inner.max_pending_bytes
        };
        if matches!(self.inner.flush_mode, SessionStoreFlushMode::Eager) || over {
            let inner = self.inner.clone();
            tokio::spawn(async move { inner.drain().await });
        }
    }

    /// Flushes all pending entries, serialized after any in-flight eager flush.
    pub async fn flush(&self) {
        self.inner.clone().drain().await;
    }

    /// Final flush before teardown. Never raises.
    pub async fn close(&self) {
        self.flush().await;
    }
}

impl Inner {
    async fn drain(self: Arc<Self>) {
        // Detach the pending buffer before acquiring the lock so enqueue can
        // keep accumulating into a fresh buffer while a prior flush is in flight.
        let items = {
            let mut p = self.pending.lock().unwrap();
            p.entries = 0;
            p.bytes = 0;
            std::mem::take(&mut p.items)
        };
        if items.is_empty() {
            return;
        }
        let mut errors: Vec<(SessionKey, String)> = Vec::new();
        {
            let _guard = self.drain_lock.lock().await;
            self.do_flush(items, &mut errors).await;
        }
        // Report errors after releasing the lock so a slow callback can't block
        // subsequent drains.
        for (key, msg) in errors {
            (self.on_error)(Some(key), msg).await;
        }
    }

    async fn do_flush(&self, items: Vec<MirrorEntry>, errors: &mut Vec<(SessionKey, String)>) {
        // Coalesce by file_path (first-seen order; entries keep enqueue order).
        let mut order: Vec<String> = Vec::new();
        let mut by_path: HashMap<String, Vec<SessionStoreEntry>> = HashMap::new();
        for item in items {
            match by_path.get_mut(&item.file_path) {
                Some(bucket) => bucket.extend(item.entries),
                None => {
                    order.push(item.file_path.clone());
                    by_path.insert(item.file_path, item.entries);
                }
            }
        }

        let projects = Path::new(&self.projects_dir);
        for file_path in order {
            let entries = by_path.remove(&file_path).unwrap_or_default();
            if entries.is_empty() {
                continue;
            }
            let key = match file_path_to_session_key(Path::new(&file_path), projects) {
                Some(k) => k,
                None => continue, // filePath not under projects_dir — drop.
            };

            let mut last_err: Option<String> = None;
            let mut succeeded = false;
            for attempt in 0..MAX_ATTEMPTS {
                if attempt > 0 {
                    tokio::time::sleep(BACKOFF[attempt - 1]).await;
                }
                match tokio::time::timeout(SEND_TIMEOUT, self.store.append(&key, &entries)).await {
                    Ok(Ok(())) => {
                        succeeded = true;
                        break;
                    }
                    Ok(Err(e)) => last_err = Some(e.to_string()),
                    Err(_elapsed) => {
                        // Don't retry a timeout — the in-flight call may still land.
                        last_err = Some(format!("append timed out after {SEND_TIMEOUT:?}"));
                        break;
                    }
                }
            }
            if !succeeded {
                errors.push((key, last_err.unwrap_or_else(|| "unknown error".into())));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemorySessionStore;
    use crate::types::SessionKey;
    use serde_json::json;

    const SID: &str = "11111111-1111-4111-8111-111111111111";

    fn noop() -> MirrorOnError {
        Arc::new(|_k, _e| Box::pin(async {}))
    }
    fn ent(uuid: &str) -> SessionStoreEntry {
        json!({"type": "user", "uuid": uuid}).as_object().cloned().unwrap()
    }
    fn batcher(store: Arc<InMemorySessionStore>, mode: SessionStoreFlushMode) -> TranscriptMirrorBatcher {
        TranscriptMirrorBatcher::new(store, "/projects".into(), mode, noop())
    }
    fn main_key() -> SessionKey {
        SessionKey { project_key: "-pk".into(), session_id: SID.into(), subpath: None }
    }

    #[tokio::test]
    async fn flush_with_nothing_pending_is_noop() {
        let store = Arc::new(InMemorySessionStore::new());
        batcher(store.clone(), SessionStoreFlushMode::Batched).flush().await;
        assert_eq!(store.size(), 0);
    }

    #[tokio::test]
    async fn coalesces_same_path_and_skips_empty_batches() {
        let store = Arc::new(InMemorySessionStore::new());
        let b = batcher(store.clone(), SessionStoreFlushMode::Batched);
        let fp = format!("/projects/-pk/{SID}.jsonl");
        b.enqueue(fp.clone(), vec![ent("a")]);
        b.enqueue(fp, vec![ent("b")]); // same path -> coalesced into one append
        b.enqueue(format!("/projects/-pk/{SID}.jsonl"), vec![]); // empty -> skipped
        b.flush().await;
        assert_eq!(store.get_entries(&main_key()).len(), 2);
    }

    #[tokio::test]
    async fn eager_mode_flushes_then_close() {
        let store = Arc::new(InMemorySessionStore::new());
        let b = batcher(store.clone(), SessionStoreFlushMode::Eager);
        b.enqueue(format!("/projects/-pk/{SID}.jsonl"), vec![ent("x")]);
        b.close().await; // final flush; exactly-once regardless of the eager race
        assert_eq!(store.get_entries(&main_key()).len(), 1);
    }

    #[tokio::test]
    async fn file_path_outside_projects_dir_is_dropped() {
        let store = Arc::new(InMemorySessionStore::new());
        let b = batcher(store.clone(), SessionStoreFlushMode::Batched);
        b.enqueue("/elsewhere/x.jsonl".into(), vec![ent("a")]);
        b.flush().await;
        assert_eq!(store.size(), 0);
    }
}
