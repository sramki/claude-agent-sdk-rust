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

use claude_agent_sdk_rs::types::{ContentBlock, SessionStoreEntry};
use claude_agent_sdk_rs::{
    content_blocks, get_session_entries, get_session_entries_from_store, get_session_entries_typed,
    get_session_entries_typed_from_store, get_session_messages, import_session_to_store,
    InMemorySessionStore,
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

/// Reads the **committed** fixture transcript through `get_session_entries`,
/// reconstructs it, and asserts byte-for-byte equality — including a numeric
/// line (`1e3` / `1.50`) that a parse→re-serialize `Value` round-trip would
/// normalize, proving the raw reader preserves what the typed path cannot.
#[test]
fn committed_fixture_reconstructs_byte_for_byte() {
    let _g = env_guard!();
    let config = claude_config_dir();

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript.jsonl");
    let raw = std::fs::read(&fixture).unwrap(); // source-of-truth bytes

    // Place the fixture verbatim where the reader resolves it.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    std::mem::forget(tmp);
    let pd = config.dir.join("projects").join(sanitize(&realpath(&cwd)));
    std::fs::create_dir_all(&pd).unwrap();
    let sid = "f1f2f3f4-0000-4000-8000-000000000001";
    std::fs::copy(&fixture, pd.join(format!("{sid}.jsonl"))).unwrap();

    let entries = get_session_entries(sid, Some(&cwd)).unwrap();

    // (1) Byte-for-byte reconstruction of the fixture.
    let raw_str = String::from_utf8(raw.clone()).unwrap();
    let rebuilt = if raw_str.ends_with('\n') {
        entries.join("\n") + "\n"
    } else {
        entries.join("\n")
    };
    assert_eq!(rebuilt.as_bytes(), raw.as_slice(), "fixture must reconstruct byte-for-byte");

    // (2) The interior blank line and both fork branches survive.
    assert!(entries.iter().any(|l| l.is_empty()), "blank line preserved");
    let branches = entries
        .iter()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["message"]["content"].as_str().is_some_and(|c| c.starts_with("branch ")))
        .count();
    assert_eq!(branches, 2, "both fork branches present");

    // (3) Contrast: a Value round-trip normalizes the numeric line; raw doesn't.
    let numeric = entries.iter().find(|l| l.contains("\"score\":1e3")).expect("numeric line");
    assert!(numeric.contains("1e3") && numeric.contains("1.50"), "raw keeps 1e3 / 1.50 verbatim");
    let reserialized = serde_json::to_string(&serde_json::from_str::<Value>(numeric).unwrap()).unwrap();
    assert_ne!(
        &reserialized, numeric,
        "a Value round-trip normalizes 1e3/1.50 (1000.0/1.5) — only the raw line is byte-exact"
    );
}

/// The real `~/.claude` config home, or `None` if this machine has no sessions
/// (so the test skips cleanly on CI / other machines).
fn real_config_home() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let cfg = PathBuf::from(home).join(".claude");
    cfg.join("projects").is_dir().then_some(cfg)
}

