//! Parity tests for the store-backed session readers (`*_from_store`), ported
//! from the read half of upstream `test_session_helpers_store.py`.

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use claude_agent_sdk_rs::types::{SessionKey, SessionStore, SessionStoreEntry};
use claude_agent_sdk_rs::{
    delete_session_via_store, fork_session_via_store, get_session_info_from_store,
    get_session_messages_from_store, get_subagent_messages_from_store, list_sessions_from_store,
    list_subagents_from_store, project_key_for_directory, rename_session_via_store,
    tag_session_via_store, InMemorySessionStore, Result,
};
use std::path::Path;

const DIR: &str = "/workspace/project";

fn dir() -> Option<&'static Path> {
    Some(Path::new(DIR))
}

fn pkey() -> String {
    project_key_for_directory(dir())
}

fn new_uuid(n: u64) -> String {
    let hex = format!("{n:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn entry(v: Value) -> SessionStoreEntry {
    v.as_object().cloned().unwrap_or_else(Map::new)
}

fn user(text: &str, uid: &str, parent: Option<&str>, sid: &str) -> SessionStoreEntry {
    entry(json!({
        "type": "user", "uuid": uid, "parentUuid": parent, "sessionId": sid,
        "timestamp": "2024-01-01T00:00:00.000Z",
        "message": {"role": "user", "content": text},
    }))
}

fn assistant(text: &str, uid: &str, parent: &str, sid: &str) -> SessionStoreEntry {
    entry(json!({
        "type": "assistant", "uuid": uid, "parentUuid": parent, "sessionId": sid,
        "timestamp": "2024-01-01T00:00:01.000Z",
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
    }))
}

fn key(sid: &str) -> SessionKey {
    SessionKey {
        project_key: pkey(),
        session_id: sid.to_string(),
        subpath: None,
    }
}

fn subkey(sid: &str, subpath: &str) -> SessionKey {
    SessionKey {
        project_key: pkey(),
        session_id: sid.to_string(),
        subpath: Some(subpath.to_string()),
    }
}

/// Appends `n` user/assistant pairs to `sid`; returns their uuids in order.
async fn seed_chain(store: &InMemorySessionStore, sid: &str, n: usize, seed: u64) -> Vec<String> {
    let mut uuids = Vec::new();
    let mut entries = Vec::new();
    let mut parent: Option<String> = None;
    for i in 0..n {
        let u = new_uuid(seed + (i as u64) * 2);
        let a = new_uuid(seed + (i as u64) * 2 + 1);
        entries.push(user(&format!("prompt {i}"), &u, parent.as_deref(), sid));
        entries.push(assistant(&format!("reply {i}"), &a, &u, sid));
        uuids.push(u.clone());
        uuids.push(a.clone());
        parent = Some(a);
    }
    store.append(&key(sid), &entries).await.unwrap();
    uuids
}

/// A duck-typed store implementing only the required append/load.
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

// --- list_sessions_from_store -------------------------------------------------

#[tokio::test]
async fn lists_seeded_sessions_sorted_by_mtime() {
    let store = InMemorySessionStore::new();
    let (a, b, c) = (new_uuid(0x100), new_uuid(0x200), new_uuid(0x300));
    seed_chain(&store, &a, 1, 0x1000).await;
    seed_chain(&store, &b, 1, 0x2000).await;
    seed_chain(&store, &c, 1, 0x3000).await;

    let sessions = list_sessions_from_store(&store, dir(), None, 0)
        .await
        .unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    assert_eq!(ids, vec![c.as_str(), b.as_str(), a.as_str()]); // newest first
    assert_eq!(sessions[0].summary, "prompt 0");
}

#[tokio::test]
async fn list_limit_and_offset() {
    let store = InMemorySessionStore::new();
    for i in 0..5u64 {
        seed_chain(&store, &new_uuid(0x100 + i), 1, 0x1000 + i * 100).await;
    }
    let page = list_sessions_from_store(&store, dir(), Some(2), 0)
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    let page2 = list_sessions_from_store(&store, dir(), Some(2), 2)
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page[0].last_modified >= page2[0].last_modified);
}

