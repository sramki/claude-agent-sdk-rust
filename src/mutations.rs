//! Portable session mutation functions.
//!
//! Faithful port of the local-filesystem functions in
//! `_internal/session_mutations.py`: `rename_session` / `tag_session` append
//! typed metadata entries to the session JSONL; `delete_session` removes the
//! file (and subagent dir); `fork_session` copies the transcript with fresh
//! UUIDs. Directory resolution matches the reader. The `*_via_store` async
//! variants run the same transforms against a [`SessionStore`].
//!
//! `serde_json`'s `preserve_order` feature keeps `type` first in emitted
//! entries, which the reader's `{"type":"tag"` prefix scan relies on.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::parse::{
    extract_first_prompt_from_head, extract_last_json_string_field, LITE_READ_BUF_SIZE,
};
use crate::paths::{
    canonicalize_path, find_project_dir, projects_dir, sanitize_path, validate_uuid, worktree_paths,
};
use crate::types::{SessionKey, SessionStore, SessionStoreEntry};

const TRANSCRIPT_TYPES: [&str; 5] = ["user", "assistant", "attachment", "system", "progress"];

/// Derives the [`SessionKey`](crate::types::SessionKey) `project_key` for a
/// directory (realpath + NFC + djb2-hashed sanitize). Mirrors
/// `project_key_for_directory`. Defaults to the current directory.
pub fn project_key_for_directory(directory: Option<&Path>) -> String {
    let canonical = canonicalize_path(directory.unwrap_or_else(|| Path::new(".")));
    sanitize_path(&canonical)
}

/// Renames a session by appending a `custom-title` entry. Mirrors
/// `rename_session`. The reader reads the last custom-title, so repeated calls
/// are safe.
pub fn rename_session(session_id: &str, title: &str, directory: Option<&Path>) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let stripped = title.trim();
    if stripped.is_empty() {
        return Err(Error::Invalid("title must be non-empty".into()));
    }
    let data = json!({"type": "custom-title", "customTitle": stripped, "sessionId": session_id})
        .to_string()
        + "\n";
    append_to_session(session_id, &data, directory)
}

/// Tags a session (pass `None` to clear). Mirrors `tag_session`. Tags are
/// Unicode-sanitized; the reader reads the last tag.
pub fn tag_session(session_id: &str, tag: Option<&str>, directory: Option<&Path>) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let tag_value: String = match tag {
        None => String::new(),
        Some(t) => {
            let sanitized = sanitize_unicode(t);
            let sanitized = sanitized.trim();
            if sanitized.is_empty() {
                return Err(Error::Invalid(
                    "tag must be non-empty (use None to clear)".into(),
                ));
            }
            sanitized.to_string()
        }
    };
    let data = json!({"type": "tag", "tag": tag_value, "sessionId": session_id}).to_string() + "\n";
    append_to_session(session_id, &data, directory)
}

/// Deletes a session by removing its JSONL file and subagent transcripts.
/// Mirrors `delete_session`. Hard delete.
pub fn delete_session(session_id: &str, directory: Option<&Path>) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let path = find_session_file_with_dir(session_id, directory)
        .map(|(p, _)| p)
        .ok_or_else(|| Error::SessionNotFound(format!("Session {session_id} not found")))?;
    std::fs::remove_file(&path)
        .map_err(|_| Error::SessionNotFound(format!("Session {session_id} not found")))?;
    // Subagent transcripts live in a sibling {session_id}/ dir; often absent.
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir_all(parent.join(session_id));
    }
    Ok(())
}

/// The result of a fork operation. Mirrors `ForkSessionResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkSessionResult {
    /// UUID of the new forked session.
    pub session_id: String,
}

