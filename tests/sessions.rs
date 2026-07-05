//! Integration tests ported from the Python SDK's `tests/test_sessions.py`
//! (the filesystem path). Each test builds a temporary `~/.claude/projects/...`
//! layout, points `CLAUDE_CONFIG_DIR` at it, and exercises the public API.
//!
//! Because `CLAUDE_CONFIG_DIR` is process-global and Rust runs tests in
//! parallel threads, every test here holds a shared mutex for its duration so
//! the env var and filesystem fixtures never interleave.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use claude_agent_sdk::{
    get_session_info, get_session_messages, get_subagent_messages, list_sessions, list_subagents,
    MessageType,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Recovers a poisoned lock so one panicking test doesn't cascade into all the
/// others reporting a lock-poison panic instead of their real failure.
macro_rules! env_guard {
    () => {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    };
}

// ---------------------------------------------------------------------------
// Fixture helpers (mirror the pytest fixtures)
// ---------------------------------------------------------------------------

struct Config {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

/// Creates a temporary `~/.claude` and points `CLAUDE_CONFIG_DIR` at it.
fn claude_config_dir() -> Config {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude");
    fs::create_dir_all(dir.join("projects")).unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
    Config { _tmp: tmp, dir }
}

/// Short-path sanitize (no long-path hashing needed for these fixtures).
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

#[derive(Default)]
struct SessionOpts<'a> {
    session_id: Option<&'a str>,
    first_prompt: Option<&'a str>,
    summary: Option<&'a str>,
    custom_title: Option<&'a str>,
    git_branch: Option<&'a str>,
    cwd: Option<&'a str>,
    is_sidechain: bool,
    is_meta_only: bool,
    mtime: Option<u64>,
}

fn set_mtime(path: &Path, secs: u64) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
}

/// Mirrors `_make_session_file`. Returns the session id.
fn make_session_file(project_dir: &Path, opts: SessionOpts) -> String {
    let sid = opts
        .session_id
        .map(str::to_string)
        .unwrap_or_else(new_uuid);
    let first_prompt = opts.first_prompt.unwrap_or("Hello Claude");

    let mut first = Map::new();
    first.insert("type".into(), json!("user"));
    first.insert("message".into(), json!({"role": "user", "content": first_prompt}));
    if let Some(cwd) = opts.cwd {
        first.insert("cwd".into(), json!(cwd));
    }
    if let Some(gb) = opts.git_branch {
        first.insert("gitBranch".into(), json!(gb));
    }
    if opts.is_sidechain {
        first.insert("isSidechain".into(), json!(true));
    }
    if opts.is_meta_only {
        first.insert("isMeta".into(), json!(true));
    }

    let assistant = json!({"type": "assistant", "message": {"role": "assistant", "content": "Hi there!"}});

    let mut tail = Map::new();
    tail.insert("type".into(), json!("summary"));
    if let Some(s) = opts.summary {
        tail.insert("summary".into(), json!(s));
    }
    if let Some(ct) = opts.custom_title {
        tail.insert("customTitle".into(), json!(ct));
    }
    if let Some(gb) = opts.git_branch {
        tail.insert("gitBranch".into(), json!(gb));
    }

    let content = format!(
        "{}\n{}\n{}\n",
        Value::Object(first),
        assistant,
        Value::Object(tail)
    );
    let path = project_dir.join(format!("{sid}.jsonl"));
    fs::write(&path, content).unwrap();

    if let Some(m) = opts.mtime {
        set_mtime(&path, m);
    }
    sid
}

/// A random-enough UUID (no external crate). Format `8-4-4-4-12` hex.
fn new_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH as EPOCH};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(EPOCH).unwrap().as_nanos() as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Mix time + counter + address entropy into 128 bits of hex.
    let a = nanos ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let b = nanos
        .rotate_left(32)
        .wrapping_add(n.wrapping_mul(0xD1B5_4A32_D192_ED03))
        ^ (&n as *const _ as u64);
    let hex = format!("{a:016x}{b:016x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn tentry(t: &str, uuid: &str, parent: Option<&str>, sid: &str, content: Option<Value>) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!(t));
    o.insert("uuid".into(), json!(uuid));
    o.insert(
        "parentUuid".into(),
        parent.map(|p| json!(p)).unwrap_or(Value::Null),
    );
    o.insert("sessionId".into(), json!(sid));
    if let Some(c) = content {
        let role = if t == "user" || t == "assistant" { t } else { "user" };
        o.insert("message".into(), json!({"role": role, "content": c}));
    }
    Value::Object(o)
}