#[tokio::test]
async fn list_raises_when_store_lacks_list_sessions() {
    let store = MinimalStore::default();
    let err = list_sessions_from_store(&store, dir(), None, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, claude_agent_sdk_rs::Error::Invalid(_)));
}

#[tokio::test]
async fn list_drops_sidechain_sessions() {
    let store = InMemorySessionStore::new();
    let normal = new_uuid(0x100);
    seed_chain(&store, &normal, 1, 0x1000).await;
    let side = new_uuid(0x200);
    store
        .append(
            &key(&side),
            &[entry(json!({
                "type": "user", "uuid": new_uuid(0x2001), "parentUuid": null,
                "sessionId": side, "isSidechain": true,
                "message": {"role": "user", "content": "side"},
            }))],
        )
        .await
        .unwrap();

    let sessions = list_sessions_from_store(&store, dir(), None, 0)
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, normal);
}

// --- get_session_info_from_store ---------------------------------------------

#[tokio::test]
async fn info_returns_seeded_and_none_for_unknown() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;

    let info = get_session_info_from_store(&store, &sid, dir())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.session_id, sid);
    assert_eq!(info.summary, "prompt 0");

    assert!(get_session_info_from_store(&store, &new_uuid(0x999), dir())
        .await
        .unwrap()
        .is_none());
    assert!(get_session_info_from_store(&store, "not-a-uuid", dir())
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn info_reflects_custom_title_and_cwd_fallback() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    store
        .append(
            &key(&sid),
            &[entry(
                json!({"type": "custom-title", "customTitle": "My Title", "sessionId": sid}),
            )],
        )
        .await
        .unwrap();

    let info = get_session_info_from_store(&store, &sid, dir())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.summary, "My Title");
    assert_eq!(info.custom_title.as_deref(), Some("My Title"));
    // Entries lack cwd -> falls back to the canonicalized project directory.
    assert!(info.cwd.as_deref().unwrap().ends_with("workspace/project"));
}

// --- get_session_messages_from_store -----------------------------------------

#[tokio::test]
async fn messages_chain_in_order_and_ignores_metadata() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    let uuids = seed_chain(&store, &sid, 2, 0x1000).await;
    // Interleave metadata that must be ignored by the chain builder.
    store
        .append(
            &key(&sid),
            &[
                entry(json!({"type": "tag", "tag": "x", "sessionId": sid})),
                entry(json!({"type": "custom-title", "customTitle": "T", "sessionId": sid})),
            ],
        )
        .await
        .unwrap();

    let msgs = get_session_messages_from_store(&store, &sid, dir(), None, 0)
        .await
        .unwrap();
    let ids: Vec<&str> = msgs.iter().map(|m| m.uuid.as_str()).collect();
    assert_eq!(ids, uuids.iter().map(String::as_str).collect::<Vec<_>>());

    // limit/offset
    let page = get_session_messages_from_store(&store, &sid, dir(), Some(2), 1)
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].uuid, uuids[1]);

    // unknown session -> empty
    assert!(
        get_session_messages_from_store(&store, &new_uuid(0x999), dir(), None, 0)
            .await
            .unwrap()
            .is_empty()
    );
}

// --- subagents ----------------------------------------------------------------

#[tokio::test]
async fn list_and_get_subagent_messages() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    let (u, a) = (new_uuid(0x5001), new_uuid(0x5002));
    store
        .append(
            &subkey(&sid, "subagents/agent-abc"),
            &[
                user("task", &u, None, &sid),
                assistant("done", &a, &u, &sid),
            ],
        )
        .await
        .unwrap();

    let subs = list_subagents_from_store(&store, &sid, dir())
        .await
        .unwrap();
    assert_eq!(subs, vec!["abc"]);

    let msgs = get_subagent_messages_from_store(&store, &sid, "abc", dir(), None, 0)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].uuid, u);
}

