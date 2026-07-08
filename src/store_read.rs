//! Store-backed async counterparts to the session reader.
//!
//! Faithful port of the `*_from_store` functions in `_internal/sessions.py`.
//! These read session history from a [`SessionStore`] instead of local disk,
//! reusing the same lite-parse / chain-building so a given transcript yields
//! identical results on both paths.
//!
//! Where Python probes `SessionStore` Protocol method overrides, the Rust trait
//! surfaces unimplemented optional methods as [`Error::Unsupported`]; this module
//! branches on that instead.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{stream, StreamExt};
use serde_json::{Map, Value};

use crate::chain::{entries_to_session_messages, entries_to_subagent_messages};
use crate::error::{Error, Result};
use crate::parse::{jsonl_to_lite, parse_iso_to_epoch_ms, parse_session_info_from_lite};
use crate::paths::{canonicalize_path, sanitize_path, validate_uuid};
use crate::sessions::apply_sort_limit_offset;
use crate::store::summary_entry_to_sdk_info;
use crate::types::{
    SessionInfo, SessionKey, SessionMessage, SessionStore, SessionStoreEntry,
    SessionStoreListEntry, SessionSummaryEntry, TranscriptEntry,
};

/// Transcript entry types kept when filtering store entries (mirrors the
/// reader's `TRANSCRIPT_ENTRY_TYPES`).
const TRANSCRIPT_ENTRY_TYPES: [&str; 5] = ["user", "assistant", "progress", "system", "attachment"];

/// Max concurrent per-session `load()`s during a store listing. Mirrors
/// `_STORE_LIST_LOAD_CONCURRENCY`.
const STORE_LIST_LOAD_CONCURRENCY: usize = 16;

fn project_path_for(directory: Option<&Path>) -> String {
    canonicalize_path(directory.unwrap_or_else(|| Path::new(".")))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn key(project_key: &str, session_id: &str, subpath: Option<String>) -> SessionKey {
    SessionKey {
        project_key: project_key.to_string(),
        session_id: session_id.to_string(),
        subpath,
    }
}

/// Serializes store entries to a JSONL string with `type` hoisted first (so the
/// tag-line prefix scan matches the disk byte shape). Mirrors `_entries_to_jsonl`.
fn entries_to_jsonl(entries: &[SessionStoreEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        let value = if e.contains_key("type") {
            // Rebuild with `type` first (serde_json preserve_order keeps order).
            let mut m = Map::new();
            if let Some(t) = e.get("type") {
                m.insert("type".into(), t.clone());
            }
            for (k, v) in e {
                if k != "type" {
                    m.insert(k.clone(), v.clone());
                }
            }
            Value::Object(m)
        } else {
            Value::Object(e.clone())
        };
        out.push_str(&value.to_string());
        out.push('\n');
    }
    out
}

/// Best-effort mtime from the last entry's `timestamp`, else now. Mirrors
/// `_mtime_from_jsonl_tail`.
fn mtime_from_jsonl_tail(jsonl: &str) -> i64 {
    let trimmed = jsonl.trim_end();
    let last_line = match trimmed.rfind('\n') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    };
    serde_json::from_str::<Value>(last_line)
        .ok()
        .and_then(|v| {
            v.get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .and_then(|ts| parse_iso_to_epoch_ms(&ts))
        .unwrap_or_else(now_ms)
}

/// Filters store entries to transcript message types with a string `uuid`.
/// Mirrors `_filter_transcript_entries`.
fn filter_transcript_entries(entries: &[SessionStoreEntry]) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| {
            e.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| TRANSCRIPT_ENTRY_TYPES.contains(&t))
                && e.get("uuid").and_then(Value::as_str).is_some()
        })
        .map(|e| Value::Object(e.clone()))
        .collect()
}

/// Loads a session's entries and serializes to JSONL, or `Ok(None)` if empty.
async fn load_store_entries_as_jsonl(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&Path>,
) -> Result<Option<String>> {
    let project_key = crate::project_key_for_directory(directory);
    let entries = store.load(&key(&project_key, session_id, None)).await?;
    Ok(match entries {
        Some(entries) if !entries.is_empty() => Some(entries_to_jsonl(&entries)),
        _ => None,
    })
}