fn with(mut e: Value, key: &str, val: Value) -> Value {
    e.as_object_mut().unwrap().insert(key.into(), val);
    e
}

fn write_transcript(project_dir: &Path, sid: &str, entries: &[Value]) -> PathBuf {
    let body: String = entries
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let path = project_dir.join(format!("{sid}.jsonl"));
    fs::write(&path, body + "\n").unwrap();
    path
}

fn ids(msgs: &[claude_agent_sdk::SessionMessage]) -> Vec<String> {
    msgs.iter().map(|m| m.uuid.clone()).collect()
}

// ---------------------------------------------------------------------------
// list_sessions()
// ---------------------------------------------------------------------------

#[test]
fn empty_projects_dir() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(list_sessions(None, None, 0, true).is_empty());
}

#[test]
fn no_config_dir() {
    let _g = env_guard!();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path().join("nonexistent"));
    assert!(list_sessions(None, None, 0, true).is_empty());
}

#[test]
fn single_session() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("my-project");
    fs::create_dir_all(&project_path).unwrap();
    let canonical = realpath(&project_path);
    let pd = make_project_dir(&c, &canonical);
    let sid = make_session_file(
        &pd,
        SessionOpts {
            first_prompt: Some("What is 2+2?"),
            git_branch: Some("main"),
            cwd: Some(project_path.to_str().unwrap()),
            ..Default::default()
        },
    );

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.session_id, sid);
    assert_eq!(s.first_prompt.as_deref(), Some("What is 2+2?"));
    assert_eq!(s.summary, "What is 2+2?");
    assert_eq!(s.git_branch.as_deref(), Some("main"));
    assert_eq!(s.cwd.as_deref(), Some(project_path.to_str().unwrap()));
    assert!(s.file_size.unwrap() > 0);
    assert!(s.last_modified > 0);
    assert_eq!(s.custom_title, None);
}

#[test]
fn custom_title_wins_summary() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    make_session_file(
        &pd,
        SessionOpts {
            first_prompt: Some("original question"),
            summary: Some("auto summary"),
            custom_title: Some("My Custom Title"),
            ..Default::default()
        },
    );

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].summary, "My Custom Title");
    assert_eq!(sessions[0].custom_title.as_deref(), Some("My Custom Title"));
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("original question"));
}

#[test]
fn summary_wins_first_prompt() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    make_session_file(
        &pd,
        SessionOpts {
            first_prompt: Some("question"),
            summary: Some("better summary"),
            ..Default::default()
        },
    );

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].summary, "better summary");
    assert_eq!(sessions[0].custom_title, None);
}

#[test]
fn multiple_sessions_sorted_by_mtime() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));

    let sid_old = make_session_file(&pd, SessionOpts { first_prompt: Some("old"), mtime: Some(1000), ..Default::default() });
    let sid_new = make_session_file(&pd, SessionOpts { first_prompt: Some("new"), mtime: Some(3000), ..Default::default() });
    let sid_mid = make_session_file(&pd, SessionOpts { first_prompt: Some("mid"), mtime: Some(2000), ..Default::default() });

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 3);
    assert_eq!(
        sessions.iter().map(|s| s.session_id.clone()).collect::<Vec<_>>(),
        vec![sid_new, sid_mid, sid_old]
    );
    assert_eq!(sessions[0].last_modified, 3_000_000);
    assert_eq!(sessions[1].last_modified, 2_000_000);
    assert_eq!(sessions[2].last_modified, 1_000_000);
}

#[test]
fn limit_restricts() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    for i in 0..5 {
        make_session_file(&pd, SessionOpts { first_prompt: Some("p"), mtime: Some(1000 + i), ..Default::default() });
    }
    let sessions = list_sessions(Some(&project_path), Some(2), 0, false);
    assert_eq!(sessions.len(), 2);
    assert!(sessions[0].last_modified >= sessions[1].last_modified);
}