#[tokio::test]
async fn nested_workflow_subpath_and_agent_metadata_filtered() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    let (u, a) = (new_uuid(0x6001), new_uuid(0x6002));
    store
        .append(
            &subkey(&sid, "subagents/workflows/run-1/agent-deep"),
            &[
                entry(json!({"type": "agent_metadata", "note": "sidecar"})),
                user("hi", &u, None, &sid),
                assistant("hello", &a, &u, &sid),
            ],
        )
        .await
        .unwrap();

    assert_eq!(
        list_subagents_from_store(&store, &sid, dir())
            .await
            .unwrap(),
        vec!["deep"]
    );
    let msgs = get_subagent_messages_from_store(&store, &sid, "deep", dir(), None, 0)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 2); // agent_metadata dropped
}

#[tokio::test]
async fn list_subagents_dedupes_across_subpaths() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    store
        .append(
            &subkey(&sid, "subagents/agent-x"),
            &[user("a", &new_uuid(0x7001), None, &sid)],
        )
        .await
        .unwrap();
    store
        .append(
            &subkey(&sid, "subagents/workflows/r/agent-x"),
            &[user("b", &new_uuid(0x7002), None, &sid)],
        )
        .await
        .unwrap();
    let subs = list_subagents_from_store(&store, &sid, dir())
        .await
        .unwrap();
    assert_eq!(subs, vec!["x"]); // deduped
}

#[tokio::test]
async fn subagent_helpers_non_uuid_and_missing_list_subkeys() {
    let store = InMemorySessionStore::new();
    assert!(list_subagents_from_store(&store, "not-a-uuid", dir())
        .await
        .unwrap()
        .is_empty());
    assert!(
        get_subagent_messages_from_store(&store, "not-a-uuid", "a", dir(), None, 0)
            .await
            .unwrap()
            .is_empty()
    );

    // A store without list_subkeys -> list raises, but get falls back to the direct path.
    let minimal = MinimalStore::default();
    let sid = new_uuid(0x100);
    let err = list_subagents_from_store(&minimal, &sid, dir())
        .await
        .unwrap_err();
    assert!(matches!(err, claude_agent_sdk_rs::Error::Invalid(_)));

    let (u, a) = (new_uuid(0x8001), new_uuid(0x8002));
    minimal
        .append(
            &subkey(&sid, "subagents/agent-direct"),
            &[user("t", &u, None, &sid), assistant("d", &a, &u, &sid)],
        )
        .await
        .unwrap();
    let msgs = get_subagent_messages_from_store(&minimal, &sid, "direct", dir(), None, 0)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 2); // direct-path fallback
}

// --- *_via_store mutations ----------------------------------------------------

#[tokio::test]
async fn rename_via_store_appends_trimmed_custom_title() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;

    rename_session_via_store(&store, &sid, "  New Title  ", dir()).await.unwrap();

    let entries = store.get_entries(&key(&sid));
    let last = entries.last().unwrap();
    assert_eq!(last["type"], "custom-title");
    assert_eq!(last["customTitle"], "New Title");
    assert_eq!(last["sessionId"], sid);
    assert!(last["uuid"].is_string());
    assert!(last["timestamp"].is_string());
}

#[tokio::test]
async fn rename_via_store_invalid_inputs_raise() {
    let store = InMemorySessionStore::new();
    assert!(rename_session_via_store(&store, "not-a-uuid", "t", dir()).await.is_err());
    assert!(rename_session_via_store(&store, &new_uuid(0x1), "  ", dir()).await.is_err());
}

