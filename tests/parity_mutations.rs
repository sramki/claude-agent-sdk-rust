//! Parity tests ported from upstream `tests/test_session_mutations.py`
//! (claude-agent-sdk Python). Covers the local-filesystem mutation surface:
//! `rename_session`, `tag_session`, `delete_session`, `fork_session`, and
//! `project_key_for_directory`.
//!
//! These are the DISTINCT upstream cases not already covered by
//! `tests/mutations.rs`. `_try_append` / `_sanitize_unicode` micro-tests and
//! the `*_via_store` (SessionStore-backed) suite are intentionally skipped:
//! the former are private helpers (already unit-tested in `src/mutations.rs`),
//! the latter is the async store bucket, not ported.
//!
//! Env (`CLAUDE_CONFIG_DIR`) is process-global, so every test holds `ENV_LOCK`
//! and the file must run single-threaded (`--test-threads=1`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use claude_agent_sdk_rs::{
    delete_session, fork_session, get_session_info, get_session_messages, list_sessions,
    project_key_for_directory, rename_session, tag_session, Error,
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
    fs::create_dir_all(dir.join("projects")).unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
    Config { _tmp: tmp, dir }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn realpath(p: &Path) -> String {
    fs::canonicalize(p).unwrap().to_string_lossy().into_owned()
}

fn make_project_dir(config: &Config, project_path: &str) -> PathBuf {
    let dir = config.dir.join("projects").join(sanitize(project_path));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A valid, unique-enough UUID.
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

/// Two-line user/assistant session (no uuids) — matches upstream
/// `_make_session_file`.
fn write_session(pd: &Path, sid: &str, first_prompt: &str) {
    let body = format!(
        "{}\n{}\n",
        serde_json::json!({"type": "user", "message": {"role": "user", "content": first_prompt}}),
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "Hi!"}}),
    );
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();
}

/// Multi-turn transcript with a proper uuid/parentUuid chain — matches upstream
/// `_make_transcript_session`. Returns the ordered list of message uuids.
fn write_transcript(pd: &Path, sid: &str, num_turns: u64) -> Vec<String> {
    let mut uuids = Vec::new();
    let mut lines = Vec::new();
    let mut parent: Option<String> = None;
    for i in 0..num_turns {
        let user_uuid = new_uuid(0x1000 + i * 2);
        uuids.push(user_uuid.clone());
        lines.push(
            serde_json::json!({
                "type": "user",
                "uuid": user_uuid,
                "parentUuid": parent,
                "sessionId": sid,
                "timestamp": "2026-03-01T00:00:00Z",
                "message": {"role": "user", "content": format!("Turn {} question", i + 1)},
            })
            .to_string(),
        );
        parent = Some(user_uuid);

        let asst_uuid = new_uuid(0x1001 + i * 2);
        uuids.push(asst_uuid.clone());
        lines.push(
            serde_json::json!({
                "type": "assistant",
                "uuid": asst_uuid,
                "parentUuid": parent,
                "sessionId": sid,
                "timestamp": "2026-03-01T00:00:00Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": format!("Turn {} answer", i + 1)}]},
            })
            .to_string(),
        );
        parent = Some(asst_uuid);
    }
    fs::write(pd.join(format!("{sid}.jsonl")), lines.join("\n") + "\n").unwrap();
    uuids
}

/// Sets up a temp project with a canonicalized project dir. Returns
/// (project_path, project_dir).
fn temp_project(c: &Config) -> (PathBuf, PathBuf) {
    let tmp = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    let pd = make_project_dir(c, &realpath(&project));
    (project, pd)
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

// ===========================================================================
// rename_session
// ===========================================================================

#[test]
fn rename_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(matches!(
        rename_session("not-a-uuid", "title", None),
        Err(Error::InvalidSessionId(_))
    ));
    assert!(matches!(
        rename_session("", "title", None),
        Err(Error::InvalidSessionId(_))
    ));
}

#[test]
fn rename_empty_title() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x1001);
    write_session(&pd, &sid, "hi");
    for bad in ["", "   ", "\n\t"] {
        assert!(
            matches!(rename_session(&sid, bad, Some(&project)), Err(Error::Invalid(_))),
            "title {bad:?} should be rejected"
        );
    }
}

#[test]
fn rename_no_projects_dir() {
    let _g = env_guard!();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path().join("nonexistent"));
    let sid = new_uuid(0x1002);
    assert!(matches!(
        rename_session(&sid, "title", None),
        Err(Error::SessionNotFound(_))
    ));
}

