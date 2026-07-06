//! Parity tests for `import_session_to_store`, ported from upstream
//! `test_session_import.py`. Creates a local `~/.claude` transcript, imports it
//! into an `InMemorySessionStore`, and verifies the round-trip.
//!
//! The env lock is intentionally held across `.await` to serialize the
//! process-global `CLAUDE_CONFIG_DIR` for the whole (single-threaded) async
//! test — so `await_holding_lock` is allowed here.
#![allow(clippy::await_holding_lock)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Value};

use claude_agent_sdk_rs::types::{SessionKey, SessionStore, SessionStoreEntry};
use claude_agent_sdk_rs::{
    import_session_to_store, list_subagents_from_store, InMemorySessionStore, Result,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());
macro_rules! env_guard {
    () => {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    };
}

struct Config {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

fn claude_config_dir() -> Config {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude");
    std::fs::create_dir_all(dir.join("projects")).unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
    Config { _tmp: tmp, dir }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn realpath(p: &Path) -> String {
    std::fs::canonicalize(p).unwrap().to_string_lossy().into_owned()
}

fn project_dir(config: &Config, canonical: &str) -> PathBuf {
    let dir = config.dir.join("projects").join(sanitize(canonical));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

fn write_lines(path: &Path, lines: &[Value]) {
    let body = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn skey(pk: &str, sid: &str, subpath: Option<&str>) -> SessionKey {
    SessionKey {
        project_key: pk.to_string(),
        session_id: sid.to_string(),
        subpath: subpath.map(str::to_string),
    }
}

/// A store that counts append() calls (for batching assertions).
#[derive(Default)]
struct CountingStore {
    appends: AtomicUsize,
    inner: InMemorySessionStore,
}

#[async_trait]
impl SessionStore for CountingStore {
    async fn append(&self, key: &SessionKey, entries: &[SessionStoreEntry]) -> Result<()> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        self.inner.append(key, entries).await
    }
    async fn load(&self, key: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        self.inner.load(key).await
    }
}

/// Sets up a project + session file; returns (config, cwd, project_key, sid, project_dir).
fn setup(sid: &str) -> (Config, PathBuf, String, PathBuf) {
    let config = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    // Leak the tmp dir so cwd stays valid for the test duration.
    std::mem::forget(tmp);
    let canonical = realpath(&cwd);
    let pk = sanitize(&canonical);
    let pd = project_dir(&config, &canonical);
    write_lines(
        &pd.join(format!("{sid}.jsonl")),
        &[
            json!({"type": "user", "uuid": new_uuid(0xA1), "parentUuid": null, "sessionId": sid, "message": {"role": "user", "content": "hi"}}),
            json!({"type": "assistant", "uuid": new_uuid(0xA2), "parentUuid": new_uuid(0xA1), "sessionId": sid, "message": {"role": "assistant", "content": "hello"}}),
        ],
    );
    (config, cwd, pk, pd)
}

#[tokio::test]
async fn imports_main_transcript() {
    let _g = env_guard!();
    let sid = new_uuid(0x1001);
    let (_c, cwd, pk, _pd) = setup(&sid);
    let store = InMemorySessionStore::new();

    import_session_to_store(&sid, &store, Some(&cwd), true, 500).await.unwrap();

    let entries = store.get_entries(&skey(&pk, &sid, None));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["type"], "user");
}

#[tokio::test]
async fn batching_and_blank_lines_and_default_batch() {
    let _g = env_guard!();
    let sid = new_uuid(0x2002);
    let config = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    std::mem::forget(tmp);
    let canonical = realpath(&cwd);
    let pd = project_dir(&config, &canonical);
    // 5 entries with an interleaved blank line.
    let mut body = String::new();
    for i in 0..5 {
        body.push_str(&json!({"type": "user", "uuid": new_uuid(0x2100 + i), "parentUuid": null, "sessionId": sid, "message": {"content": "x"}}).to_string());
        body.push('\n');
        if i == 2 {
            body.push('\n'); // blank line — must be skipped
        }
    }
    std::fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();

    let store = CountingStore::default();
    import_session_to_store(&sid, &store, Some(&cwd), true, 2).await.unwrap();
    // 5 entries / batch 2 => 3 append calls; blank line skipped.
    assert_eq!(store.appends.load(Ordering::SeqCst), 3);
    let entries = store.inner.get_entries(&skey(&sanitize(&canonical), &sid, None));
    assert_eq!(entries.len(), 5);

    // batch_size 0 uses the default -> a single append for a small file.
    let store2 = CountingStore::default();
    import_session_to_store(&sid, &store2, Some(&cwd), true, 0).await.unwrap();
    assert_eq!(store2.appends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn imports_subagents_with_subpath_and_meta() {
    let _g = env_guard!();
    let sid = new_uuid(0x3003);
    let (_c, cwd, pk, pd) = setup(&sid);

    let subagents = pd.join(&sid).join("subagents");
    write_lines(
        &subagents.join("agent-abc.jsonl"),
        &[json!({"type": "user", "uuid": new_uuid(0x3100), "parentUuid": null, "sessionId": sid, "message": {"content": "task"}})],
    );
    // Nested workflow subagent.
    write_lines(
        &subagents.join("workflows").join("run-1").join("agent-deep.jsonl"),
        &[json!({"type": "user", "uuid": new_uuid(0x3200), "parentUuid": null, "sessionId": sid, "message": {"content": "deep"}})],
    );
    // A .meta.json sidecar for agent-abc.
    std::fs::write(
        subagents.join("agent-abc.meta.json"),
        json!({"agentType": "general-purpose", "worktreePath": "/wt"}).to_string(),
    )
    .unwrap();

    let store = InMemorySessionStore::new();
    import_session_to_store(&sid, &store, Some(&cwd), true, 500).await.unwrap();

    // Subagents discoverable via the store reader.
    let mut subs = list_subagents_from_store(&store, &sid, Some(&cwd)).await.unwrap();
    subs.sort();
    assert_eq!(subs, vec!["abc", "deep"]);

    // agent-abc entries include the synthetic agent_metadata from the sidecar.
    let abc = store.get_entries(&skey(&pk, &sid, Some("subagents/agent-abc")));
    assert!(abc.iter().any(|e| e["type"] == "agent_metadata"
        && e["agentType"] == "general-purpose"));

    // Nested subpath keyed correctly.
    let deep = store.get_entries(&skey(&pk, &sid, Some("subagents/workflows/run-1/agent-deep")));
    assert_eq!(deep.len(), 1);
}

#[tokio::test]
async fn include_subagents_false_and_no_subagents_dir() {
    let _g = env_guard!();
    let sid = new_uuid(0x4004);
    let (_c, cwd, pk, pd) = setup(&sid);
    let subagents = pd.join(&sid).join("subagents");
    write_lines(
        &subagents.join("agent-x.jsonl"),
        &[json!({"type": "user", "uuid": new_uuid(0x4100), "parentUuid": null, "sessionId": sid, "message": {"content": "t"}})],
    );

    let store = InMemorySessionStore::new();
    import_session_to_store(&sid, &store, Some(&cwd), false, 500).await.unwrap();
    assert!(list_subagents_from_store(&store, &sid, Some(&cwd)).await.unwrap().is_empty());
    // Main transcript still imported.
    assert_eq!(store.get_entries(&skey(&pk, &sid, None)).len(), 2);

    // A session with no subagents dir imports fine (noop for subagents).
    let sid2 = new_uuid(0x4005);
    write_lines(
        &pd.join(format!("{sid2}.jsonl")),
        &[json!({"type": "user", "uuid": new_uuid(0x4200), "parentUuid": null, "sessionId": sid2, "message": {"content": "x"}})],
    );
    let store2 = InMemorySessionStore::new();
    import_session_to_store(&sid2, &store2, Some(&cwd), true, 500).await.unwrap();
    assert_eq!(store2.get_entries(&skey(&pk, &sid2, None)).len(), 1);
}

#[tokio::test]
async fn invalid_uuid_and_not_found_raise() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    let store = InMemorySessionStore::new();
    assert!(matches!(
        import_session_to_store("not-a-uuid", &store, None, true, 500).await,
        Err(claude_agent_sdk_rs::Error::InvalidSessionId(_))
    ));
    assert!(matches!(
        import_session_to_store(&new_uuid(0x9999), &store, None, true, 500).await,
        Err(claude_agent_sdk_rs::Error::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn directory_none_keys_from_resolved_path() {
    let _g = env_guard!();
    let sid = new_uuid(0x5005);
    let (_c, _cwd, pk, _pd) = setup(&sid);
    let store = InMemorySessionStore::new();

    // Import with directory=None: the resolver searches all projects and keys
    // from the on-disk project dir name, not the process cwd.
    import_session_to_store(&sid, &store, None, true, 500).await.unwrap();
    let entries = store.get_entries(&skey(&pk, &sid, None));
    assert_eq!(entries.len(), 2);
}

