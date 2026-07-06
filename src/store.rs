//! In-memory reference [`SessionStore`] and the incremental summary fold.
//!
//! Port of `_internal/session_store.py` and `_internal/session_summary.py`:
//! [`InMemorySessionStore`] (a working reference adapter), [`fold_session_summary`]
//! (maintain a [`SessionSummaryEntry`] sidecar inside `append` without re-reading),
//! [`summary_entry_to_sdk_info`], and [`file_path_to_session_key`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::error::Result;
use crate::parse::{command_name, matches_skip_first_prompt, parse_iso_to_epoch_ms};
use crate::types::{
    SessionInfo, SessionKey, SessionListSubkeysKey, SessionStore, SessionStoreEntry,
    SessionStoreListEntry, SessionSummaryEntry,
};

/// Last-wins string fields: JSONL entry key → summary `data` key.
const LAST_WINS_FIELDS: [(&str, &str); 5] = [
    ("customTitle", "custom_title"),
    ("aiTitle", "ai_title"),
    ("lastPrompt", "last_prompt"),
    ("summary", "summary_hint"),
    ("gitBranch", "git_branch"),
];

fn key_to_string(key: &SessionKey) -> String {
    match &key.subpath {
        Some(sub) if !sub.is_empty() => {
            format!("{}/{}/{}", key.project_key, key.session_id, sub)
        }
        _ => format!("{}/{}", key.project_key, key.session_id),
    }
}

fn as_str<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn is_true(entry: &Map<String, Value>, key: &str) -> bool {
    entry.get(key) == Some(&Value::Bool(true))
}

/// Extracts text strings from a `user` entry's message content. Mirrors
/// `_entry_text_blocks`.
fn entry_text_blocks(entry: &Map<String, Value>) -> Vec<String> {
    let Some(message) = entry.get("message").and_then(Value::as_object) else {
        return Vec::new();
    };
    match message.get("content") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                let b = b.as_object()?;
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    b.get("text").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Folds first-prompt state for one entry. Mirrors `_fold_first_prompt`.
fn fold_first_prompt(data: &mut Map<String, Value>, entry: &Map<String, Value>) {
    if data.get("first_prompt_locked") == Some(&Value::Bool(true)) {
        return;
    }
    if entry.get("type").and_then(Value::as_str) != Some("user") {
        return;
    }
    if is_true(entry, "isMeta") || is_true(entry, "isCompactSummary") {
        return;
    }
    if let Some(content) = entry
        .get("message")
        .and_then(Value::as_object)
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        let has_tool_result = content.iter().any(|b| {
            b.as_object()
                .and_then(|o| o.get("type"))
                .and_then(Value::as_str)
                == Some("tool_result")
        });
        if has_tool_result {
            return;
        }
    }

    for raw in entry_text_blocks(entry) {
        let result = raw.replace('\n', " ");
        let result = result.trim();
        if result.is_empty() {
            continue;
        }
        if let Some(cmd) = command_name(result) {
            if !data.contains_key("command_fallback") {
                data.insert("command_fallback".into(), Value::String(cmd));
            }
            continue;
        }
        if matches_skip_first_prompt(result) {
            continue;
        }
        let final_text = if result.chars().count() > 200 {
            let truncated: String = result.chars().take(200).collect();
            format!("{}\u{2026}", truncated.trim_end())
        } else {
            result.to_string()
        };
        data.insert("first_prompt".into(), Value::String(final_text));
        data.insert("first_prompt_locked".into(), Value::Bool(true));
        return;
    }
}