#[test]
fn offset_pagination() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    for i in 0..5 {
        make_session_file(&pd, SessionOpts { first_prompt: Some("p"), mtime: Some(1000 + i), ..Default::default() });
    }
    let page1 = list_sessions(Some(&project_path), Some(2), 0, false);
    let page2 = list_sessions(Some(&project_path), Some(2), 2, false);
    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    let p1: std::collections::HashSet<_> = page1.iter().map(|s| &s.session_id).collect();
    let p2: std::collections::HashSet<_> = page2.iter().map(|s| &s.session_id).collect();
    assert!(p1.is_disjoint(&p2));
    assert!(page1[0].last_modified > page2[0].last_modified);
    assert!(list_sessions(Some(&project_path), None, 100, false).is_empty());
}

#[test]
fn filters_sidechain_sessions() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    make_session_file(&pd, SessionOpts { first_prompt: Some("normal"), ..Default::default() });
    make_session_file(&pd, SessionOpts { first_prompt: Some("sidechain"), is_sidechain: true, ..Default::default() });

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("normal"));
}

#[test]
fn filters_empty_sessions() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    make_session_file(&pd, SessionOpts { first_prompt: Some("ignored meta"), is_meta_only: true, ..Default::default() });
    make_session_file(&pd, SessionOpts { first_prompt: Some("real content"), ..Default::default() });

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("real content"));
}

#[test]
fn filters_non_uuid_filenames() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    fs::write(
        pd.join("not-a-uuid.jsonl"),
        "{\"type\":\"user\",\"message\":{\"content\":\"x\"}}\n",
    )
    .unwrap();
    make_session_file(&pd, SessionOpts { first_prompt: Some("valid session"), ..Default::default() });

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("valid session"));
}

#[test]
fn ignores_non_jsonl_files() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    fs::write(pd.join("README.md"), "not a session").unwrap();
    make_session_file(&pd, SessionOpts { first_prompt: Some("session"), ..Default::default() });

    assert_eq!(list_sessions(Some(&project_path), None, 0, false).len(), 1);
}

#[test]
fn list_all_sessions() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let p1 = make_project_dir(&c, "/some/path/one");
    let p2 = make_project_dir(&c, "/some/path/two");
    make_session_file(&p1, SessionOpts { first_prompt: Some("from proj1"), mtime: Some(1000), ..Default::default() });
    make_session_file(&p2, SessionOpts { first_prompt: Some("from proj2"), mtime: Some(2000), ..Default::default() });

    let sessions = list_sessions(None, None, 0, true);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("from proj2"));
    assert_eq!(sessions[1].first_prompt.as_deref(), Some("from proj1"));
}

#[test]
fn list_all_sessions_dedupes() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let p1 = make_project_dir(&c, "/path/one");
    let p2 = make_project_dir(&c, "/path/two");
    let shared = new_uuid();
    make_session_file(&p1, SessionOpts { session_id: Some(&shared), first_prompt: Some("older"), mtime: Some(1000), ..Default::default() });
    make_session_file(&p2, SessionOpts { session_id: Some(&shared), first_prompt: Some("newer"), mtime: Some(2000), ..Default::default() });

    let sessions = list_sessions(None, None, 0, true);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("newer"));
    assert_eq!(sessions[0].last_modified, 2_000_000);
}

#[test]
fn nonexistent_project_dir() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("never-used");
    fs::create_dir_all(&project_path).unwrap();
    assert!(list_sessions(Some(&project_path), None, 0, false).is_empty());
}

#[test]
fn empty_file_filtered() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    fs::write(pd.join(format!("{}.jsonl", new_uuid())), "").unwrap();
    assert!(list_sessions(Some(&project_path), None, 0, false).is_empty());
}

#[test]
fn include_worktrees_disabled() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("main-proj");
    fs::create_dir_all(&project_path).unwrap();
    let canonical = realpath(&project_path);
    let main_dir = make_project_dir(&c, &canonical);
    make_session_file(&main_dir, SessionOpts { first_prompt: Some("main session"), ..Default::default() });
    let other_dir = make_project_dir(&c, &format!("{canonical}-worktree"));
    make_session_file(&other_dir, SessionOpts { first_prompt: Some("worktree session"), ..Default::default() });

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt.as_deref(), Some("main session"));
}

#[test]
fn limit_zero_returns_all() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    for _ in 0..3 {
        make_session_file(&pd, SessionOpts { first_prompt: Some("p"), ..Default::default() });
    }
    assert_eq!(list_sessions(Some(&project_path), Some(0), 0, false).len(), 3);
}