/// Derives a [`SessionInfo`] per listing entry via bounded-concurrency
/// per-session `load()` + lite-parse. An adapter error degrades that row to an
/// empty summary; sidechain / no-summary sessions are dropped. Mirrors
/// `_derive_infos_via_load`.
async fn derive_infos_via_load(
    store: &dyn SessionStore,
    listing: &[SessionStoreListEntry],
    directory: Option<&Path>,
    project_path: &str,
) -> Vec<SessionInfo> {
    let loaded: Vec<(String, i64, Result<Option<String>>)> = stream::iter(listing.iter())
        .map(|entry| async move {
            let jsonl = load_store_entries_as_jsonl(store, &entry.session_id, directory).await;
            (entry.session_id.clone(), entry.mtime, jsonl)
        })
        .buffer_unordered(STORE_LIST_LOAD_CONCURRENCY)
        .collect()
        .await;

    let mut results = Vec::new();
    for (sid, mtime, outcome) in loaded {
        match outcome {
            Err(_) => results.push(SessionInfo {
                session_id: sid,
                summary: String::new(),
                last_modified: mtime,
                ..Default::default()
            }),
            Ok(None) => {}
            Ok(Some(jsonl)) => {
                let lite = jsonl_to_lite(&jsonl, mtime);
                if let Some(mut info) =
                    parse_session_info_from_lite(&sid, &lite, Some(project_path))
                {
                    info.last_modified = mtime;
                    results.push(info);
                }
            }
        }
    }
    results
}