#[tokio::test]
async fn tag_via_store_append_clear_and_reflected() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;

    tag_session_via_store(&store, &sid, Some("experiment"), dir()).await.unwrap();
    let last = store.get_entries(&key(&sid)).last().unwrap().clone();
    assert_eq!(last["type"], "tag");
    assert_eq!(last["tag"], "experiment");

    // Reflected through the store reader.
    let info = get_session_info_from_store(&store, &sid, dir()).await.unwrap().unwrap();
    assert_eq!(info.tag.as_deref(), Some("experiment"));

    // None clears (empty-string tag entry).
    tag_session_via_store(&store, &sid, None, dir()).await.unwrap();
    let last = store.get_entries(&key(&sid)).last().unwrap().clone();
    assert_eq!(last["tag"], "");
    let info = get_session_info_from_store(&store, &sid, dir()).await.unwrap().unwrap();
    assert_eq!(info.tag, None);
}

#[tokio::test]
async fn delete_via_store_removes_and_noop_without_delete() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    assert_eq!(store.size(), 1);
    delete_session_via_store(&store, &sid, dir()).await.unwrap();
    assert_eq!(store.size(), 0);

    // A store without delete() -> no-op (no error).
    let minimal = MinimalStore::default();
    delete_session_via_store(&minimal, &new_uuid(0x2), dir()).await.unwrap();

    // Non-UUID is rejected without touching the store.
    assert!(delete_session_via_store(&store, "not-a-uuid", dir()).await.is_err());
    assert!(tag_session_via_store(&store, "not-a-uuid", Some("x"), dir()).await.is_err());
}

#[tokio::test]
async fn fork_via_store_round_trips_with_new_uuids() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    let src = seed_chain(&store, &sid, 2, 0x1000).await;

    let result = fork_session_via_store(&store, &sid, dir(), None, None).await.unwrap();
    assert_ne!(result.session_id, sid);

    let forked = store.get_entries(&key(&result.session_id));
    let msg_entries: Vec<_> = forked
        .iter()
        .filter(|e| matches!(e["type"].as_str(), Some("user") | Some("assistant")))
        .collect();
    assert_eq!(msg_entries.len(), 4);
    // Fresh UUIDs — none of the source uuids reappear.
    for e in &msg_entries {
        assert!(!src.contains(&e["uuid"].as_str().unwrap().to_string()));
    }
    // A custom-title trailer with "(fork)" is appended.
    assert!(forked.iter().any(|e| e["type"] == "custom-title"
        && e["customTitle"].as_str().unwrap().ends_with("(fork)")));

    // The fork is readable via the store reader.
    let msgs = get_session_messages_from_store(&store, &result.session_id, dir(), None, 0)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 4);
}

#[tokio::test]
async fn fork_via_store_up_to_and_errors() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    let src = seed_chain(&store, &sid, 3, 0x1000).await;

    // Fork up to the 2nd message (index 1) -> 2 conversation messages.
    let result = fork_session_via_store(&store, &sid, dir(), Some(&src[1]), None).await.unwrap();
    let msgs = get_session_messages_from_store(&store, &result.session_id, dir(), None, 0)
        .await
        .unwrap();
    assert_eq!(msgs.len(), 2);

    // Not found + invalid ids.
    assert!(fork_session_via_store(&store, &new_uuid(0x999), dir(), None, None).await.is_err());
    assert!(fork_session_via_store(&store, "not-a-uuid", dir(), None, None).await.is_err());
    assert!(fork_session_via_store(&store, &sid, dir(), Some("bad"), None).await.is_err());
}

// --- slow path / gap-fill / error branches -----------------------------------

use claude_agent_sdk_rs::store::fold_session_summary;
use claude_agent_sdk_rs::types::{SessionStoreListEntry, SessionSummaryEntry};
use claude_agent_sdk_rs::Error;

/// A store with `list_sessions` but no `list_session_summaries` — forces the
/// per-session `load()` slow path. `load` behavior is configurable per session
/// id: "err" errors, "none" returns None, otherwise a one-message transcript.
struct SlowPathStore {
    listing: Vec<(String, i64)>,
}