/// Folds a batch of appended entries into the running summary for `key`.
/// Mirrors `fold_session_summary`. Do not call for keys with a `subpath`.
pub fn fold_session_summary(
    prev: Option<&SessionSummaryEntry>,
    key: &SessionKey,
    entries: &[SessionStoreEntry],
) -> SessionSummaryEntry {
    let mut summary = match prev {
        Some(p) => SessionSummaryEntry {
            session_id: p.session_id.clone(),
            mtime: p.mtime,
            data: p.data.clone(),
        },
        None => SessionSummaryEntry {
            session_id: key.session_id.clone(),
            mtime: 0,
            data: Map::new(),
        },
    };
    let data = &mut summary.data;

    for entry in entries {
        let ms = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_to_epoch_ms);

        if !data.contains_key("is_sidechain") {
            data.insert("is_sidechain".into(), Value::Bool(is_true(entry, "isSidechain")));
        }
        if !data.contains_key("created_at") {
            if let Some(ms) = ms {
                data.insert("created_at".into(), Value::from(ms));
            }
        }
        if !data.contains_key("cwd") {
            if let Some(cwd) = as_str(entry, "cwd").filter(|s| !s.is_empty()) {
                data.insert("cwd".into(), Value::String(cwd.to_string()));
            }
        }

        fold_first_prompt(data, entry);

        for (src, dst) in LAST_WINS_FIELDS {
            if let Some(val) = as_str(entry, src) {
                data.insert(dst.into(), Value::String(val.to_string()));
            }
        }

        if entry.get("type").and_then(Value::as_str) == Some("tag") {
            match as_str(entry, "tag").filter(|s| !s.is_empty()) {
                Some(tag) => {
                    data.insert("tag".into(), Value::String(tag.to_string()));
                }
                None => {
                    data.remove("tag");
                }
            }
        }
    }

    summary
}

/// Converts a [`SessionSummaryEntry`] to a [`SessionInfo`]. Returns `None` for
/// sidechain sessions or sessions with no extractable summary. Mirrors
/// `summary_entry_to_sdk_info`.
pub fn summary_entry_to_sdk_info(
    entry: &SessionSummaryEntry,
    project_path: Option<&str>,
) -> Option<SessionInfo> {
    let data = &entry.data;
    if data.get("is_sidechain") == Some(&Value::Bool(true)) {
        return None;
    }

    let str_field = |k: &str| data.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());

    let first_prompt = if data.get("first_prompt_locked") == Some(&Value::Bool(true)) {
        str_field("first_prompt")
    } else {
        str_field("command_fallback")
    }
    .map(str::to_string);

    let custom_title = str_field("custom_title")
        .or_else(|| str_field("ai_title"))
        .map(str::to_string);

    let summary = custom_title
        .clone()
        .or_else(|| str_field("last_prompt").map(str::to_string))
        .or_else(|| str_field("summary_hint").map(str::to_string))
        .or_else(|| first_prompt.clone())?;

    Some(SessionInfo {
        session_id: entry.session_id.clone(),
        summary,
        last_modified: entry.mtime,
        file_size: None,
        custom_title,
        first_prompt,
        git_branch: str_field("git_branch").map(str::to_string),
        cwd: str_field("cwd")
            .map(str::to_string)
            .or_else(|| project_path.filter(|p| !p.is_empty()).map(str::to_string)),
        tag: str_field("tag").map(str::to_string),
        created_at: data.get("created_at").and_then(Value::as_i64),
    })
}

/// Derives a [`SessionKey`] from an absolute transcript file path. Mirrors
/// `file_path_to_session_key`. Returns `None` for paths not under
/// `projects_dir` or with an unrecognized shape.
pub fn file_path_to_session_key(file_path: &Path, projects_dir: &Path) -> Option<SessionKey> {
    let rel = file_path.strip_prefix(projects_dir).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let project_key = parts[0].clone();
    let second = &parts[1];

    // Main transcript: <project_key>/<session_id>.jsonl
    if parts.len() == 2 {
        if let Some(sid) = second.strip_suffix(".jsonl") {
            return Some(SessionKey {
                project_key,
                session_id: sid.to_string(),
                subpath: None,
            });
        }
        return None;
    }

    // Subagent transcript: <project_key>/<session_id>/subagents/.../agent-<id>.jsonl
    if parts.len() >= 4 {
        let mut subpath_parts: Vec<String> = parts[2..].to_vec();
        if let Some(last) = subpath_parts.last_mut() {
            if let Some(stripped) = last.strip_suffix(".jsonl") {
                *last = stripped.to_string();
            }
        }
        return Some(SessionKey {
            project_key,
            session_id: second.clone(),
            subpath: Some(subpath_parts.join("/")),
        });
    }

    None
}