#[test]
fn cwd_from_head_fallback_to_project_path() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let canonical = realpath(&project_path);
    let pd = make_project_dir(&c, &canonical);
    make_session_file(&pd, SessionOpts { first_prompt: Some("no cwd field"), ..Default::default() });

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].cwd.as_deref(), Some(canonical.as_str()));
}

#[test]
fn git_branch_from_tail_preferred() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let body = "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"gitBranch\":\"old-branch\"}\n\
         {\"type\":\"summary\",\"gitBranch\":\"new-branch\"}\n";
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].git_branch.as_deref(), Some("new-branch"));
}

// ---------------------------------------------------------------------------
// Tag extraction
// ---------------------------------------------------------------------------

fn write_lines(pd: &Path, sid: &str, lines: &[&str]) {
    let body = lines.join("\n") + "\n";
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();
}

#[test]
fn tag_extracted_from_tail() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}",
            &format!("{{\"type\":\"tag\",\"tag\":\"my-tag\",\"sessionId\":\"{sid}\"}}"),
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].tag.as_deref(), Some("my-tag"));
}

#[test]
fn tag_last_wins() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}",
            &format!("{{\"type\":\"tag\",\"tag\":\"first-tag\",\"sessionId\":\"{sid}\"}}"),
            &format!("{{\"type\":\"tag\",\"tag\":\"second-tag\",\"sessionId\":\"{sid}\"}}"),
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].tag.as_deref(), Some("second-tag"));
}

#[test]
fn tag_empty_string_is_none() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}",
            &format!("{{\"type\":\"tag\",\"tag\":\"old-tag\",\"sessionId\":\"{sid}\"}}"),
            &format!("{{\"type\":\"tag\",\"tag\":\"\",\"sessionId\":\"{sid}\"}}"),
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].tag, None);
}

#[test]
fn tag_absent() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    make_session_file(&pd, SessionOpts { first_prompt: Some("hello"), ..Default::default() });
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].tag, None);
}

#[test]
fn tag_ignores_tool_use_inputs() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"tag this v1.0\"}}",
            &format!("{{\"type\":\"tag\",\"tag\":\"real-tag\",\"sessionId\":\"{sid}\"}}"),
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"mcp__docker__build\",\"input\":{\"tag\":\"myapp:v2\",\"context\":\".\"}}]}}",
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].tag.as_deref(), Some("real-tag"));
}

#[test]
fn tag_none_when_only_tool_use_tag() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"build docker\"}}",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"input\":{\"tag\":\"prod\"}}]}}",
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].tag, None);
}

// ---------------------------------------------------------------------------
// created_at extraction
// ---------------------------------------------------------------------------

#[test]
fn created_at_from_iso_timestamp() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"timestamp\":\"2026-01-15T10:30:00.000Z\"}",
            "{\"type\":\"assistant\",\"message\":{\"content\":\"hi\"},\"timestamp\":\"2026-01-15T10:35:00.000Z\"}",
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].created_at, Some(1768473000000));
}

#[test]
fn created_at_leq_last_modified() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &["{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"timestamp\":\"2026-01-01T00:00:00.000Z\"}"],
    );
    set_mtime(&pd.join(format!("{sid}.jsonl")), 1769904000); // 2026-02-01 UTC

    let sessions = list_sessions(Some(&project_path), None, 0, false);
    let s = &sessions[0];
    assert!(s.created_at.is_some());
    assert!(s.created_at.unwrap() <= s.last_modified);
}

#[test]
fn created_at_when_first_line_lacks_timestamp() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"permission-mode\",\"permissionMode\":\"acceptEdits\"}",
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"timestamp\":\"2026-01-15T10:30:00.000Z\"}",
        ],
    );
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].created_at, Some(1768473000000));
}

#[test]
fn created_at_none_when_missing() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    make_session_file(&pd, SessionOpts { first_prompt: Some("no timestamp"), ..Default::default() });
    let sessions = list_sessions(Some(&project_path), None, 0, false);
    assert_eq!(sessions[0].created_at, None);
}

// ---------------------------------------------------------------------------
// get_session_messages()
// ---------------------------------------------------------------------------