/// Forks a session into a new branch with fresh UUIDs. Mirrors `fork_session`.
pub fn fork_session(
    session_id: &str,
    directory: Option<&Path>,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
) -> Result<ForkSessionResult> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    if let Some(up) = up_to_message_id {
        if !validate_uuid(up) {
            return Err(Error::Invalid(format!("Invalid up_to_message_id: {up}")));
        }
    }

    let (file_path, project_dir) = find_session_file_with_dir(session_id, directory)
        .ok_or_else(|| Error::SessionNotFound(format!("Session {session_id} not found")))?;

    let content = std::fs::read(&file_path).map_err(Error::Io)?;
    if content.is_empty() {
        return Err(Error::Invalid(format!(
            "Session {session_id} has no messages to fork"
        )));
    }

    let (transcript, content_replacements) = parse_fork_transcript(&content, session_id);

    let (forked_session_id, lines) = build_fork_lines(
        transcript,
        content_replacements,
        session_id,
        up_to_message_id,
        title,
        || derive_title_from_content(&content),
    )?;

    let fork_path = project_dir.join(format!("{forked_session_id}.jsonl"));
    write_new_file(&fork_path, &(lines.join("\n") + "\n"))?;

    Ok(ForkSessionResult {
        session_id: forked_session_id,
    })
}

// ---------------------------------------------------------------------------
// SessionStore-backed mutations (*_via_store)
// ---------------------------------------------------------------------------

fn store_key(project_key: String, session_id: &str) -> SessionKey {
    SessionKey {
        project_key,
        session_id: session_id.to_string(),
        subpath: None,
    }
}

/// Renames a session by appending a `custom-title` entry to a store. Async,
/// store-backed counterpart to [`rename_session`]. Mirrors
/// `rename_session_via_store`.
pub async fn rename_session_via_store(
    store: &dyn SessionStore,
    session_id: &str,
    title: &str,
    directory: Option<&std::path::Path>,
) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let stripped = title.trim();
    if stripped.is_empty() {
        return Err(Error::Invalid("title must be non-empty".into()));
    }
    let project_key = project_key_for_directory(directory);
    let entry = json!({
        "type": "custom-title", "customTitle": stripped, "sessionId": session_id,
        "uuid": uuid::Uuid::new_v4().to_string(), "timestamp": iso_now(),
    })
    .as_object()
    .unwrap()
    .clone();
    store
        .append(&store_key(project_key, session_id), &[entry])
        .await
}

/// Tags a session (or clears with `None`) via a store. Async, store-backed
/// counterpart to [`tag_session`]. Mirrors `tag_session_via_store`.
pub async fn tag_session_via_store(
    store: &dyn SessionStore,
    session_id: &str,
    tag: Option<&str>,
    directory: Option<&std::path::Path>,
) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let tag_value: String = match tag {
        None => String::new(),
        Some(t) => {
            let s = sanitize_unicode(t);
            let s = s.trim();
            if s.is_empty() {
                return Err(Error::Invalid(
                    "tag must be non-empty (use None to clear)".into(),
                ));
            }
            s.to_string()
        }
    };
    let project_key = project_key_for_directory(directory);
    let entry = json!({
        "type": "tag", "tag": tag_value, "sessionId": session_id,
        "uuid": uuid::Uuid::new_v4().to_string(), "timestamp": iso_now(),
    })
    .as_object()
    .unwrap()
    .clone();
    store
        .append(&store_key(project_key, session_id), &[entry])
        .await
}

/// Deletes a session from a store. Async, store-backed counterpart to
/// [`delete_session`]. A no-op if the store does not implement `delete`
/// (WORM/append-only backends). Mirrors `delete_session_via_store`.
pub async fn delete_session_via_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&std::path::Path>,
) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let project_key = project_key_for_directory(directory);
    match store.delete(&store_key(project_key, session_id)).await {
        Ok(()) => Ok(()),
        Err(Error::Unsupported(_)) => Ok(()), // no-op for append-only stores
        Err(e) => Err(e),
    }
}