#[test]
fn rename_appends_custom_title_entry() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x1003);
    write_session(&pd, &sid, "hi");

    rename_session(&sid, "My New Title", Some(&project)).unwrap();

    let lines = read_lines(&pd.join(format!("{sid}.jsonl")));
    let last = lines.last().unwrap();
    assert_eq!(last["type"], "custom-title");
    assert_eq!(last["customTitle"], "My New Title");
    assert_eq!(last["sessionId"], sid);
}

#[test]
fn rename_title_trimmed() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x1004);
    write_session(&pd, &sid, "hi");

    rename_session(&sid, "  Trimmed Title  ", Some(&project)).unwrap();

    let lines = read_lines(&pd.join(format!("{sid}.jsonl")));
    assert_eq!(lines.last().unwrap()["customTitle"], "Trimmed Title");
    let info = get_session_info(&sid, Some(&project)).unwrap().unwrap();
    assert_eq!(info.custom_title.as_deref(), Some("Trimmed Title"));
}

#[test]
fn rename_last_wins_via_list_sessions() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x1005);
    write_session(&pd, &sid, "original");

    rename_session(&sid, "First Title", Some(&project)).unwrap();
    rename_session(&sid, "Second Title", Some(&project)).unwrap();
    rename_session(&sid, "Final Title", Some(&project)).unwrap();

    let sessions = list_sessions(Some(&project), None, 0, false).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].custom_title.as_deref(), Some("Final Title"));
    assert_eq!(sessions[0].summary, "Final Title");
}

#[test]
fn rename_search_all_projects() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let pd = make_project_dir(&c, "/some/project");
    let sid = new_uuid(0x1006);
    write_session(&pd, &sid, "hi");

    rename_session(&sid, "Found Without Dir", None).unwrap();

    let lines = read_lines(&pd.join(format!("{sid}.jsonl")));
    assert_eq!(lines.last().unwrap()["customTitle"], "Found Without Dir");
}

#[test]
fn rename_skips_zero_byte_stub() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let proj_a = make_project_dir(&c, "/aaa/project");
    let proj_z = make_project_dir(&c, "/zzz/project");
    let sid = new_uuid(0x1007);
    // 0-byte stub in one dir; real file in another. Ordering does not matter:
    // the stub is skipped (0-byte), the real file gets the entry.
    fs::write(proj_a.join(format!("{sid}.jsonl")), "").unwrap();
    write_session(&proj_z, &sid, "real");

    rename_session(&sid, "New Title", None).unwrap();

    assert_eq!(fs::read_to_string(proj_a.join(format!("{sid}.jsonl"))).unwrap(), "");
    let real = fs::read_to_string(proj_z.join(format!("{sid}.jsonl"))).unwrap();
    assert!(real.contains(r#""customTitle":"New Title""#));
}

#[test]
fn rename_compact_json_format() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x1008);
    write_session(&pd, &sid, "hi");

    rename_session(&sid, "Title", Some(&project)).unwrap();

    let content = fs::read_to_string(pd.join(format!("{sid}.jsonl"))).unwrap();
    let last = content.trim().lines().last().unwrap();
    assert_eq!(
        last,
        format!(r#"{{"type":"custom-title","customTitle":"Title","sessionId":"{sid}"}}"#)
    );
}

// ===========================================================================
// tag_session
// ===========================================================================

#[test]
fn tag_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(matches!(
        tag_session("not-a-uuid", Some("tag"), None),
        Err(Error::InvalidSessionId(_))
    ));
    assert!(matches!(
        tag_session("", Some("tag"), None),
        Err(Error::InvalidSessionId(_))
    ));
}

#[test]
fn tag_empty_tag() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2001);
    write_session(&pd, &sid, "hi");
    for bad in ["", "   "] {
        assert!(
            matches!(tag_session(&sid, Some(bad), Some(&project)), Err(Error::Invalid(_))),
            "tag {bad:?} should be rejected"
        );
    }
}

#[test]
fn tag_session_not_found() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, _pd) = temp_project(&c);
    let sid = new_uuid(0x2002);
    assert!(matches!(
        tag_session(&sid, Some("tag"), Some(&project)),
        Err(Error::SessionNotFound(_))
    ));
}

#[test]
fn tag_appends_tag_entry() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2003);
    write_session(&pd, &sid, "hi");

    tag_session(&sid, Some("experiment"), Some(&project)).unwrap();

    let last = read_lines(&pd.join(format!("{sid}.jsonl"))).pop().unwrap();
    assert_eq!(last["type"], "tag");
    assert_eq!(last["tag"], "experiment");
    assert_eq!(last["sessionId"], sid);
}