#[async_trait]
impl SessionStore for SlowPathStore {
    async fn append(&self, _k: &SessionKey, _e: &[SessionStoreEntry]) -> Result<()> {
        Ok(())
    }
    async fn load(&self, k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        if k.session_id.starts_with("err") {
            return Err(Error::connection("load boom"));
        }
        if k.session_id.starts_with("none") {
            return Ok(None);
        }
        Ok(Some(vec![user("hi", &new_uuid(0xAB), None, &k.session_id)]))
    }
    async fn list_sessions(&self, _project_key: &str) -> Result<Vec<SessionStoreListEntry>> {
        Ok(self
            .listing
            .iter()
            .map(|(sid, mtime)| SessionStoreListEntry {
                session_id: sid.clone(),
                mtime: *mtime,
            })
            .collect())
    }
}

#[tokio::test]
async fn slow_path_load_ok_err_and_none() {
    // A real UUID for the Ok row (parse_session_info requires a plausible id);
    // "err"/"none" prefixes drive the other two branches.
    let ok = new_uuid(0x100);
    let store = SlowPathStore {
        listing: vec![
            (ok.clone(), 3000),
            ("err-session".into(), 2000),
            ("none-session".into(), 1000),
        ],
    };
    let sessions = list_sessions_from_store(&store, dir(), None, 0).await.unwrap();
    // Ok row parses; err row degrades to an empty-summary row; none row drops.
    assert!(sessions.iter().any(|s| s.session_id == ok && s.summary == "hi"));
    assert!(sessions.iter().any(|s| s.session_id == "err-session" && s.summary.is_empty()));
    assert!(!sessions.iter().any(|s| s.session_id == "none-session"));
}

/// A store returning stale/dropped/fresh summaries alongside `list_sessions`,
/// to exercise the gap-fill and drop branches of the summary fast-path.
struct StaleSummaryStore {
    inner: InMemorySessionStore,
    summaries: Vec<SessionSummaryEntry>,
    listing: Vec<(String, i64)>,
}

#[async_trait]
impl SessionStore for StaleSummaryStore {
    async fn append(&self, k: &SessionKey, e: &[SessionStoreEntry]) -> Result<()> {
        self.inner.append(k, e).await
    }
    async fn load(&self, k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        self.inner.load(k).await
    }
    async fn list_sessions(&self, _p: &str) -> Result<Vec<SessionStoreListEntry>> {
        Ok(self
            .listing
            .iter()
            .map(|(sid, mtime)| SessionStoreListEntry { session_id: sid.clone(), mtime: *mtime })
            .collect())
    }
    async fn list_session_summaries(&self, _p: &str) -> Result<Vec<SessionSummaryEntry>> {
        Ok(self.summaries.clone())
    }
}

#[tokio::test]
async fn summary_fast_path_stale_dropped_and_fresh() {
    let fresh = new_uuid(0x100);
    let stale = new_uuid(0x200);
    let dropped = new_uuid(0x300); // in summaries but not in listing
    let inner = InMemorySessionStore::new();
    // Seed transcripts for fresh + stale so gap-fill/load can parse them.
    seed_chain(&inner, &fresh, 1, 0x1000).await;
    seed_chain(&inner, &stale, 1, 0x2000).await;

    // Build a valid (opaque-data) summary via the fold, then override mtime.
    let summary = |sid: &str, mtime: i64| {
        let mut e = fold_session_summary(
            None,
            &key(sid),
            &[user("prompt 0", &new_uuid(0xFEED), None, sid)],
        );
        e.mtime = mtime;
        let _: &SessionSummaryEntry = &e;
        e
    };
    let store = StaleSummaryStore {
        inner,
        summaries: vec![
            summary(&fresh, 5000),
            summary(&stale, 10),   // older than listing mtime -> gap-fill
            summary(&dropped, 9000), // not in listing -> dropped
        ],
        listing: vec![(fresh.clone(), 5000), (stale.clone(), 6000)],
    };

    let sessions = list_sessions_from_store(&store, dir(), None, 0).await.unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    assert!(ids.contains(&fresh.as_str()));
    assert!(ids.contains(&stale.as_str()));
    assert!(!ids.contains(&dropped.as_str())); // dropped: summary but no listing row
    // The fresh row comes from the summary; the stale one is gap-filled via load.
    let fresh_row = sessions.iter().find(|s| s.session_id == fresh).unwrap();
    assert!(!fresh_row.summary.is_empty());
}