/// Forks a session with fresh UUIDs via a store, running the transform directly
/// over the loaded entries. Async, store-backed counterpart to [`fork_session`].
/// Mirrors `fork_session_via_store`.
pub async fn fork_session_via_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&std::path::Path>,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
) -> Result<ForkSessionResult> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    if let Some(up) = up_to_message_id {
        if !validate_uuid(up) {
            return Err(Error::Invalid(format!("Invalid up_to_message_id: {up}")));
        }
    }
    let project_key = project_key_for_directory(directory);
    let loaded = store
        .load(&store_key(project_key.clone(), session_id))
        .await?;
    let raw = match loaded {
        Some(r) if !r.is_empty() => r,
        _ => {
            return Err(Error::SessionNotFound(format!(
                "Session {session_id} not found"
            )))
        }
    };

    let mut transcript: Vec<Value> = Vec::new();
    let mut content_replacements: Vec<Value> = Vec::new();
    for e in &raw {
        let etype = e.get("type").and_then(Value::as_str);
        let has_uuid = e.get("uuid").and_then(Value::as_str).is_some();
        if etype.is_some_and(|t| TRANSCRIPT_TYPES.contains(&t)) && has_uuid {
            transcript.push(Value::Object(e.clone()));
        } else if etype == Some("content-replacement")
            && e.get("sessionId").and_then(Value::as_str) == Some(session_id)
        {
            if let Some(reps) = e.get("replacements").and_then(Value::as_array) {
                content_replacements.extend(reps.iter().cloned());
            }
        }
    }

    let (forked_session_id, lines) = build_fork_lines(
        transcript,
        content_replacements,
        session_id,
        up_to_message_id,
        title,
        || derive_title_from_entries(&raw),
    )?;

    let entries: Vec<SessionStoreEntry> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Map<String, Value>>(line).ok())
        .collect();
    store
        .append(&store_key(project_key, &forked_session_id), &entries)
        .await?;
    Ok(ForkSessionResult {
        session_id: forked_session_id,
    })
}

// ---------------------------------------------------------------------------
// Fork transform
// ---------------------------------------------------------------------------

fn parse_fork_transcript(content: &[u8], session_id: &str) -> (Vec<Value>, Vec<Value>) {
    let text = String::from_utf8_lossy(content);
    let mut transcript = Vec::new();
    let mut content_replacements = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !entry.is_object() {
            continue;
        }
        let etype = entry.get("type").and_then(Value::as_str);
        let has_uuid = entry.get("uuid").and_then(Value::as_str).is_some();
        if etype.is_some_and(|t| TRANSCRIPT_TYPES.contains(&t)) && has_uuid {
            transcript.push(entry);
        } else if etype == Some("content-replacement")
            && entry.get("sessionId").and_then(Value::as_str) == Some(session_id)
        {
            if let Some(reps) = entry.get("replacements").and_then(Value::as_array) {
                content_replacements.extend(reps.iter().cloned());
            }
        }
    }
    (transcript, content_replacements)
}

/// Title derivation for the disk path: head/tail byte scan. Mirrors the disk
/// `_derive_title` closure in `fork_session`.
fn derive_title_from_content(content: &[u8]) -> Option<String> {
    let len = content.len();
    let head = String::from_utf8_lossy(&content[..len.min(LITE_READ_BUF_SIZE)]).into_owned();
    let tail =
        String::from_utf8_lossy(&content[len.saturating_sub(LITE_READ_BUF_SIZE)..]).into_owned();
    extract_last_json_string_field(&tail, "customTitle")
        .or_else(|| extract_last_json_string_field(&head, "customTitle"))
        .or_else(|| extract_last_json_string_field(&tail, "aiTitle"))
        .or_else(|| extract_last_json_string_field(&head, "aiTitle"))
        .or_else(|| {
            let p = extract_first_prompt_from_head(&head);
            if p.is_empty() {
                None
            } else {
                Some(p)
            }
        })
}

