//! Parity tests for the store-backed session readers (`*_from_store`), ported
//! from the read half of upstream `test_session_helpers_store.py`.

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use claude_agent_sdk_rs::types::{SessionKey, SessionStore, SessionStoreEntry};
use claude_agent_sdk_rs::{
    get_session_info_from_store, get_session_messages_from_store, get_subagent_messages_from_store,
    list_sessions_from_store, list_subagents_from_store, project_key_for_directory,
    InMemorySessionStore, Result,
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

    let sessions = list_sessions_from_store(&store, dir(), None, 0).await.unwrap();
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
    let page = list_sessions_from_store(&store, dir(), Some(2), 0).await.unwrap();
    assert_eq!(page.len(), 2);
    let page2 = list_sessions_from_store(&store, dir(), Some(2), 2).await.unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page[0].last_modified >= page2[0].last_modified);
}

#[tokio::test]
async fn list_raises_when_store_lacks_list_sessions() {
    let store = MinimalStore::default();
    let err = list_sessions_from_store(&store, dir(), None, 0).await.unwrap_err();
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

    let sessions = list_sessions_from_store(&store, dir(), None, 0).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, normal);
}

// --- get_session_info_from_store ---------------------------------------------

#[tokio::test]
async fn info_returns_seeded_and_none_for_unknown() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;

    let info = get_session_info_from_store(&store, &sid, dir()).await.unwrap().unwrap();
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
            &[entry(json!({"type": "custom-title", "customTitle": "My Title", "sessionId": sid}))],
        )
        .await
        .unwrap();

    let info = get_session_info_from_store(&store, &sid, dir()).await.unwrap().unwrap();
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

    let msgs = get_session_messages_from_store(&store, &sid, dir(), None, 0).await.unwrap();
    let ids: Vec<&str> = msgs.iter().map(|m| m.uuid.as_str()).collect();
    assert_eq!(ids, uuids.iter().map(String::as_str).collect::<Vec<_>>());

    // limit/offset
    let page = get_session_messages_from_store(&store, &sid, dir(), Some(2), 1).await.unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].uuid, uuids[1]);

    // unknown session -> empty
    assert!(get_session_messages_from_store(&store, &new_uuid(0x999), dir(), None, 0)
        .await
        .unwrap()
        .is_empty());
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

    let subs = list_subagents_from_store(&store, &sid, dir()).await.unwrap();
    assert_eq!(subs, vec!["abc"]);

    let msgs = get_subagent_messages_from_store(&store, &sid, "abc", dir(), None, 0).await.unwrap();
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

    assert_eq!(list_subagents_from_store(&store, &sid, dir()).await.unwrap(), vec!["deep"]);
    let msgs = get_subagent_messages_from_store(&store, &sid, "deep", dir(), None, 0).await.unwrap();
    assert_eq!(msgs.len(), 2); // agent_metadata dropped
}

#[tokio::test]
async fn list_subagents_dedupes_across_subpaths() {
    let store = InMemorySessionStore::new();
    let sid = new_uuid(0x100);
    seed_chain(&store, &sid, 1, 0x1000).await;
    store.append(&subkey(&sid, "subagents/agent-x"), &[user("a", &new_uuid(0x7001), None, &sid)]).await.unwrap();
    store.append(&subkey(&sid, "subagents/workflows/r/agent-x"), &[user("b", &new_uuid(0x7002), None, &sid)]).await.unwrap();
    let subs = list_subagents_from_store(&store, &sid, dir()).await.unwrap();
    assert_eq!(subs, vec!["x"]); // deduped
}

#[tokio::test]
async fn subagent_helpers_non_uuid_and_missing_list_subkeys() {
    let store = InMemorySessionStore::new();
    assert!(list_subagents_from_store(&store, "not-a-uuid", dir()).await.unwrap().is_empty());
    assert!(get_subagent_messages_from_store(&store, "not-a-uuid", "a", dir(), None, 0)
        .await
        .unwrap()
        .is_empty());

    // A store without list_subkeys -> list raises, but get falls back to the direct path.
    let minimal = MinimalStore::default();
    let sid = new_uuid(0x100);
    let err = list_subagents_from_store(&minimal, &sid, dir()).await.unwrap_err();
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