#[test]
fn messages_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_session_messages("not-a-uuid", None, None, 0).is_empty());
    assert!(get_session_messages("", None, None, 0).is_empty());
}

#[test]
fn messages_nonexistent_session() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_session_messages(&new_uuid(), None, None, 0).is_empty());
}

#[test]
fn messages_no_config_dir() {
    let _g = env_guard!();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path().join("nonexistent"));
    assert!(get_session_messages(&new_uuid(), None, None, 0).is_empty());
}

#[test]
fn messages_simple_chain() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, a1, u2, a2) = (new_uuid(), new_uuid(), new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &u1, None, &sid, Some(json!("hello"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hi!"))),
        tentry("user", &u2, Some(&a1), &sid, Some(json!("thanks"))),
        tentry("assistant", &a2, Some(&u2), &sid, Some(json!("welcome"))),
    ];
    write_transcript(&pd, &sid, &entries);

    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].message_type, MessageType::User);
    assert_eq!(msgs[0].uuid, u1);
    assert_eq!(msgs[0].session_id, sid);
    assert_eq!(msgs[0].message, json!({"role": "user", "content": "hello"}));
    assert_eq!(msgs[0].parent_tool_use_id, None);
    assert_eq!(msgs[1].message_type, MessageType::Assistant);
    assert_eq!(msgs[1].uuid, a1);
    assert_eq!(ids(&msgs), vec![u1, a1, u2, a2]);
}

#[test]
fn messages_filters_meta() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, meta, a1) = (new_uuid(), new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &u1, None, &sid, Some(json!("hello"))),
        with(tentry("user", &meta, Some(&u1), &sid, Some(json!("meta"))), "isMeta", json!(true)),
        tentry("assistant", &a1, Some(&meta), &sid, Some(json!("hi"))),
    ];
    write_transcript(&pd, &sid, &entries);

    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![u1, a1]);
}

#[test]
fn messages_filters_progress() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, prog, a1) = (new_uuid(), new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &u1, None, &sid, Some(json!("hello"))),
        tentry("progress", &prog, Some(&u1), &sid, None),
        tentry("assistant", &a1, Some(&prog), &sid, Some(json!("hi"))),
    ];
    write_transcript(&pd, &sid, &entries);
    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![u1, a1]);
}

#[test]
fn messages_keeps_compact_summary() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, a1) = (new_uuid(), new_uuid());
    let entries = vec![
        with(tentry("user", &u1, None, &sid, Some(json!("compact summary"))), "isCompactSummary", json!(true)),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hi"))),
    ];
    write_transcript(&pd, &sid, &entries);
    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].uuid, u1);
}

#[test]
fn messages_limit_and_offset() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let us: Vec<String> = (0..6).map(|_| new_uuid()).collect();
    let entries: Vec<Value> = us
        .iter()
        .enumerate()
        .map(|(i, uid)| {
            let parent = if i > 0 { Some(us[i - 1].as_str()) } else { None };
            let t = if i % 2 == 0 { "user" } else { "assistant" };
            tentry(t, uid, parent, &sid, Some(json!(format!("m{i}"))))
        })
        .collect();
    write_transcript(&pd, &sid, &entries);

    assert_eq!(get_session_messages(&sid, Some(&project_path), None, 0).len(), 6);
    let page = get_session_messages(&sid, Some(&project_path), Some(2), 0);
    assert_eq!(ids(&page), vec![us[0].clone(), us[1].clone()]);
    let page = get_session_messages(&sid, Some(&project_path), Some(2), 2);
    assert_eq!(ids(&page), vec![us[2].clone(), us[3].clone()]);
    let page = get_session_messages(&sid, Some(&project_path), None, 4);
    assert_eq!(ids(&page), vec![us[4].clone(), us[5].clone()]);
    assert_eq!(get_session_messages(&sid, Some(&project_path), Some(0), 0).len(), 6);
    assert!(get_session_messages(&sid, Some(&project_path), None, 100).is_empty());
}

#[test]
fn messages_picks_main_over_sidechain() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (root, main_leaf, side_leaf) = (new_uuid(), new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &root, None, &sid, Some(json!("root"))),
        tentry("assistant", &main_leaf, Some(&root), &sid, Some(json!("main"))),
        with(tentry("assistant", &side_leaf, Some(&root), &sid, Some(json!("side"))), "isSidechain", json!(true)),
    ];
    write_transcript(&pd, &sid, &entries);
    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![root, main_leaf]);
}