/// Lists sessions from a [`SessionStore`]. Async, store-backed counterpart to
/// [`list_sessions`](crate::list_sessions). `include_worktrees` has no meaning
/// on the store path (a store operates on a single `project_key`).
///
/// Returns [`Error::Invalid`] if the store implements neither
/// `list_session_summaries` nor `list_sessions`.
pub async fn list_sessions_from_store(
    store: &dyn SessionStore,
    directory: Option<&Path>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionInfo>> {
    let project_path = project_path_for(directory);
    let project_key = sanitize_path(&project_path);

    // Fast path: incremental summaries.
    match store.list_session_summaries(&project_key).await {
        Ok(summaries) => {
            return list_from_summaries(
                store,
                &project_key,
                &project_path,
                directory,
                summaries,
                limit,
                offset,
            )
            .await;
        }
        Err(Error::Unsupported(_)) => {} // fall through
        Err(e) => return Err(e),
    }

    // Slow path: one load() per session.
    let listing = match store.list_sessions(&project_key).await {
        Ok(l) => l,
        Err(Error::Unsupported(_)) => {
            return Err(Error::Invalid(
                "session_store implements neither list_session_summaries() nor list_sessions() -- cannot list sessions. Provide a store with at least one of those methods.".into(),
            ))
        }
        Err(e) => return Err(e),
    };
    let results = derive_infos_via_load(store, &listing, directory, &project_path).await;
    Ok(apply_sort_limit_offset(results, limit, offset))
}

struct Slot {
    mtime: i64,
    session_id: Option<String>,
    info: Option<SessionInfo>,
}

#[allow(clippy::too_many_arguments)]
async fn list_from_summaries(
    store: &dyn SessionStore,
    project_key: &str,
    project_path: &str,
    directory: Option<&Path>,
    summaries: Vec<SessionSummaryEntry>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionInfo>> {
    let (listing, known_mtimes, has_list_sessions) = match store.list_sessions(project_key).await {
        Ok(l) => {
            let m: HashMap<String, i64> =
                l.iter().map(|e| (e.session_id.clone(), e.mtime)).collect();
            (l, m, true)
        }
        Err(Error::Unsupported(_)) => (Vec::new(), HashMap::new(), false),
        Err(e) => return Err(e),
    };

    let mut slots: Vec<Slot> = Vec::new();
    let mut fresh: HashSet<String> = HashSet::new();
    for s in &summaries {
        let sid = &s.session_id;
        if has_list_sessions {
            match known_mtimes.get(sid) {
                None => continue,                            // no longer listed — drop
                Some(&known) if s.mtime < known => continue, // stale — gap-fill
                _ => {}
            }
        }
        match summary_entry_to_sdk_info(s, Some(project_path)) {
            None => {
                fresh.insert(sid.clone());
            }
            Some(info) => {
                slots.push(Slot {
                    mtime: s.mtime,
                    session_id: None,
                    info: Some(info),
                });
                fresh.insert(sid.clone());
            }
        }
    }
    if has_list_sessions {
        for e in &listing {
            if !fresh.contains(&e.session_id) {
                slots.push(Slot {
                    mtime: e.mtime,
                    session_id: Some(e.session_id.clone()),
                    info: None,
                });
            }
        }
    }

    // Paginate BEFORE the gap-fill loads so their count is bounded by page size.
    slots.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    if offset > 0 {
        let start = offset.min(slots.len());
        slots.drain(..start);
    }
    if let Some(l) = limit {
        if l > 0 {
            slots.truncate(l);
        }
    }

    let to_fill: Vec<SessionStoreListEntry> = slots
        .iter()
        .filter(|s| s.info.is_none())
        .filter_map(|s| {
            s.session_id.clone().map(|sid| SessionStoreListEntry {
                session_id: sid,
                mtime: s.mtime,
            })
        })
        .collect();
    if !to_fill.is_empty() {
        let filled = derive_infos_via_load(store, &to_fill, directory, project_path).await;
        let by_sid: HashMap<String, SessionInfo> = filled
            .into_iter()
            .map(|f| (f.session_id.clone(), f))
            .collect();
        for slot in slots.iter_mut().filter(|s| s.info.is_none()) {
            if let Some(sid) = &slot.session_id {
                slot.info = by_sid.get(sid).cloned();
            }
        }
    }

    Ok(slots.into_iter().filter_map(|s| s.info).collect())
}

/// Reads metadata for one session from a [`SessionStore`]. Store-backed
/// counterpart to [`get_session_info`](crate::get_session_info).
pub async fn get_session_info_from_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&Path>,
) -> Result<Option<SessionInfo>> {
    if !validate_uuid(session_id) {
        return Ok(None);
    }
    let jsonl = match load_store_entries_as_jsonl(store, session_id, directory).await? {
        Some(j) => j,
        None => return Ok(None),
    };
    let lite = jsonl_to_lite(&jsonl, mtime_from_jsonl_tail(&jsonl));
    let project_path = project_path_for(directory);
    Ok(parse_session_info_from_lite(
        session_id,
        &lite,
        Some(&project_path),
    ))
}

/// Reads a session's conversation messages from a [`SessionStore`]. Store-backed
/// counterpart to [`get_session_messages`](crate::get_session_messages).
pub async fn get_session_messages_from_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&Path>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionMessage>> {
    if !validate_uuid(session_id) {
        return Ok(Vec::new());
    }
    let project_key = crate::project_key_for_directory(directory);
    let entries = store.load(&key(&project_key, session_id, None)).await?;
    let entries = match entries {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(Vec::new()),
    };
    let filtered = filter_transcript_entries(&entries);
    Ok(entries_to_session_messages(&filtered, limit, offset))
}

/// Reads a session's **full raw entries** from a [`SessionStore`] — the lossless
/// counterpart to [`get_session_messages_from_store`]. Returns every stored
/// entry verbatim (all fields, all branches, no chain selection or filtering),
/// exactly as the store holds it.
///
/// Note: a store keeps parsed entries ([`SessionStoreEntry`] = a JSON map), so
/// this preserves every *field* but is not byte-addressable the way the on-disk
/// [`get_session_entries`](crate::get_session_entries) is — a store never had
/// source bytes. Non-upstream extension.
///
/// Returns `Ok(vec![])` for an unknown session or a non-UUID `session_id`.
pub async fn get_session_entries_from_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&Path>,
) -> Result<Vec<SessionStoreEntry>> {
    if !validate_uuid(session_id) {
        return Ok(Vec::new());
    }
    let project_key = crate::project_key_for_directory(directory);
    let entries = store.load(&key(&project_key, session_id, None)).await?;
    Ok(entries.unwrap_or_default())
}