// ---------------------------------------------------------------------------
// InMemorySessionStore
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Inner {
    store: HashMap<String, Vec<SessionStoreEntry>>,
    mtimes: HashMap<String, i64>,
    summaries: HashMap<(String, String), SessionSummaryEntry>,
    last_mtime: i64,
}

/// In-memory reference [`SessionStore`] for testing and development. Data is
/// lost when the process exits. Mirrors `InMemorySessionStore`.
#[derive(Default)]
pub struct InMemorySessionStore {
    inner: Mutex<Inner>,
}

impl InMemorySessionStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn next_mtime(inner: &mut Inner) -> i64 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let stamped = if now_ms <= inner.last_mtime {
            inner.last_mtime + 1
        } else {
            now_ms
        };
        inner.last_mtime = stamped;
        stamped
    }

    /// Test helper — all entries for a key (empty if absent).
    pub fn get_entries(&self, key: &SessionKey) -> Vec<SessionStoreEntry> {
        self.inner
            .lock()
            .unwrap()
            .store
            .get(&key_to_string(key))
            .cloned()
            .unwrap_or_default()
    }

    /// Test helper — number of stored main-transcript sessions.
    pub fn size(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .store
            .keys()
            .filter(|k| {
                k.find('/')
                    .map(|i| !k[i + 1..].contains('/'))
                    .unwrap_or(false)
            })
            .count()
    }

    /// Test helper — clear all data.
    pub fn clear(&self) {
        *self.inner.lock().unwrap() = Inner::default();
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn append(&self, key: &SessionKey, entries: &[SessionStoreEntry]) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let k = key_to_string(key);
        inner.store.entry(k.clone()).or_default().extend_from_slice(entries);
        let now_ms = Self::next_mtime(&mut inner);
        if key.subpath.is_none() {
            let sk = (key.project_key.clone(), key.session_id.clone());
            let prev = inner.summaries.get(&sk);
            let mut folded = fold_session_summary(prev, key, entries);
            folded.mtime = now_ms;
            inner.summaries.insert(sk, folded);
        }
        inner.mtimes.insert(k, now_ms);
        Ok(())
    }

    async fn load(&self, key: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Ok(self.inner.lock().unwrap().store.get(&key_to_string(key)).cloned())
    }

    async fn list_sessions(&self, project_key: &str) -> Result<Vec<SessionStoreListEntry>> {
        let inner = self.inner.lock().unwrap();
        let prefix = format!("{project_key}/");
        let mut results = Vec::new();
        for k in inner.store.keys() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.contains('/') {
                    results.push(SessionStoreListEntry {
                        session_id: rest.to_string(),
                        mtime: inner.mtimes.get(k).copied().unwrap_or(0),
                    });
                }
            }
        }
        Ok(results)
    }

    async fn list_session_summaries(
        &self,
        project_key: &str,
    ) -> Result<Vec<SessionSummaryEntry>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .summaries
            .iter()
            .filter(|((pk, _), _)| pk == project_key)
            .map(|(_, s)| s.clone())
            .collect())
    }

    async fn delete(&self, key: &SessionKey) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let k = key_to_string(key);
        inner.store.remove(&k);
        inner.mtimes.remove(&k);
        if key.subpath.is_none() {
            inner
                .summaries
                .remove(&(key.project_key.clone(), key.session_id.clone()));
            let prefix = format!("{}/{}/", key.project_key, key.session_id);
            let to_remove: Vec<String> = inner
                .store
                .keys()
                .filter(|sk| sk.starts_with(&prefix))
                .cloned()
                .collect();
            for sk in to_remove {
                inner.store.remove(&sk);
                inner.mtimes.remove(&sk);
            }
        }
        Ok(())
    }

    async fn list_subkeys(&self, key: &SessionListSubkeysKey) -> Result<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        let prefix = format!("{}/{}/", key.project_key, key.session_id);
        Ok(inner
            .store
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(v: Value) -> SessionStoreEntry {
        v.as_object().unwrap().clone()
    }

    fn key(pk: &str, sid: &str) -> SessionKey {
        SessionKey {
            project_key: pk.into(),
            session_id: sid.into(),
            subpath: None,
        }
    }

    #[tokio::test]
    async fn append_load_roundtrip() {
        let store = InMemorySessionStore::new();
        let k = key("proj", "s1");
        store
            .append(&k, &[entry(json!({"type": "user", "uuid": "u1"}))])
            .await
            .unwrap();
        let loaded = store.load(&k).await.unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(store.load(&key("proj", "missing")).await.unwrap().is_none());
        assert_eq!(store.size(), 1);
    }

    #[tokio::test]
    async fn summaries_track_title_and_prompt() {
        let store = InMemorySessionStore::new();
        let k = key("proj", "s1");
        store
            .append(
                &k,
                &[entry(json!({"type": "user", "message": {"role": "user", "content": "first question"}}))],
            )
            .await
            .unwrap();
        store
            .append(&k, &[entry(json!({"type": "custom-title", "customTitle": "My Title"}))])
            .await
            .unwrap();

        let summaries = store.list_session_summaries("proj").await.unwrap();
        assert_eq!(summaries.len(), 1);
        let info = summary_entry_to_sdk_info(&summaries[0], None).unwrap();
        assert_eq!(info.summary, "My Title");
        assert_eq!(info.first_prompt.as_deref(), Some("first question"));
        assert_eq!(info.custom_title.as_deref(), Some("My Title"));
    }

    #[tokio::test]
    async fn delete_cascades_to_subkeys() {
        let store = InMemorySessionStore::new();
        store.append(&key("p", "s"), &[entry(json!({"type": "user"}))]).await.unwrap();
        let sub = SessionKey {
            project_key: "p".into(),
            session_id: "s".into(),
            subpath: Some("subagents/agent-a".into()),
        };
        store.append(&sub, &[entry(json!({"type": "user"}))]).await.unwrap();
        assert_eq!(store.list_subkeys(&SessionListSubkeysKey { project_key: "p".into(), session_id: "s".into() }).await.unwrap().len(), 1);

        store.delete(&key("p", "s")).await.unwrap();
        assert!(store.load(&sub).await.unwrap().is_none());
        assert_eq!(store.size(), 0);
    }

    #[test]
    fn file_path_key_main_and_subagent() {
        let projects = Path::new("/home/u/.claude/projects");
        let main = file_path_to_session_key(
            &projects.join("-proj/abc.jsonl"),
            projects,
        )
        .unwrap();
        assert_eq!(main.session_id, "abc");
        assert_eq!(main.subpath, None);

        let sub = file_path_to_session_key(
            &projects.join("-proj/abc/subagents/agent-x.jsonl"),
            projects,
        )
        .unwrap();
        assert_eq!(sub.session_id, "abc");
        assert_eq!(sub.subpath.as_deref(), Some("subagents/agent-x"));
    }

    #[test]
    fn fold_clears_tag_on_empty() {
        let k = key("p", "s");
        let s1 = fold_session_summary(None, &k, &[entry(json!({"type": "tag", "tag": "x"}))]);
        assert_eq!(s1.data.get("tag").and_then(Value::as_str), Some("x"));
        let s2 = fold_session_summary(Some(&s1), &k, &[entry(json!({"type": "tag", "tag": ""}))]);
        assert!(s2.data.get("tag").is_none());
    }
}