#[test]
fn messages_picks_latest_leaf_by_position() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (root, old_leaf, new_leaf) = (new_uuid(), new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &root, None, &sid, Some(json!("root"))),
        tentry("assistant", &old_leaf, Some(&root), &sid, Some(json!("old"))),
        tentry("assistant", &new_leaf, Some(&root), &sid, Some(json!("new"))),
    ];
    write_transcript(&pd, &sid, &entries);
    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![root, new_leaf]);
}

#[test]
fn messages_terminal_non_message_walked_back() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, a1, prog) = (new_uuid(), new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &u1, None, &sid, Some(json!("hi"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hello"))),
        tentry("progress", &prog, Some(&a1), &sid, None),
    ];
    write_transcript(&pd, &sid, &entries);
    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![u1, a1]);
}

#[test]
fn messages_corrupt_lines_skipped() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, a1) = (new_uuid(), new_uuid());
    let body = format!(
        "{}\nnot valid json {{{{\n\n{}\n",
        tentry("user", &u1, None, &sid, Some(json!("hi"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hello")))
    );
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();
    let msgs = get_session_messages(&sid, Some(&project_path), None, 0);
    assert_eq!(msgs.len(), 2);
}

#[test]
fn messages_search_all_projects_when_no_dir() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let _p1 = make_project_dir(&c, "/path/one");
    let p2 = make_project_dir(&c, "/path/two");
    let sid = new_uuid();
    let (u1, a1) = (new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &u1, None, &sid, Some(json!("hi"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hello"))),
    ];
    write_transcript(&p2, &sid, &entries);
    let msgs = get_session_messages(&sid, None, None, 0);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].uuid, u1);
}

#[test]
fn messages_cycle_detection() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, a1) = (new_uuid(), new_uuid());
    let entries = vec![
        tentry("user", &u1, Some(&a1), &sid, Some(json!("hi"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hello"))),
    ];
    write_transcript(&pd, &sid, &entries);
    assert!(get_session_messages(&sid, Some(&project_path), None, 0).is_empty());
}

#[test]
fn messages_empty_transcript_file() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    fs::write(pd.join(format!("{sid}.jsonl")), "").unwrap();
    assert!(get_session_messages(&sid, Some(&project_path), None, 0).is_empty());
}

#[test]
fn messages_ignores_non_transcript_types() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    let (u1, a1) = (new_uuid(), new_uuid());
    let body = format!(
        "{}\n{}\n{}\n",
        tentry("user", &u1, None, &sid, Some(json!("hi"))),
        json!({"type": "summary", "summary": "A nice chat"}),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hello")))
    );
    fs::write(pd.join(format!("{sid}.jsonl")), body).unwrap();
    assert_eq!(get_session_messages(&sid, Some(&project_path), None, 0).len(), 2);
}

// ---------------------------------------------------------------------------
// get_session_info()
// ---------------------------------------------------------------------------

#[test]
fn info_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_session_info("not-a-uuid", None).is_none());
    assert!(get_session_info("", None).is_none());
}

#[test]
fn info_nonexistent_session() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_session_info(&new_uuid(), None).is_none());
}

#[test]
fn info_no_config_dir() {
    let _g = env_guard!();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path().join("nonexistent"));
    assert!(get_session_info(&new_uuid(), None).is_none());
}

#[test]
fn info_found_with_directory() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = make_session_file(&pd, SessionOpts { first_prompt: Some("hello"), git_branch: Some("main"), ..Default::default() });

    let info = get_session_info(&sid, Some(&project_path)).unwrap();
    assert_eq!(info.session_id, sid);
    assert_eq!(info.summary, "hello");
    assert_eq!(info.git_branch.as_deref(), Some("main"));
}

#[test]
fn info_found_without_directory() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let pd = make_project_dir(&c, "/some/project");
    let sid = make_session_file(&pd, SessionOpts { first_prompt: Some("search all"), ..Default::default() });
    let info = get_session_info(&sid, None).unwrap();
    assert_eq!(info.session_id, sid);
    assert_eq!(info.summary, "search all");
}

#[test]
fn info_returns_none_for_sidechain() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = make_session_file(&pd, SessionOpts { first_prompt: Some("sidechain"), is_sidechain: true, ..Default::default() });
    assert!(get_session_info(&sid, Some(&project_path)).is_none());
}