/// Reads a session's **typed, full-fidelity entries** from a [`SessionStore`] —
/// the typed counterpart to [`get_session_entries_from_store`] (and the
/// store-backed counterpart to [`get_session_entries_typed`]).
///
/// Each stored entry is parsed into a [`TranscriptEntry`] (envelope typed, all
/// other fields kept in `extra`). No chain selection or filtering. Non-upstream
/// extension. Returns `Ok(vec![])` for an unknown or non-UUID `session_id`.
pub async fn get_session_entries_typed_from_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&Path>,
) -> Result<Vec<TranscriptEntry>> {
    if !validate_uuid(session_id) {
        return Ok(Vec::new());
    }
    let project_key = crate::project_key_for_directory(directory);
    let entries = store.load(&key(&project_key, session_id, None)).await?;
    Ok(entries
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| serde_json::from_value::<TranscriptEntry>(Value::Object(m)).ok())
        .collect())
}

/// Lists subagent IDs for a session from a [`SessionStore`]. Store-backed
/// counterpart to [`list_subagents`](crate::list_subagents). Returns
/// [`Error::Invalid`] if the store does not implement `list_subkeys`.
pub async fn list_subagents_from_store(
    store: &dyn SessionStore,
    session_id: &str,
    directory: Option<&Path>,
) -> Result<Vec<String>> {
    if !validate_uuid(session_id) {
        return Ok(Vec::new());
    }
    let project_key = crate::project_key_for_directory(directory);
    let subkeys = match store
        .list_subkeys(&crate::types::SessionListSubkeysKey {
            project_key,
            session_id: session_id.to_string(),
        })
        .await
    {
        Ok(sk) => sk,
        Err(Error::Unsupported(_)) => {
            return Err(Error::Invalid(
                "session_store does not implement list_subkeys() -- cannot list subagents. Provide a store with a list_subkeys() method.".into(),
            ))
        }
        Err(e) => return Err(e),
    };

    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for subpath in subkeys {
        if !subpath.starts_with("subagents/") {
            continue;
        }
        let last = subpath.rsplit('/').next().unwrap_or("");
        if let Some(agent_id) = last.strip_prefix("agent-") {
            if seen.insert(agent_id.to_string()) {
                ids.push(agent_id.to_string());
            }
        }
    }
    Ok(ids)
}

/// Reads a subagent's messages from a [`SessionStore`]. Store-backed counterpart
/// to [`get_subagent_messages`](crate::get_subagent_messages).
pub async fn get_subagent_messages_from_store(
    store: &dyn SessionStore,
    session_id: &str,
    agent_id: &str,
    directory: Option<&Path>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionMessage>> {
    if !validate_uuid(session_id) || agent_id.is_empty() {
        return Ok(Vec::new());
    }
    let project_key = crate::project_key_for_directory(directory);

    let mut subpath = format!("subagents/agent-{agent_id}");
    match store
        .list_subkeys(&crate::types::SessionListSubkeysKey {
            project_key: project_key.clone(),
            session_id: session_id.to_string(),
        })
        .await
    {
        Ok(subkeys) => {
            let target = format!("agent-{agent_id}");
            match subkeys.into_iter().find(|sk| {
                sk.starts_with("subagents/") && sk.rsplit('/').next() == Some(target.as_str())
            }) {
                Some(m) => subpath = m,
                None => return Ok(Vec::new()),
            }
        }
        Err(Error::Unsupported(_)) => {} // fall back to the direct path
        Err(e) => return Err(e),
    }

    let entries = store
        .load(&key(&project_key, session_id, Some(subpath)))
        .await?;
    let entries = match entries {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(Vec::new()),
    };
    // Drop synthetic agent_metadata entries (sidecar descriptors, not transcript).
    let transcript: Vec<SessionStoreEntry> = entries
        .into_iter()
        .filter(|e| e.get("type").and_then(Value::as_str) != Some("agent_metadata"))
        .collect();
    if transcript.is_empty() {
        return Ok(Vec::new());
    }
    let filtered = filter_transcript_entries(&transcript);
    Ok(entries_to_subagent_messages(&filtered, limit, offset))
}