/// Title derivation for the store path: scan already-parsed entries (last-wins
/// customTitle/aiTitle, else first-prompt over re-serialized JSONL). Mirrors
/// `_derive_title_from_entries`.
fn derive_title_from_entries(raw: &[SessionStoreEntry]) -> Option<String> {
    let mut custom: Option<String> = None;
    let mut ai: Option<String> = None;
    for e in raw {
        if let Some(ct) = e
            .get("customTitle")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            custom = Some(ct.to_string());
        }
        if let Some(at) = e
            .get("aiTitle")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            ai = Some(at.to_string());
        }
    }
    if custom.is_some() {
        return custom;
    }
    if ai.is_some() {
        return ai;
    }
    let jsonl: String = raw
        .iter()
        .map(|e| Value::Object(e.clone()).to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let p = extract_first_prompt_from_head(&jsonl);
    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

fn build_fork_lines<F>(
    transcript: Vec<Value>,
    content_replacements: Vec<Value>,
    session_id: &str,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
    derive_title: F,
) -> Result<(String, Vec<String>)>
where
    F: FnOnce() -> Option<String>,
{
    // Filter out sidechains; keep isMeta entries.
    let mut transcript: Vec<Value> = transcript
        .into_iter()
        .filter(|e| !truthy(e, "isSidechain"))
        .collect();
    if transcript.is_empty() {
        return Err(Error::Invalid(format!(
            "Session {session_id} has no messages to fork"
        )));
    }

    if let Some(up) = up_to_message_id {
        let cutoff = transcript
            .iter()
            .position(|e| e.get("uuid").and_then(Value::as_str) == Some(up));
        match cutoff {
            Some(i) => transcript.truncate(i + 1),
            None => {
                return Err(Error::Invalid(format!(
                    "Message {up} not found in session {session_id}"
                )))
            }
        }
    }

    // Map every uuid (including progress) so parent chains resolve.
    let mut uuid_mapping = std::collections::HashMap::new();
    for entry in &transcript {
        if let Some(u) = entry.get("uuid").and_then(Value::as_str) {
            uuid_mapping.insert(u.to_string(), uuid::Uuid::new_v4().to_string());
        }
    }
    let by_uuid: std::collections::HashMap<&str, &Value> = transcript
        .iter()
        .filter_map(|e| e.get("uuid").and_then(Value::as_str).map(|u| (u, e)))
        .collect();

    let writable: Vec<&Value> = transcript
        .iter()
        .filter(|e| e.get("type").and_then(Value::as_str) != Some("progress"))
        .collect();
    if writable.is_empty() {
        return Err(Error::Invalid(format!(
            "Session {session_id} has no messages to fork"
        )));
    }

    let forked_session_id = uuid::Uuid::new_v4().to_string();
    let now = iso_now();
    let mut lines = Vec::new();

    for (i, original) in writable.iter().enumerate() {
        let orig_uuid = original.get("uuid").and_then(Value::as_str).unwrap_or("");
        let new_uuid = uuid_mapping.get(orig_uuid).cloned().unwrap_or_default();

        // Resolve parentUuid, skipping progress ancestors.
        let mut new_parent_uuid = Value::Null;
        let mut parent_id = original
            .get("parentUuid")
            .and_then(Value::as_str)
            .map(str::to_string);
        while let Some(pid) = parent_id {
            match by_uuid.get(pid.as_str()) {
                None => break,
                Some(parent) => {
                    if parent.get("type").and_then(Value::as_str) != Some("progress") {
                        new_parent_uuid = uuid_mapping
                            .get(&pid)
                            .cloned()
                            .map(Value::String)
                            .unwrap_or(Value::Null);
                        break;
                    }
                    parent_id = parent
                        .get("parentUuid")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
        }

        let timestamp = if i == writable.len() - 1 {
            Value::String(now.clone())
        } else {
            original
                .get("timestamp")
                .cloned()
                .unwrap_or_else(|| Value::String(now.clone()))
        };

        let new_logical_parent = match original.get("logicalParentUuid").and_then(Value::as_str) {
            Some(lp) => uuid_mapping
                .get(lp)
                .cloned()
                .map(Value::String)
                .unwrap_or(Value::String(lp.to_string())),
            None => Value::Null,
        };

        let mut forked = original.as_object().cloned().unwrap_or_default();
        forked.insert("uuid".into(), Value::String(new_uuid));
        forked.insert("parentUuid".into(), new_parent_uuid);
        forked.insert("logicalParentUuid".into(), new_logical_parent);
        forked.insert("sessionId".into(), Value::String(forked_session_id.clone()));
        forked.insert("timestamp".into(), timestamp);
        forked.insert("isSidechain".into(), Value::Bool(false));
        forked.insert(
            "forkedFrom".into(),
            json!({"sessionId": session_id, "messageUuid": orig_uuid}),
        );
        for key in ["teamName", "agentName", "slug", "sourceToolAssistantUUID"] {
            forked.remove(key);
        }
        lines.push(Value::Object(forked).to_string());
    }

    if !content_replacements.is_empty() {
        lines.push(
            json!({
                "type": "content-replacement",
                "sessionId": forked_session_id,
                "replacements": content_replacements,
                "uuid": uuid::Uuid::new_v4().to_string(),
                "timestamp": now,
            })
            .to_string(),
        );
    }

    let fork_title = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => format!(
            "{} (fork)",
            derive_title().unwrap_or_else(|| "Forked session".to_string())
        ),
    };
    lines.push(
        json!({
            "type": "custom-title",
            "sessionId": forked_session_id,
            "customTitle": fork_title,
            "uuid": uuid::Uuid::new_v4().to_string(),
            "timestamp": now,
        })
        .to_string(),
    );

    Ok((forked_session_id, lines))
}

fn truthy(e: &Value, key: &str) -> bool {
    matches!(e.get(key), Some(Value::Bool(true)))
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn find_session_file_with_dir(
    session_id: &str,
    directory: Option<&Path>,
) -> Option<(PathBuf, PathBuf)> {
    let file_name = format!("{session_id}.jsonl");
    let try_dir = |project_dir: &Path| -> Option<(PathBuf, PathBuf)> {
        let path = project_dir.join(&file_name);
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() && m.len() > 0 => Some((path, project_dir.to_path_buf())),
            _ => None,
        }
    };

    if let Some(dir) = directory {
        let canonical = canonicalize_path(dir);
        if let Some(project_dir) = find_project_dir(&canonical) {
            if let Some(r) = try_dir(&project_dir) {
                return Some(r);
            }
        }
        for wt in worktree_paths(&canonical) {
            if wt == canonical {
                continue;
            }
            if let Some(wt_project_dir) = find_project_dir(&wt) {
                if let Some(r) = try_dir(&wt_project_dir) {
                    return Some(r);
                }
            }
        }
        return None;
    }

    let entries = std::fs::read_dir(projects_dir()).ok()?;
    for entry in entries.flatten() {
        if let Some(r) = try_dir(&entry.path()) {
            return Some(r);
        }
    }
    None
}

/// Appends `data` to an existing session file, searching candidate dirs.
/// Mirrors `_append_to_session`.
fn append_to_session(session_id: &str, data: &str, directory: Option<&Path>) -> Result<()> {
    let file_name = format!("{session_id}.jsonl");

    if let Some(dir) = directory {
        let canonical = canonicalize_path(dir);
        if let Some(project_dir) = find_project_dir(&canonical) {
            if try_append(&project_dir.join(&file_name), data)? {
                return Ok(());
            }
        }
        for wt in worktree_paths(&canonical) {
            if wt == canonical {
                continue;
            }
            if let Some(wt_project_dir) = find_project_dir(&wt) {
                if try_append(&wt_project_dir.join(&file_name), data)? {
                    return Ok(());
                }
            }
        }
        return Err(Error::SessionNotFound(format!(
            "Session {session_id} not found in project directory for {}",
            dir.display()
        )));
    }

    let entries = std::fs::read_dir(projects_dir()).map_err(|_| {
        Error::SessionNotFound(format!(
            "Session {session_id} not found (no projects directory)"
        ))
    })?;
    for entry in entries.flatten() {
        if try_append(&entry.path().join(&file_name), data)? {
            return Ok(());
        }
    }
    Err(Error::SessionNotFound(format!(
        "Session {session_id} not found in any project directory"
    )))
}

/// Tries to append to a path with `O_APPEND` (no create). Returns `Ok(false)`
/// if the file is missing or 0-byte, `Ok(true)` on success. Mirrors
/// `_try_append`.
fn try_append(path: &Path, data: &str) -> Result<bool> {
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new().append(true).open(path) {
        Ok(f) => f,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false)
        }
        Err(e) => return Err(Error::Io(e)),
    };
    let meta = file.metadata().map_err(Error::Io)?;
    if meta.len() == 0 {
        return Ok(false);
    }
    file.write_all(data.as_bytes()).map_err(Error::Io)?;
    Ok(true)
}

