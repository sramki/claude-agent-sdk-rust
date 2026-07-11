//! Integration tests for the cartridge's fs-backed locate + blob functions
//! (`list_projects`, `discover_transcripts`, `resolve_blob`). `CLAUDE_CONFIG_DIR`
//! is process-global, so tests serialize on `ENV_LOCK`.

use std::path::PathBuf;
use std::sync::Mutex;

use claude_agent_sdk_rs::cartridge::{discover_transcripts, list_projects, resolve_blob, Blob};

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

fn write(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn list_projects_and_discover_transcripts() {
    let _g = env_guard!();
    let cfg = claude_config_dir();
    let projects = cfg.dir.join("projects");

    // Project A: two top-level sessions + one nested subagent transcript.
    let sa = "aaaaaaaa-0000-4000-8000-000000000001";
    let sb = "bbbbbbbb-0000-4000-8000-000000000002";
    write(&projects.join("-proj-a").join(format!("{sa}.jsonl")), "{\"type\":\"user\"}\n");
    write(&projects.join("-proj-a").join(format!("{sb}.jsonl")), "{\"type\":\"user\"}\n");
    write(
        &projects.join("-proj-a").join(sa).join("subagents").join("agent-x.jsonl"),
        "{\"type\":\"user\"}\n",
    );
    // Project B: one session.
    let sc = "cccccccc-0000-4000-8000-000000000003";
    write(&projects.join("-proj-b").join(format!("{sc}.jsonl")), "{\"type\":\"user\"}\n");

    // list_projects: two folders, session_count = top-level *.jsonl only.
    let mut projs = list_projects().unwrap();
    projs.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(projs.len(), 2);
    assert_eq!(projs[0].name, "-proj-a");
    assert_eq!(projs[0].session_count, 2); // nested subagent not counted
    assert!(projs[0].path.ends_with("-proj-a"));
    assert_eq!(projs[1].name, "-proj-b");
    assert_eq!(projs[1].session_count, 1);

    // discover(false): top-level only (3 files: sa, sb, sc).
    let top = discover_transcripts(false).unwrap();
    assert_eq!(top.len(), 3);
    assert!(top.iter().all(|f| !f.is_subagent && f.subpath.is_none()));

    // discover(true): includes the nested subagent transcript (4 files).
    let all = discover_transcripts(true).unwrap();
    assert_eq!(all.len(), 4);
    let sub = all.iter().find(|f| f.is_subagent).unwrap();
    assert_eq!(sub.session_id, sa); // parent session id, not the file stem
    assert_eq!(sub.subpath.as_deref(), Some("subagents/agent-x"));
    assert_eq!(sub.project, "-proj-a");
}

#[test]
fn resolve_blob_finds_paste_cache_and_file_history() {
    let _g = env_guard!();
    let cfg = claude_config_dir();
    // Seed a paste-cache text blob and a file-history entry.
    write(&cfg.dir.join("paste-cache").join("deadbeefdeadbeef.txt"), "pasted text");
    std::fs::create_dir_all(cfg.dir.join("file-history").join("abc-123")).unwrap();

    match resolve_blob("deadbeefdeadbeef") {
        Some(Blob::Path(p)) => assert!(p.ends_with("paste-cache/deadbeefdeadbeef.txt")),
        other => panic!("expected paste-cache path, got {other:?}"),
    }
    assert!(matches!(resolve_blob("abc-123"), Some(Blob::Path(_)))); // file-history dir
    assert!(resolve_blob("missing").is_none());
    assert!(resolve_blob("../escape").is_none()); // traversal rejected
}

/// Real-data smoke: on a machine with sessions, recursive discovery must find
/// strictly more than top-level (the nested subagent/workflow transcripts).
/// Gated — skips cleanly where there are no local sessions.
#[test]
fn recursive_discovery_finds_more_on_real_data() {
    let _g = env_guard!();
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => return,
    };
    let cfg = home.join(".claude");
    if !cfg.join("projects").is_dir() {
        eprintln!("skip: no ~/.claude/projects");
        return;
    }
    std::env::set_var("CLAUDE_CONFIG_DIR", &cfg);
    let top = discover_transcripts(false).unwrap().len();
    let all = discover_transcripts(true).unwrap().len();
    eprintln!("real discovery: top-level={top}, recursive={all}");
    assert!(top > 0, "expected some top-level sessions");
    assert!(all >= top, "recursive must be a superset of top-level");
}
