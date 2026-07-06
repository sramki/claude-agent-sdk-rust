//! Integration tests for session mutations, verifying they round-trip through
//! the reader (exercising the real on-disk JSONL format).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use claude_agent_sdk::{
    delete_session, fork_session, get_session_info, list_sessions, rename_session, tag_session,
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

fn write_session(pd: &Path, sid: &str, first_prompt: &str) {
    let (u1, a1) = (new_uuid(0xA1), new_uuid(0xA2));
    let body = format!(
        "{}\n{}\n",
        serde_json::json!({"type": "user", "uuid": u1, "parentUuid": null, "sessionId": sid, "message": {"role": "user", "content": first_prompt}}),
        serde_json::json!({"type": "assistant", "uuid": a1, "parentUuid": u1, "sessionId": sid, "message": {"role": "assistant", "content": "hi"}}),
    );
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();
}

#[test]
fn rename_then_tag_are_read_back() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    let pd = make_project_dir(&c, &realpath(&project));
    let sid = new_uuid(0x1001);
    write_session(&pd, &sid, "original prompt");

    rename_session(&sid, "My Title", Some(&project)).unwrap();
    tag_session(&sid, Some("experiment"), Some(&project)).unwrap();

    let info = get_session_info(&sid, Some(&project)).unwrap().unwrap();
    assert_eq!(info.summary, "My Title");
    assert_eq!(info.custom_title.as_deref(), Some("My Title"));
    assert_eq!(info.tag.as_deref(), Some("experiment"));

    // Clearing the tag reads back as None.
    tag_session(&sid, None, Some(&project)).unwrap();
    let info = get_session_info(&sid, Some(&project)).unwrap().unwrap();
    assert_eq!(info.tag, None);
}

#[test]
fn rename_rejects_missing_session() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    let sid = new_uuid(0x2002);
    let err = rename_session(&sid, "x", None).unwrap_err();
    assert!(matches!(err, claude_agent_sdk::Error::SessionNotFound(_)));
}

#[test]
fn delete_removes_session() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    let pd = make_project_dir(&c, &realpath(&project));
    let sid = new_uuid(0x3003);
    write_session(&pd, &sid, "to delete");

    assert_eq!(list_sessions(Some(&project), None, 0, false).unwrap().len(), 1);
    delete_session(&sid, Some(&project)).unwrap();
    assert!(list_sessions(Some(&project), None, 0, false).unwrap().is_empty());
    assert!(get_session_info(&sid, Some(&project)).unwrap().is_none());
}

#[test]
fn fork_creates_new_readable_session() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    fs::create_dir_all(&project).unwrap();
    let pd = make_project_dir(&c, &realpath(&project));
    let sid = new_uuid(0x4004);
    write_session(&pd, &sid, "source conversation");

    let result = fork_session(&sid, Some(&project), None, None).unwrap();
    assert_ne!(result.session_id, sid);

    // The fork is a distinct, listable session with a "(fork)" title.
    let sessions = list_sessions(Some(&project), None, 0, false).unwrap();
    assert_eq!(sessions.len(), 2);
    let fork = get_session_info(&result.session_id, Some(&project))
        .unwrap()
        .unwrap();
    assert!(fork.summary.ends_with("(fork)"), "summary: {}", fork.summary);

    // With an explicit title, that title surfaces verbatim.
    let titled = fork_session(&sid, Some(&project), None, Some("Custom Fork")).unwrap();
    let info = get_session_info(&titled.session_id, Some(&project))
        .unwrap()
        .unwrap();
    assert_eq!(info.summary, "Custom Fork");
}