/// Creates a new file exclusively (`O_CREAT | O_EXCL`) and writes `data`.
fn write_new_file(path: &Path, data: &str) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(Error::Io)?;
    file.write_all(data.as_bytes()).map_err(Error::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unicode sanitization + time
// ---------------------------------------------------------------------------

/// Removes dangerous Unicode characters, iterating NFKC normalization with a
/// strip of the `Cf` (format), `Co` (private-use), and `Cn` (unassigned)
/// general categories plus the upstream explicit ranges, until stable (max 10
/// iterations). Faithful port of `_sanitize_unicode`.
fn sanitize_unicode(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut current = value.to_string();
    for _ in 0..10 {
        let previous = current.clone();
        let normalized: String = current.nfkc().collect();
        current = normalized.chars().filter(|c| !is_stripped(*c)).collect();
        if current == previous {
            break;
        }
    }
    current
}

fn is_stripped(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory};
    // Cf / Co / Cn general categories.
    if matches!(
        get_general_category(c),
        GeneralCategory::Format | GeneralCategory::PrivateUse | GeneralCategory::Unassigned
    ) {
        return true;
    }
    // Explicit ranges (redundant with the category check, matching upstream).
    matches!(c,
        '\u{200b}'..='\u{200f}'   // zero-width, LTR/RTL marks
        | '\u{202a}'..='\u{202e}' // directional formatting
        | '\u{2066}'..='\u{2069}' // directional isolates
        | '\u{feff}'              // BOM
        | '\u{e000}'..='\u{f8ff}' // BMP private use
    )
}

