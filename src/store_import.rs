//! Replay a local on-disk session transcript into a [`SessionStore`].
//!
//! Faithful port of `_internal/session_import.py` — the inverse of session
//! resume. Reads the local `~/.claude/projects/<dir>/<sessionId>.jsonl` (plus
//! subagent transcripts and their `.meta.json` sidecars) and replays each line
//! into `store.append()`. The destination `project_key` is the on-disk project
//! directory name, so an imported session is indistinguishable from a
//! live-mirrored one and resumable from the original cwd.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::paths::validate_uuid;
use crate::sessions::resolve_session_file_path;
use crate::types::{SessionKey, SessionStore, SessionStoreEntry};

/// Max entries per `append()` batch. Mirrors `MAX_PENDING_ENTRIES`.
pub(crate) const MAX_PENDING_ENTRIES: usize = 500;
/// Max line bytes per `append()` batch. Mirrors `MAX_PENDING_BYTES` (1 MiB).
pub(crate) const MAX_PENDING_BYTES: usize = 1024 * 1024;

/// Replays a local session transcript into a [`SessionStore`], flushing to
/// `store.append()` every `batch_size` entries (or 1 MiB, whichever comes
/// first). Mirrors `import_session_to_store`. `batch_size <= 0` uses the default.
///
/// Returns [`Error::InvalidSessionId`] for a bad UUID and [`Error::SessionNotFound`]
/// if the session JSONL is not on disk.
pub async fn import_session_to_store(
    session_id: &str,
    store: &dyn SessionStore,
    directory: Option<&Path>,
    include_subagents: bool,
    batch_size: usize,
) -> Result<()> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let resolved = resolve_session_file_path(session_id, directory)
        .ok_or_else(|| Error::SessionNotFound(format!("Session {session_id} not found")))?;

    // Key under the on-disk project directory name (matches the mirror path).
    let project_key = resolved
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let batch_size = if batch_size == 0 { MAX_PENDING_ENTRIES } else { batch_size };

    let main_key = SessionKey {
        project_key: project_key.clone(),
        session_id: session_id.to_string(),
        subpath: None,
    };
    append_jsonl_file_in_batches(&resolved, &main_key, store, batch_size).await?;

    if !include_subagents {
        return Ok(());
    }

    // Subagent transcripts live at <projectDir>/<sessionId>/subagents/**.
    let session_dir = resolved.with_extension("");
    let subagents_dir = session_dir.join("subagents");
    for file_path in collect_jsonl_files(&subagents_dir) {
        let rel = match file_path.strip_prefix(&session_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if let Some(last) = parts.last_mut() {
            if let Some(stripped) = last.strip_suffix(".jsonl") {
                *last = stripped.to_string();
            }
        }
        let sub_key = SessionKey {
            project_key: project_key.clone(),
            session_id: session_id.to_string(),
            subpath: Some(parts.join("/")),
        };
        append_jsonl_file_in_batches(&file_path, &sub_key, store, batch_size).await?;

        // Import the .meta.json sidecar as a synthetic agent_metadata entry so
        // resume can recreate it (the on-disk .jsonl never carries it).
        let meta_path = file_path.with_extension("meta.json");
        match std::fs::read_to_string(&meta_path) {
            Ok(text) => {
                if let Ok(Value::Object(meta)) = serde_json::from_str::<Value>(&text) {
                    let mut entry: SessionStoreEntry = Map::new();
                    entry.insert("type".into(), Value::String("agent_metadata".into()));
                    for (k, v) in meta {
                        entry.insert(k, v);
                    }
                    store.append(&sub_key, &[entry]).await?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(())
}

/// Stream-reads a JSONL file and flushes to `store.append()` in batches bounded
/// by entry count or byte size. Mirrors `_append_jsonl_file_in_batches`.
async fn append_jsonl_file_in_batches(
    file_path: &Path,
    key: &SessionKey,
    store: &dyn SessionStore,
    batch_size: usize,
) -> Result<()> {
    let content = std::fs::read_to_string(file_path).map_err(Error::Io)?;
    let mut batch: Vec<SessionStoreEntry> = Vec::new();
    let mut nbytes = 0usize;
    for line in content.split('\n') {
        if line.is_empty() {
            continue;
        }
        let entry: Map<String, Value> =
            serde_json::from_str(line).map_err(|e| Error::json_decode(line, e))?;
        nbytes += line.len();
        batch.push(entry);
        if batch.len() >= batch_size || nbytes >= MAX_PENDING_BYTES {
            store.append(key, &batch).await?;
            batch.clear();
            nbytes = 0;
        }
    }
    if !batch.is_empty() {
        store.append(key, &batch).await?;
    }
    Ok(())
}

/// Recursively collects `*.jsonl` files under `base`, sorted per directory for
/// deterministic import order. Mirrors `_collect_jsonl_files`.
fn collect_jsonl_files(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(base) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => return out,
    };
    entries.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()).unwrap_or_default());
    for entry in entries {
        if entry.is_dir() {
            out.extend(collect_jsonl_files(&entry));
        } else if entry.is_file()
            && entry.extension().is_some_and(|x| x == "jsonl")
        {
            out.push(entry);
        }
    }
    out
}
