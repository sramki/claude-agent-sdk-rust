//! Parity tests for the incremental session-summary fold.
//!
//! Ported from upstream `tests/test_session_summary.py` (claude-agent-sdk
//! Python). Covers `fold_session_summary`, `summary_entry_to_sdk_info`, and
//! `InMemorySessionStore::list_session_summaries`.
//!
//! Deliberately omitted (out of scope for this crate's current surface):
//!   * `TestListSessionsFromStoreFastPath` — depends on `list_sessions_from_store`,
//!     the store-mirror/resume runtime fast path, which is not yet ported.
//!   * `TestParityWithLiteParse` — depends on the `pub(crate)` lite-parse helpers
//!     (`_entries_to_jsonl` / `_jsonl_to_lite` / `_parse_session_info_from_lite`),
//!     which are not reachable from an integration test.

use claude_agent_sdk_rs::types::{SessionKey, SessionStore, SessionStoreEntry, SessionSummaryEntry};
use claude_agent_sdk_rs::{fold_session_summary, summary_entry_to_sdk_info, InMemorySessionStore};
use serde_json::{json, Map, Value};

const PROJECT_KEY: &str = "-workspace-project";
const KEY_SID: &str = "11111111-1111-4111-8111-111111111111";

fn obj(v: Value) -> SessionStoreEntry {
    v.as_object().unwrap().clone()
}

fn key(sid: &str) -> SessionKey {
    SessionKey {
        project_key: PROJECT_KEY.into(),
        session_id: sid.into(),
        subpath: None,
    }
}

/// Mirror of the upstream `_user(text, ts, **extra)` helper for string content.
fn user_ts(text: &str, ts: &str) -> SessionStoreEntry {
    obj(json!({
        "type": "user",
        "timestamp": ts,
        "message": {"role": "user", "content": text},
    }))
}

fn user(text: &str) -> SessionStoreEntry {
    user_ts(text, "2024-01-01T00:00:00.000Z")
}

fn get_str<'a>(data: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    data.get(k).and_then(Value::as_str)
}

// ---------------------------------------------------------------------------
// fold_session_summary
// ---------------------------------------------------------------------------

#[test]
fn fold_init_from_none() {
    let s = fold_session_summary(None, &key(KEY_SID), &[]);
    assert_eq!(s.session_id, KEY_SID);
    assert_eq!(s.mtime, 0);
    assert!(s.data.is_empty());
}

#[test]
fn fold_set_once_fields_freeze() {
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[
            obj(json!({
                "type": "x",
                "timestamp": "2024-01-01T00:00:00.000Z",
                "cwd": "/a",
                "isSidechain": false,
            })),
            obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:05.000Z", "cwd": "/b"})),
        ],
    );
    assert_eq!(s.data.get("created_at").and_then(Value::as_i64), Some(1704067200000));
    assert_eq!(get_str(&s.data, "cwd"), Some("/a"));
    assert_eq!(s.data.get("is_sidechain"), Some(&Value::Bool(false)));

    // Second append must not overwrite set-once fields.
    let s2 = fold_session_summary(
        Some(&s),
        &key(KEY_SID),
        &[obj(json!({
            "type": "x",
            "timestamp": "2024-01-02T00:00:00.000Z",
            "cwd": "/c",
            "isSidechain": true,
        }))],
    );
    assert_eq!(s2.data.get("created_at").and_then(Value::as_i64), Some(1704067200000));
    assert_eq!(get_str(&s2.data, "cwd"), Some("/a"));
    assert_eq!(s2.data.get("is_sidechain"), Some(&Value::Bool(false)));
}