#[test]
fn info_directory_not_containing_session() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_a = tmp.path().join("proj-a");
    let project_b = tmp.path().join("proj-b");
    fs::create_dir_all(&project_a).unwrap();
    fs::create_dir_all(&project_b).unwrap();
    let dir_a = make_project_dir(&c, &realpath(&project_a));
    make_project_dir(&c, &realpath(&project_b));
    let sid = make_session_file(&dir_a, SessionOpts { first_prompt: Some("in A only"), ..Default::default() });

    assert!(get_session_info(&sid, Some(&project_b)).is_none());
    assert!(get_session_info(&sid, None).is_some());
}

#[test]
fn info_includes_tag() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = new_uuid();
    write_lines(
        &pd,
        &sid,
        &[
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}",
            &format!("{{\"type\":\"tag\",\"tag\":\"urgent\",\"sessionId\":\"{sid}\"}}"),
        ],
    );
    let info = get_session_info(&sid, Some(&project_path)).unwrap();
    assert_eq!(info.tag.as_deref(), Some("urgent"));
}

// ---------------------------------------------------------------------------
// list_subagents() / get_subagent_messages()
// ---------------------------------------------------------------------------

/// Mirrors `_make_session_with_subagents`. Returns (session_id, subagents_dir).
fn make_session_with_subagents(
    c: &Config,
    project_path: &str,
    agent_ids: &[&str],
) -> (String, PathBuf) {
    let pd = make_project_dir(c, &realpath(Path::new(project_path)));
    let sid = make_session_file(&pd, SessionOpts::default());
    let subagents = pd.join(&sid).join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    for id in agent_ids {
        fs::write(
            subagents.join(format!("agent-{id}.jsonl")),
            "{\"type\":\"user\",\"uuid\":\"u\",\"parentUuid\":null}\n",
        )
        .unwrap();
    }
    (sid, subagents)
}

#[test]
fn subagents_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(list_subagents("not-a-uuid", None).is_empty());
    assert!(list_subagents("", None).is_empty());
}

#[test]
fn subagents_nonexistent_session() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(list_subagents(&new_uuid(), None).is_empty());
}

#[test]
fn subagents_session_exists_no_dir() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let pd = make_project_dir(&c, &realpath(&project_path));
    let sid = make_session_file(&pd, SessionOpts::default());
    assert!(list_subagents(&sid, Some(&project_path)).is_empty());
}

#[test]
fn subagents_empty_dir() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, _) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &[]);
    assert!(list_subagents(&sid, Some(&project_path)).is_empty());
}

#[test]
fn subagents_happy_path() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, _) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &["abc123", "def456"]);
    let mut result = list_subagents(&sid, Some(&project_path));
    result.sort();
    assert_eq!(result, vec!["abc123", "def456"]);
}

#[test]
fn subagents_ignores_non_agent_files() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &["keep"]);
    fs::write(subagents.join("agent-keep.meta.json"), "{}").unwrap();
    fs::write(subagents.join("other.jsonl"), "{}\n").unwrap();
    fs::write(subagents.join("agent-noext"), "{}").unwrap();
    assert_eq!(list_subagents(&sid, Some(&project_path)), vec!["keep"]);
}

#[test]
fn subagents_recurses_into_subdirectories() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &["top"]);
    let nested = subagents.join("workflows").join("run-1");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("agent-nested.jsonl"), "{}\n").unwrap();
    let mut result = list_subagents(&sid, Some(&project_path));
    result.sort();
    assert_eq!(result, vec!["nested", "top"]);
}

#[test]
fn subagents_searches_all_projects_without_directory() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let pd = make_project_dir(&c, "/some/project");
    let sid = make_session_file(&pd, SessionOpts::default());
    let subagents = pd.join(&sid).join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(subagents.join("agent-x.jsonl"), "{}\n").unwrap();
    assert_eq!(list_subagents(&sid, None), vec!["x"]);
}

#[test]
fn subagent_messages_invalid_session_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_subagent_messages("not-a-uuid", "abc", None, None, 0).is_empty());
    assert!(get_subagent_messages("", "abc", None, None, 0).is_empty());
}

#[test]
fn subagent_messages_empty_agent_id() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_subagent_messages(&new_uuid(), "", None, None, 0).is_empty());
}