/// Current UTC time as an ISO-8601 string with a `Z` suffix (millisecond
/// precision). Mirrors `_iso_now`.
fn iso_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Inverse of `days_from_civil` (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_unicode_strips_zero_width() {
        assert_eq!(sanitize_unicode("a\u{200b}b\u{feff}c"), "abc");
        assert_eq!(sanitize_unicode("plain"), "plain");
    }

    #[test]
    fn sanitize_unicode_strips_format_category_outside_explicit_ranges() {
        // U+00AD SOFT HYPHEN is general category Cf but not in the explicit
        // ranges — the category strip must catch it.
        assert_eq!(sanitize_unicode("a\u{00ad}b"), "ab");
        // U+2028 LINE SEPARATOR (Zl) is not stripped — normal-ish text survives.
        assert_eq!(sanitize_unicode("hello world"), "hello world");
    }

    #[test]
    fn project_key_matches_sanitize() {
        // A path that does not exist canonicalizes to the NFC input, then sanitizes.
        let key = project_key_for_directory(Some(Path::new("/tmp/does-not-exist-xyz")));
        assert!(key.starts_with("-tmp-does-not-exist-xyz"));
    }

    #[test]
    fn iso_now_shape() {
        let s = iso_now();
        assert_eq!(s.len(), "2026-01-01T00:00:00.000Z".len());
        assert!(s.ends_with('Z'));
        assert_eq!(&s[4..5], "-");
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(31), (1970, 2, 1));
    }

    #[test]
    fn rename_rejects_invalid_uuid_and_empty_title() {
        assert!(matches!(
            rename_session("bad", "t", None),
            Err(Error::InvalidSessionId(_))
        ));
        assert!(matches!(
            rename_session("550e8400-e29b-41d4-a716-446655440000", "  ", None),
            Err(Error::Invalid(_))
        ));
    }
}