#[test]
fn fold_last_wins_overwrite() {
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[
            obj(json!({
                "type": "x",
                "timestamp": "2024-01-01T00:00:00Z",
                "customTitle": "t1",
                "gitBranch": "main",
            })),
            obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:01Z", "customTitle": "t2"})),
        ],
    );
    assert_eq!(get_str(&s.data, "custom_title"), Some("t2"));
    assert_eq!(get_str(&s.data, "git_branch"), Some("main"));

    let s2 = fold_session_summary(
        Some(&s),
        &key(KEY_SID),
        &[obj(json!({
            "type": "x",
            "aiTitle": "ai",
            "lastPrompt": "lp",
            "summary": "sm",
            "gitBranch": "dev",
        }))],
    );
    assert_eq!(get_str(&s2.data, "custom_title"), Some("t2"));
    assert_eq!(get_str(&s2.data, "ai_title"), Some("ai"));
    assert_eq!(get_str(&s2.data, "last_prompt"), Some("lp"));
    assert_eq!(get_str(&s2.data, "summary_hint"), Some("sm"));
    assert_eq!(get_str(&s2.data, "git_branch"), Some("dev"));
}

#[test]
fn fold_mtime_not_derived_from_entries() {
    // New session: fold returns mtime=0 placeholder, adapter must stamp.
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[
            obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:05.000Z"})),
            obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:01.000Z"})),
        ],
    );
    assert_eq!(s.mtime, 0);

    // Carry-over: prev mtime preserved verbatim regardless of entry timestamps.
    let prev = SessionSummaryEntry {
        session_id: KEY_SID.into(),
        mtime: 42,
        data: Map::new(),
    };
    let s2 = fold_session_summary(
        Some(&prev),
        &key(KEY_SID),
        &[obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:10.000Z"}))],
    );
    assert_eq!(s2.mtime, 42);
}

#[test]
fn fold_tag_set_and_clear() {
    let s = fold_session_summary(None, &key(KEY_SID), &[obj(json!({"type": "tag", "tag": "wip"}))]);
    assert_eq!(get_str(&s.data, "tag"), Some("wip"));

    let s2 = fold_session_summary(Some(&s), &key(KEY_SID), &[obj(json!({"type": "tag", "tag": ""}))]);
    assert!(!s2.data.contains_key("tag"));

    // Non-tag entries with a "tag" key (e.g. tool_use input) are ignored.
    let s3 = fold_session_summary(Some(&s), &key(KEY_SID), &[obj(json!({"type": "user", "tag": "ignored"}))]);
    assert_eq!(get_str(&s3.data, "tag"), Some("wip"));
}

#[test]
fn fold_sidechain_from_first_entry() {
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:00Z", "isSidechain": true}))],
    );
    assert_eq!(s.data.get("is_sidechain"), Some(&Value::Bool(true)));
}

#[test]
fn fold_sidechain_latched_when_first_entry_lacks_timestamp() {
    // is_sidechain must latch on entry 0 even if its timestamp is
    // absent/unparseable, so entry 1 cannot overwrite it to False.
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[
            obj(json!({"type": "user", "isSidechain": true})),
            obj(json!({"type": "x", "timestamp": "2024-01-01T00:00:00Z"})),
        ],
    );
    assert_eq!(s.data.get("is_sidechain"), Some(&Value::Bool(true)));
    // created_at still picks up the first parseable timestamp.
    assert_eq!(s.data.get("created_at").and_then(Value::as_i64), Some(1704067200000));
}

#[test]
fn fold_first_prompt_skips_meta_tool_result_and_compact() {
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[
            obj(json!({
                "type": "user",
                "timestamp": "2024-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": "ignored meta"},
                "isMeta": true,
            })),
            obj(json!({
                "type": "user",
                "timestamp": "2024-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": "ignored compact"},
                "isCompactSummary": true,
            })),
            obj(json!({
                "type": "user",
                "timestamp": "2024-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "x", "content": "res"}]},
            })),
            user("real first"),
            user("not me"),
        ],
    );
    assert_eq!(get_str(&s.data, "first_prompt"), Some("real first"));
    assert_eq!(s.data.get("first_prompt_locked"), Some(&Value::Bool(true)));
}

#[test]
fn fold_first_prompt_command_fallback() {
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[
            user("<command-name>/init</command-name> stuff"),
            user("<command-name>/second</command-name>"),
        ],
    );
    assert_ne!(s.data.get("first_prompt_locked"), Some(&Value::Bool(true)));
    assert_eq!(get_str(&s.data, "command_fallback"), Some("/init"));

    // A later real prompt locks it.
    let s2 = fold_session_summary(Some(&s), &key(KEY_SID), &[user("now real")]);
    assert_eq!(get_str(&s2.data, "first_prompt"), Some("now real"));
    assert_eq!(s2.data.get("first_prompt_locked"), Some(&Value::Bool(true)));
}