/// Reads the **largest real transcript on this machine** through
/// `get_session_entries` and asserts (1) byte-for-byte fidelity and (2) that
/// every field of every entry is recoverable. Gated: skips when no local
/// sessions exist. The transcript is never copied into the repo.
#[test]
fn real_transcript_round_trips_byte_for_byte_and_exposes_all_fields() {
    let _g = env_guard!();
    let Some(cfg) = real_config_home() else {
        eprintln!("skip: no ~/.claude/projects on this machine");
        return;
    };
    // Find the largest .jsonl under projects/.
    let mut biggest: Option<(u64, PathBuf)> = None;
    for proj in std::fs::read_dir(cfg.join("projects")).into_iter().flatten().flatten() {
        if !proj.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(proj.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                if biggest.as_ref().is_none_or(|(b, _)| len > *b) {
                    biggest = Some((len, p));
                }
            }
        }
    }
    let Some((size, path)) = biggest else {
        eprintln!("skip: no transcripts found");
        return;
    };
    eprintln!("real transcript: {} ({size} bytes)", path.display());

    let raw = std::fs::read_to_string(&path).unwrap();
    let sid = path.file_stem().unwrap().to_string_lossy().into_owned();

    // Read via the public API against the real config home.
    std::env::set_var("CLAUDE_CONFIG_DIR", &cfg);
    let entries = get_session_entries(&sid, None).unwrap();

    // (1) Byte-for-byte round-trip.
    let expected = if raw.ends_with('\n') {
        entries.join("\n") + "\n"
    } else {
        entries.join("\n")
    };
    assert_eq!(expected.len(), raw.len(), "byte length mismatch");
    assert_eq!(expected, raw, "real transcript must round-trip byte-for-byte");

    // (2) Every field: parse each non-blank line, union all top-level keys.
    let mut all_keys = std::collections::BTreeSet::new();
    let mut parsed = 0usize;
    for line in entries.iter().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).expect("every non-blank line is valid JSON");
        if let Some(obj) = v.as_object() {
            for k in obj.keys() {
                all_keys.insert(k.clone());
            }
        }
        parsed += 1;
    }
    eprintln!("entries: {parsed} | distinct top-level fields ({}): {all_keys:?}", all_keys.len());
    // The envelope fields the conversation reader drops must be present raw.
    for envelope in ["parentUuid", "timestamp"] {
        assert!(all_keys.contains(envelope), "raw read must expose '{envelope}'");
    }
    assert!(parsed > 0, "expected a non-empty transcript");
}

#[tokio::test]
async fn typed_entries_preserve_envelope_and_extra_and_content() {
    let _g = env_guard!();
    let config = claude_config_dir();
    let (cwd, sid, _pk, _raw) = write_rich_transcript(&config);

    let typed = get_session_entries_typed(&sid, Some(&cwd)).unwrap();
    // Same 7 entries the raw reader sees (no chain selection, no filtering).
    assert_eq!(typed.len(), 7);

    // Typed envelope access (no Value digging).
    let asst = typed.iter().find(|e| e.entry_type.as_deref() == Some("assistant")).unwrap();
    assert_eq!(asst.request_id.as_deref(), Some("req_1"));
    assert!(asst.parent_uuid.is_some());
    let first = typed.iter().find(|e| e.git_branch.is_some()).unwrap();
    assert_eq!(first.git_branch.as_deref(), Some("main"));
    assert_eq!(first.cwd.as_deref(), Some("/proj"));

    // Unknown/extra fields are preserved losslessly in `extra`.
    assert!(typed.iter().any(|e| e.extra.contains_key("userType")));
    let sidechain = typed.iter().find(|e| e.is_sidechain == Some(true)).unwrap();
    assert_eq!(sidechain.is_sidechain, Some(true));

    // Feature C: typed content blocks from the raw message payload.
    let blocks = content_blocks(asst.message.as_ref().unwrap());
    assert!(matches!(blocks.first(), Some(ContentBlock::Text(_))));

    // Store variant returns the same typed entries.
    let store = InMemorySessionStore::new();
    import_session_to_store(&sid, &store, Some(&cwd), true, 500).await.unwrap();
    let from_store = get_session_entries_typed_from_store(&store, &sid, Some(&cwd)).await.unwrap();
    assert_eq!(from_store.len(), 7);
    assert!(from_store.iter().any(|e| e.request_id.as_deref() == Some("req_1")));
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

    // Invalid UUID -> Ok(empty); valid-but-absent UUID -> Ok(empty) (store-reader contract).
    assert!(get_session_entries_from_store(&store, "bad", Some(&cwd)).await.unwrap().is_empty());
    assert!(get_session_entries_from_store(&store, &new_uuid(0xAB5E17), Some(&cwd)).await.unwrap().is_empty());
}
