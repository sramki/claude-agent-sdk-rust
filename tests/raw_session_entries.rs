//! Tests for the lossless raw-entry readers (`get_session_entries` /
//! `get_session_entries_from_store`).
//!
//! The core guarantee is byte-for-byte fidelity of the on-disk reader: read a
//! transcript, hand back its lines, reconstruct, and compare to the original
//! bytes. The fixture deliberately exercises every way `get_session_messages`
//! loses data — a fork (two leaves), a sidechain, a meta line, a non-message
//! entry, a blank line, and rich envelope fields — so a passing byte compare
//! proves none of that is dropped.
//!
//! `CLAUDE_CONFIG_DIR` is process-global, so tests hold `ENV_LOCK` across await.
#![allow(clippy::await_holding_lock)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use claude_agent_sdk_rs::types::SessionStoreEntry;
use claude_agent_sdk_rs::{
    get_session_entries, get_session_entries_from_store, get_session_messages,
    import_session_to_store, InMemorySessionStore,
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

fn new_uuid(n: u64) -> String {
    let hex = format!("{n:032x}");
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// A transcript with all the traits `get_session_messages` would drop.
/// Returns (project_dir, session_id, project_key, raw_file_bytes).
fn write_rich_transcript(config: &Config) -> (PathBuf, String, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    std::mem::forget(tmp); // keep cwd alive for the test
    let canonical = realpath(&cwd);
    let pk = sanitize(&canonical);
    let pd = config.dir.join("projects").join(&pk);
    std::fs::create_dir_all(&pd).unwrap();

    let sid = new_uuid(0x5E51);
    let (u1, a1, u2a, u2b) = (new_uuid(1), new_uuid(2), new_uuid(3), new_uuid(4));
    // Rich envelopes + a FORK: u2a and u2b both branch off a1 (two leaves).
    let lines: Vec<Value> = vec![
        json!({"type":"user","uuid":u1,"parentUuid":null,"sessionId":sid,"timestamp":"2024-01-01T00:00:00.000Z","cwd":"/proj","gitBranch":"main","version":"1.0","userType":"external","message":{"role":"user","content":"hello"}}),
        json!({"type":"assistant","uuid":a1,"parentUuid":u1,"sessionId":sid,"timestamp":"2024-01-01T00:00:01.000Z","requestId":"req_1","message":{"role":"assistant","content":"hi"}}),
        // Fork branch A (an edit/retry) off a1:
        json!({"type":"user","uuid":u2a,"parentUuid":a1,"sessionId":sid,"timestamp":"2024-01-01T00:00:02.000Z","message":{"role":"user","content":"branch A"}}),
        // Fork branch B off a1 — only ONE of these survives get_session_messages:
        json!({"type":"user","uuid":u2b,"parentUuid":a1,"sessionId":sid,"timestamp":"2024-01-01T00:00:03.000Z","message":{"role":"user","content":"branch B"}}),
        // A sidechain entry (dropped by the chain reader):
        json!({"type":"user","uuid":new_uuid(5),"parentUuid":a1,"sessionId":sid,"isSidechain":true,"message":{"role":"user","content":"subagent chatter"}}),
        // A meta entry (dropped):
        json!({"type":"user","uuid":new_uuid(6),"parentUuid":null,"sessionId":sid,"isMeta":true,"message":{"role":"user","content":"meta"}}),
        // A non-message metadata entry (dropped):
        json!({"type":"custom-title","customTitle":"My Session","sessionId":sid,"uuid":new_uuid(7)}),
    ];
    // Assemble raw bytes: LF-terminated, with an interior blank line after entry 4.
    let mut raw = String::new();
    for (i, l) in lines.iter().enumerate() {
        raw.push_str(&l.to_string());
        raw.push('\n');
        if i == 3 {
            raw.push('\n'); // interior blank line — must survive byte-for-byte
        }
    }
    std::fs::write(pd.join(format!("{sid}.jsonl")), &raw).unwrap();
    (cwd, sid, pk, raw)
}

#[test]
fn get_session_entries_round_trips_byte_for_byte() {
    let _g = env_guard!();
    let config = claude_config_dir();
    let (cwd, sid, _pk, raw) = write_rich_transcript(&config);

    let entries = get_session_entries(&sid, Some(&cwd)).unwrap();

    // 1. Byte-for-byte: rejoin with '\n' + the file's trailing newline == source.
    let reconstructed = entries.join("\n") + "\n";
    assert_eq!(reconstructed, raw, "raw entries must reconstruct the file byte-for-byte");

    // 2. The interior blank line survived as an empty entry.
    assert!(entries.iter().any(|l| l.is_empty()), "blank line must be preserved");

    // 3. Every entry is present — including both fork branches, the sidechain,
    //    the meta line, and the non-message entry (7 JSON lines + 1 blank = 8).
    let json_lines = entries.iter().filter(|l| !l.is_empty()).count();
    assert_eq!(json_lines, 7);

    // 4. Contrast: get_session_messages loses all of that — one branch, no envelope.
    let msgs = get_session_messages(&sid, Some(&cwd), None, 0).unwrap();
    assert!(msgs.len() < 7, "conversation reader collapses to one branch");
    // Only one of the two fork branches (A/B) can appear in the chain view.
    let branch_texts: Vec<String> = entries
        .iter()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["message"]["content"].as_str().map(str::to_string))
        .filter(|c| c.starts_with("branch "))
        .collect();
    assert_eq!(branch_texts.len(), 2, "raw read keeps BOTH fork branches");
}

#[test]
fn get_session_entries_invalid_and_missing() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    // Invalid UUID -> Err (matches the disk reader contract).
    assert!(matches!(
        get_session_entries("not-a-uuid", None),
        Err(claude_agent_sdk_rs::Error::InvalidSessionId(_))
    ));
    // Unknown session -> Ok(empty).
    assert!(get_session_entries(&new_uuid(0xDEAD), None).unwrap().is_empty());
}

#[tokio::test]
async fn get_session_entries_from_store_preserves_all_fields() {
    let _g = env_guard!();
    let config = claude_config_dir();
    let (cwd, sid, _pk, _raw) = write_rich_transcript(&config);

    // Import the local transcript into a store, then read the raw entries back.
    let store = InMemorySessionStore::new();
    import_session_to_store(&sid, &store, Some(&cwd), true, 500).await.unwrap();

    let entries: Vec<SessionStoreEntry> =
        get_session_entries_from_store(&store, &sid, Some(&cwd)).await.unwrap();

    // Every entry survives (no chain selection, no filtering) — same 7 the disk
    // reader saw. Field fidelity: envelope fields like gitBranch/requestId remain.
    assert_eq!(entries.len(), 7);
    assert!(entries.iter().any(|e| e.get("gitBranch") == Some(&json!("main"))));
    assert!(entries.iter().any(|e| e.get("requestId") == Some(&json!("req_1"))));
    // Both fork branches present.
    let branches = entries
        .iter()
        .filter_map(|e| e.get("message").and_then(|m| m.get("content")).and_then(Value::as_str))
        .filter(|c| c.starts_with("branch "))
        .count();
    assert_eq!(branches, 2);

    // Unknown / invalid -> Ok(empty) (store-reader contract).
    assert!(get_session_entries_from_store(&store, "bad", Some(&cwd)).await.unwrap().is_empty());
}