#[test]
fn fold_first_prompt_skip_pattern() {
    let s = fold_session_summary(
        None,
        &key(KEY_SID),
        &[user("<local-command-stdout> some output"), user("hello")],
    );
    assert_eq!(get_str(&s.data, "first_prompt"), Some("hello"));
}

#[test]
fn fold_first_prompt_truncated() {
    let s = fold_session_summary(None, &key(KEY_SID), &[user(&"x".repeat(300))]);
    let fp = get_str(&s.data, "first_prompt").unwrap();
    assert!(fp.chars().count() <= 201);
    assert!(fp.ends_with('\u{2026}'));
}

#[test]
fn fold_prev_is_not_mutated() {
    let prev = SessionSummaryEntry {
        session_id: "a".into(),
        mtime: 5,
        data: Map::new(),
    };
    let _ = fold_session_summary(Some(&prev), &key(KEY_SID), &[obj(json!({"type": "x", "customTitle": "t"}))]);
    assert_eq!(prev.session_id, "a");
    assert_eq!(prev.mtime, 5);
    assert!(prev.data.is_empty());
}

// ---------------------------------------------------------------------------
// summary_entry_to_sdk_info
// ---------------------------------------------------------------------------

fn summary(data: Map<String, Value>, mtime: i64) -> SessionSummaryEntry {
    SessionSummaryEntry {
        session_id: "s".into(),
        mtime,
        data,
    }
}

#[test]
fn info_sidechain_returns_none() {
    let entry = summary(obj(json!({"is_sidechain": true, "custom_title": "t"})), 1);
    assert!(summary_entry_to_sdk_info(&entry, None).is_none());
}

#[test]
fn info_empty_summary_returns_none() {
    let entry = summary(Map::new(), 1);
    assert!(summary_entry_to_sdk_info(&entry, None).is_none());
}

#[test]
fn info_precedence_chain() {
    let mut data = obj(json!({
        "first_prompt": "fp",
        "first_prompt_locked": true,
        "command_fallback": "/cmd",
        "summary_hint": "sh",
        "last_prompt": "lp",
        "ai_title": "ai",
        "custom_title": "ct",
    }));

    let info = summary_entry_to_sdk_info(&summary(data.clone(), 1), None).unwrap();
    assert_eq!(info.summary, "ct");
    assert_eq!(info.custom_title.as_deref(), Some("ct"));

    data.remove("custom_title");
    let info = summary_entry_to_sdk_info(&summary(data.clone(), 1), None).unwrap();
    assert_eq!(info.summary, "ai");
    assert_eq!(info.custom_title.as_deref(), Some("ai"));

    data.remove("ai_title");
    let info = summary_entry_to_sdk_info(&summary(data.clone(), 1), None).unwrap();
    assert_eq!(info.summary, "lp");
    assert_eq!(info.custom_title, None);

    data.remove("last_prompt");
    let info = summary_entry_to_sdk_info(&summary(data.clone(), 1), None).unwrap();
    assert_eq!(info.summary, "sh");

    data.remove("summary_hint");
    let info = summary_entry_to_sdk_info(&summary(data.clone(), 1), None).unwrap();
    assert_eq!(info.summary, "fp");
    assert_eq!(info.first_prompt.as_deref(), Some("fp"));

    data.insert("first_prompt_locked".into(), Value::Bool(false));
    let info = summary_entry_to_sdk_info(&summary(data.clone(), 1), None).unwrap();
    assert_eq!(info.summary, "/cmd");
    assert_eq!(info.first_prompt.as_deref(), Some("/cmd"));
}