/// A store whose `load` always errors, to exercise error propagation in the
/// direct getters.
struct ErrLoadStore;

#[async_trait]
impl SessionStore for ErrLoadStore {
    async fn append(&self, _k: &SessionKey, _e: &[SessionStoreEntry]) -> Result<()> {
        Ok(())
    }
    async fn load(&self, _k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Err(Error::connection("load failed"))
    }
}

#[tokio::test]
async fn getters_propagate_load_errors() {
    let store = ErrLoadStore;
    let sid = new_uuid(0x100);
    assert!(get_session_info_from_store(&store, &sid, dir()).await.is_err());
    assert!(get_session_messages_from_store(&store, &sid, dir(), None, 0).await.is_err());
    assert!(get_subagent_messages_from_store(&store, &sid, "a", dir(), None, 0).await.is_err());
}

// --- more *_via_store branch coverage ----------------------------------------

#[tokio::test]
async fn tag_via_store_whitespace_only_errors() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    // Some("   ") is non-empty text but blank after trim -> rejected.
    assert!(tag_session_via_store(&store, &sid, Some("   "), dir()).await.is_err());
}

/// A store whose `delete` fails with a non-Unsupported error, to exercise the
/// error-propagation arm of `delete_session_via_store`.
struct DeleteErrStore;

#[async_trait]
impl SessionStore for DeleteErrStore {
    async fn append(&self, _k: &SessionKey, _e: &[SessionStoreEntry]) -> Result<()> {
        Ok(())
    }
    async fn load(&self, _k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Ok(None)
    }
    async fn delete(&self, _k: &SessionKey) -> Result<()> {
        Err(Error::connection("delete boom"))
    }
}

#[tokio::test]
async fn delete_via_store_propagates_real_errors() {
    let store = DeleteErrStore;
    let sid = new_uuid(0x100);
    // Unsupported is swallowed, but a real delete error must propagate.
    assert!(delete_session_via_store(&store, &sid, dir()).await.is_err());
}

#[tokio::test]
async fn fork_via_store_derives_title_from_ai_title_and_explicit_title() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    // An aiTitle entry (no customTitle) drives derive_title_from_entries' aiTitle arm.
    store
        .append(&key(&sid), &[entry(json!({"type": "ai-title", "aiTitle": "Auto Title", "sessionId": sid}))])
        .await
        .unwrap();
    let forked = fork_session_via_store(&store, &sid, dir(), None, None).await.unwrap();
    assert!(store
        .get_entries(&key(&forked.session_id))
        .iter()
        .any(|e| e["type"] == "custom-title"
            && e["customTitle"].as_str().unwrap().contains("Auto Title")));

    // An explicit title is used verbatim (with the "(fork)" suffix).
    let forked2 = fork_session_via_store(&store, &sid, dir(), None, Some("My Fork")).await.unwrap();
    assert!(store
        .get_entries(&key(&forked2.session_id))
        .iter()
        .any(|e| e["type"] == "custom-title"
            && e["customTitle"].as_str().unwrap().contains("My Fork")));
}

#[tokio::test]
async fn fork_via_store_up_to_message_not_found_errors() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    // A valid-UUID up_to that isn't in the transcript -> not-found error.
    let missing = new_uuid(0xDEAD);
    assert!(fork_session_via_store(&store, &sid, dir(), Some(&missing), None).await.is_err());
}