#[test]
fn tag_trimmed() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2004);
    write_session(&pd, &sid, "hi");

    tag_session(&sid, Some("  my-tag  "), Some(&project)).unwrap();

    let last = read_lines(&pd.join(format!("{sid}.jsonl"))).pop().unwrap();
    assert_eq!(last["tag"], "my-tag");
    let info = get_session_info(&sid, Some(&project)).unwrap().unwrap();
    assert_eq!(info.tag.as_deref(), Some("my-tag"));
}

#[test]
fn tag_none_clears() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2005);
    write_session(&pd, &sid, "hi");

    tag_session(&sid, Some("original-tag"), Some(&project)).unwrap();
    tag_session(&sid, None, Some(&project)).unwrap();

    let last = read_lines(&pd.join(format!("{sid}.jsonl"))).pop().unwrap();
    assert_eq!(last["type"], "tag");
    assert_eq!(last["tag"], "");
    assert_eq!(last["sessionId"], sid);
    // Reader reads the cleared tag back as None.
    let info = get_session_info(&sid, Some(&project)).unwrap().unwrap();
    assert_eq!(info.tag, None);
}

#[test]
fn tag_last_wins() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2006);
    write_session(&pd, &sid, "hi");

    tag_session(&sid, Some("first"), Some(&project)).unwrap();
    tag_session(&sid, Some("second"), Some(&project)).unwrap();
    tag_session(&sid, Some("third"), Some(&project)).unwrap();

    let lines = read_lines(&pd.join(format!("{sid}.jsonl")));
    assert_eq!(lines.last().unwrap()["tag"], "third");
    let tag_count = lines.iter().filter(|e| e["type"] == "tag").count();
    assert_eq!(tag_count, 3);
}

#[test]
fn tag_compact_json_format() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2007);
    write_session(&pd, &sid, "hi");

    tag_session(&sid, Some("mytag"), Some(&project)).unwrap();

    let content = fs::read_to_string(pd.join(format!("{sid}.jsonl"))).unwrap();
    let last = content.trim().lines().last().unwrap();
    assert_eq!(last, format!(r#"{{"type":"tag","tag":"mytag","sessionId":"{sid}"}}"#));
}

#[test]
fn tag_unicode_sanitization() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2008);
    write_session(&pd, &sid, "hi");

    // zero-width space + BOM embedded.
    tag_session(&sid, Some("clean\u{200b}tag\u{feff}"), Some(&project)).unwrap();

    let last = read_lines(&pd.join(format!("{sid}.jsonl"))).pop().unwrap();
    assert_eq!(last["tag"], "cleantag");
}

#[test]
fn tag_rejects_pure_invisible() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x2009);
    write_session(&pd, &sid, "hi");

    assert!(matches!(
        tag_session(&sid, Some("\u{200b}\u{200c}\u{feff}"), Some(&project)),
        Err(Error::Invalid(_))
    ));
}

// ===========================================================================
// delete_session
// ===========================================================================

#[test]
fn delete_session_not_found() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    let sid = new_uuid(0x3001);
    assert!(matches!(
        delete_session(&sid, None),
        Err(Error::SessionNotFound(_))
    ));
}

#[test]
fn delete_removes_subagent_transcript_dir() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x3002);
    write_session(&pd, &sid, "hi");
    let subagent_dir = pd.join(&sid);
    fs::create_dir(&subagent_dir).unwrap();
    fs::write(subagent_dir.join(format!("{}.jsonl", new_uuid(0x3099))), "{}\n").unwrap();

    delete_session(&sid, Some(&project)).unwrap();

    assert!(!pd.join(format!("{sid}.jsonl")).exists());
    assert!(!subagent_dir.exists());
}

#[test]
fn delete_without_directory() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let pd = make_project_dir(&c, "/any/project");
    let sid = new_uuid(0x3003);
    write_session(&pd, &sid, "hi");
    assert!(pd.join(format!("{sid}.jsonl")).exists());

    delete_session(&sid, None).unwrap();

    assert!(!pd.join(format!("{sid}.jsonl")).exists());
}

// ===========================================================================
// fork_session
// ===========================================================================

#[test]
fn fork_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(matches!(
        fork_session("not-a-uuid", None, None, None),
        Err(Error::InvalidSessionId(_))
    ));
}

#[test]
fn fork_session_not_found() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    let sid = new_uuid(0x4001);
    assert!(matches!(
        fork_session(&sid, None, None, None),
        Err(Error::SessionNotFound(_))
    ));
}