#[test]
fn info_cwd_fallback_to_project_path() {
    let info = summary_entry_to_sdk_info(&summary(obj(json!({"custom_title": "t"})), 1), Some("/proj")).unwrap();
    assert_eq!(info.cwd.as_deref(), Some("/proj"));

    let info2 =
        summary_entry_to_sdk_info(&summary(obj(json!({"custom_title": "t", "cwd": "/own"})), 1), Some("/proj")).unwrap();
    assert_eq!(info2.cwd.as_deref(), Some("/own"));
}

#[test]
fn info_field_passthrough() {
    let info = summary_entry_to_sdk_info(
        &summary(
            obj(json!({
                "custom_title": "t",
                "git_branch": "main",
                "tag": "wip",
                "created_at": 50,
            })),
            99,
        ),
        None,
    )
    .unwrap();
    assert_eq!(info.session_id, "s");
    assert_eq!(info.last_modified, 99);
    assert_eq!(info.git_branch.as_deref(), Some("main"));
    assert_eq!(info.tag.as_deref(), Some("wip"));
    assert_eq!(info.created_at, Some(50));
    // file_size is local-JSONL-only; store-backed summaries always None.
    assert_eq!(info.file_size, None);
}

// ---------------------------------------------------------------------------
// InMemorySessionStore::list_session_summaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_tracks_appends() {
    let store = InMemorySessionStore::new();
    let a = key("a");
    let b = key("b");
    store.append(&a, &[user_ts("hello a", "2024-01-01T00:00:00Z")]).await.unwrap();
    store.append(&a, &[obj(json!({"type": "x", "customTitle": "Title A"}))]).await.unwrap();
    store.append(&b, &[user_ts("hello b", "2024-01-02T00:00:00Z")]).await.unwrap();

    let summaries: std::collections::HashMap<String, SessionSummaryEntry> = store
        .list_session_summaries(PROJECT_KEY)
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.session_id.clone(), s))
        .collect();

    assert_eq!(summaries.keys().cloned().collect::<std::collections::BTreeSet<_>>(),
        ["a".to_string(), "b".to_string()].into_iter().collect());
    assert_eq!(get_str(&summaries["a"].data, "custom_title"), Some("Title A"));
    assert_eq!(get_str(&summaries["a"].data, "first_prompt"), Some("hello a"));
    assert_eq!(get_str(&summaries["b"].data, "first_prompt"), Some("hello b"));
}

#[tokio::test]
async fn store_subpath_appends_ignored() {
    let store = InMemorySessionStore::new();
    let main = key("m");
    let sub = SessionKey {
        project_key: PROJECT_KEY.into(),
        session_id: "m".into(),
        subpath: Some("subagents/agent-1".into()),
    };
    store.append(&main, &[user("main prompt")]).await.unwrap();
    store
        .append(&sub, &[user("sub prompt"), obj(json!({"type": "x", "customTitle": "sub"}))])
        .await
        .unwrap();

    let summaries = store.list_session_summaries(PROJECT_KEY).await.unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(get_str(&summaries[0].data, "first_prompt"), Some("main prompt"));
    assert!(!summaries[0].data.contains_key("custom_title"));
}

#[tokio::test]
async fn store_delete_drops_summary() {
    let store = InMemorySessionStore::new();
    let k = key("x");
    store.append(&k, &[user("hi")]).await.unwrap();
    assert_eq!(store.list_session_summaries(PROJECT_KEY).await.unwrap().len(), 1);
    store.delete(&k).await.unwrap();
    assert!(store.list_session_summaries(PROJECT_KEY).await.unwrap().is_empty());
}

#[tokio::test]
async fn store_project_isolation() {
    let store = InMemorySessionStore::new();
    let ka = SessionKey { project_key: "A".into(), session_id: "s".into(), subpath: None };
    let kb = SessionKey { project_key: "B".into(), session_id: "s".into(), subpath: None };
    store.append(&ka, &[user("a")]).await.unwrap();
    store.append(&kb, &[user("b")]).await.unwrap();
    assert_eq!(store.list_session_summaries("A").await.unwrap().len(), 1);
    assert_eq!(store.list_session_summaries("B").await.unwrap().len(), 1);
    assert!(store.list_session_summaries("C").await.unwrap().is_empty());
}