#[test]
fn subagent_messages_nonexistent_session() {
    let _g = env_guard!();
    let _c = claude_config_dir();
    assert!(get_subagent_messages(&new_uuid(), "abc", None, None, 0).is_empty());
}

#[test]
fn subagent_messages_nonexistent_agent() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, _) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &["other"]);
    assert!(get_subagent_messages(&sid, "missing", Some(&project_path), None, 0).is_empty());
}

#[test]
fn subagent_messages_simple_chain() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &[]);
    let (u1, a1, u2, a2) = (new_uuid(), new_uuid(), new_uuid(), new_uuid());
    let entries = [
        tentry("user", &u1, None, &sid, Some(json!("task"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("working"))),
        tentry("user", &u2, Some(&a1), &sid, Some(json!("continue"))),
        tentry("assistant", &a2, Some(&u2), &sid, Some(json!("done"))),
    ];
    let body = entries.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(subagents.join("agent-abc.jsonl"), body).unwrap();

    let msgs = get_subagent_messages(&sid, "abc", Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![u1, a1, u2, a2]);
    assert_eq!(msgs[0].message_type, MessageType::User);
    assert_eq!(msgs[0].session_id, sid);
    assert_eq!(msgs[0].message, json!({"role": "user", "content": "task"}));
    assert_eq!(msgs[3].message_type, MessageType::Assistant);
}

#[test]
fn subagent_messages_nested_subdirectory() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &[]);
    let nested = subagents.join("workflows").join("run-1");
    fs::create_dir_all(&nested).unwrap();
    let (u1, a1) = (new_uuid(), new_uuid());
    let entries = [
        tentry("user", &u1, None, &sid, Some(json!("hi"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("hello"))),
    ];
    let body = entries.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(nested.join("agent-deep.jsonl"), body).unwrap();

    let msgs = get_subagent_messages(&sid, "deep", Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![u1, a1]);
}

#[test]
fn subagent_messages_skips_corrupt() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &[]);
    let (u1, a1) = (new_uuid(), new_uuid());
    let body = format!(
        "{}\nnot valid json {{\n\n{}\n",
        tentry("user", &u1, None, &sid, Some(json!("hi"))),
        tentry("assistant", &a1, Some(&u1), &sid, Some(json!("ok")))
    );
    fs::write(subagents.join("agent-x.jsonl"), body).unwrap();
    let msgs = get_subagent_messages(&sid, "x", Some(&project_path), None, 0);
    assert_eq!(ids(&msgs), vec![u1, a1]);
}

#[test]
fn subagent_messages_limit_and_offset() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &[]);
    let us: Vec<String> = (0..6).map(|_| new_uuid()).collect();
    let entries: Vec<Value> = us
        .iter()
        .enumerate()
        .map(|(i, uid)| {
            let parent = if i > 0 { Some(us[i - 1].as_str()) } else { None };
            let t = if i % 2 == 0 { "user" } else { "assistant" };
            tentry(t, uid, parent, &sid, Some(json!(format!("m{i}"))))
        })
        .collect();
    let body = entries.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(subagents.join("agent-p.jsonl"), body).unwrap();

    assert_eq!(get_subagent_messages(&sid, "p", Some(&project_path), None, 0).len(), 6);
    assert_eq!(ids(&get_subagent_messages(&sid, "p", Some(&project_path), Some(2), 0)), us[..2].to_vec());
    assert_eq!(ids(&get_subagent_messages(&sid, "p", Some(&project_path), Some(2), 2)), us[2..4].to_vec());
    assert_eq!(ids(&get_subagent_messages(&sid, "p", Some(&project_path), None, 4)), us[4..].to_vec());
    assert_eq!(get_subagent_messages(&sid, "p", Some(&project_path), Some(0), 0).len(), 6);
}

#[test]
fn subagent_messages_empty_file() {
    let _g = env_guard!();
    let c = claude_config_dir();
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().join("proj");
    fs::create_dir_all(&project_path).unwrap();
    let (sid, subagents) = make_session_with_subagents(&c, project_path.to_str().unwrap(), &[]);
    fs::write(subagents.join("agent-empty.jsonl"), "").unwrap();
    assert!(get_subagent_messages(&sid, "empty", Some(&project_path), None, 0).is_empty());
}