#[test]
fn fork_remaps_uuids() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x4002);
    let original = write_transcript(&pd, &sid, 2);

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    let lines = read_lines(&pd.join(format!("{}.jsonl", result.session_id)));
    for entry in &lines {
        if matches!(entry["type"].as_str(), Some("user") | Some("assistant")) {
            let u = entry["uuid"].as_str().unwrap();
            assert!(!original.contains(&u.to_string()), "uuid {u} leaked");
            if let Some(p) = entry["parentUuid"].as_str() {
                assert!(!original.contains(&p.to_string()), "parentUuid {p} leaked");
            }
        }
    }
}

#[test]
fn fork_preserves_message_count() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x4003);
    write_transcript(&pd, &sid, 3);

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    let original_msgs = get_session_messages(&sid, Some(&project), None, 0).unwrap();
    let fork_msgs = get_session_messages(&result.session_id, Some(&project), None, 0).unwrap();
    assert_eq!(fork_msgs.len(), original_msgs.len());
}

#[test]
fn fork_session_id_in_entries() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x4004);
    write_transcript(&pd, &sid, 2);

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    let lines = read_lines(&pd.join(format!("{}.jsonl", result.session_id)));
    for entry in &lines {
        assert_eq!(entry["sessionId"], result.session_id);
    }
}

#[test]
fn fork_forked_from_field() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x4005);
    write_transcript(&pd, &sid, 2);

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    let lines = read_lines(&pd.join(format!("{}.jsonl", result.session_id)));
    for entry in &lines {
        if matches!(entry["type"].as_str(), Some("user") | Some("assistant")) {
            assert_eq!(entry["forkedFrom"]["sessionId"], sid);
        }
    }
}

#[test]
fn fork_without_directory() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let pd = make_project_dir(&c, "/any/project");
    let sid = new_uuid(0x4006);
    write_transcript(&pd, &sid, 2);

    let result = fork_session(&sid, None, None, None).unwrap();
    assert!(pd.join(format!("{}.jsonl", result.session_id)).exists());
}

#[test]
fn fork_clears_stale_fields() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x4007);
    let entry = serde_json::json!({
        "type": "user",
        "uuid": new_uuid(0x40aa),
        "parentUuid": null,
        "sessionId": sid,
        "timestamp": "2026-03-01T00:00:00Z",
        "teamName": "test-team",
        "agentName": "test-agent",
        "slug": "test-slug",
        "message": {"role": "user", "content": "Hello"},
    });
    fs::write(pd.join(format!("{sid}.jsonl")), entry.to_string() + "\n").unwrap();

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    let lines = read_lines(&pd.join(format!("{}.jsonl", result.session_id)));
    for e in &lines {
        if e["type"] == "user" {
            assert!(e.get("teamName").is_none());
            assert!(e.get("agentName").is_none());
            assert!(e.get("slug").is_none());
        }
    }
}

/// Not in upstream's test file, but exercises the documented fork behavior in
/// `src/mutations.rs`: entries with `isSidechain: true` are dropped from the
/// fork.
#[test]
fn fork_filters_sidechains() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let (project, pd) = temp_project(&c);
    let sid = new_uuid(0x4008);
    let (u1, a1, sc) = (new_uuid(0x4081), new_uuid(0x4082), new_uuid(0x4083));
    let body = [
        serde_json::json!({"type":"user","uuid":u1,"parentUuid":null,"sessionId":sid,"timestamp":"2026-03-01T00:00:00Z","message":{"role":"user","content":"main"}}),
        serde_json::json!({"type":"assistant","uuid":a1,"parentUuid":u1,"sessionId":sid,"timestamp":"2026-03-01T00:00:00Z","message":{"role":"assistant","content":"ok"}}),
        serde_json::json!({"type":"user","uuid":sc,"parentUuid":a1,"sessionId":sid,"isSidechain":true,"timestamp":"2026-03-01T00:00:00Z","message":{"role":"user","content":"SIDECHAIN_MARKER"}}),
    ]
    .iter()
    .map(|e| e.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    let content = fs::read_to_string(pd.join(format!("{}.jsonl", result.session_id))).unwrap();
    assert!(!content.contains("SIDECHAIN_MARKER"), "sidechain entry leaked into fork");
    let convo = read_lines(&pd.join(format!("{}.jsonl", result.session_id)))
        .into_iter()
        .filter(|e| matches!(e["type"].as_str(), Some("user") | Some("assistant")))
        .count();
    assert_eq!(convo, 2);
}

// ===========================================================================
// project_key_for_directory
// ===========================================================================

#[test]
fn project_key_derivation() {
    let _g = env_guard!();
    // Non-existent path canonicalizes to the NFC input, then sanitizes.
    let key = project_key_for_directory(Some(Path::new("/tmp/does-not-exist-xyz")));
    assert!(key.starts_with("-tmp-does-not-exist-xyz"), "key: {key}");
}
