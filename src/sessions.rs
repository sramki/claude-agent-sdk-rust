//! The public session-reading API and its directory-scanning internals.
//!
//! Ported from the filesystem functions in the Python
//! `_internal/sessions.py`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::chain::{
    entries_to_session_messages, entries_to_subagent_messages, parse_transcript_entries,
};
use crate::error::{Error, Result};
use crate::parse::{parse_session_info_from_lite, read_session_lite};
use crate::paths::{
    canonicalize_path, find_project_dir, projects_dir, sanitize_path, validate_uuid,
    worktree_paths, MAX_SANITIZED_LENGTH,
};
use crate::types::{SessionInfo, SessionMessage};

// ---------------------------------------------------------------------------
// Directory scanning
// ---------------------------------------------------------------------------

/// Reads session files from a single project directory. Mirrors
/// `_read_sessions_from_dir`.
fn read_sessions_from_dir(project_dir: &Path, project_path: Option<&str>) -> Vec<SessionInfo> {
    let entries = match std::fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = match name.strip_suffix(".jsonl") {
            Some(s) => s,
            None => continue,
        };
        if !validate_uuid(stem) {
            continue;
        }
        if let Some(lite) = read_session_lite(&entry.path()) {
            if let Some(info) = parse_session_info_from_lite(stem, &lite, project_path) {
                results.push(info);
            }
        }
    }
    results
}

/// Deduplicates by `session_id`, keeping the newest `last_modified`. Mirrors
/// `_deduplicate_by_session_id`.
fn deduplicate_by_session_id(sessions: Vec<SessionInfo>) -> Vec<SessionInfo> {
    let mut by_id: HashMap<String, SessionInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for s in sessions {
        match by_id.get(&s.session_id) {
            Some(existing) if s.last_modified <= existing.last_modified => {}
            _ => {
                if !by_id.contains_key(&s.session_id) {
                    order.push(s.session_id.clone());
                }
                by_id.insert(s.session_id.clone(), s);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// Sorts by `last_modified` descending (stable) and applies offset + limit.
/// Mirrors `_apply_sort_limit_offset`.
pub(crate) fn apply_sort_limit_offset(
    mut sessions: Vec<SessionInfo>,
    limit: Option<usize>,
    offset: usize,
) -> Vec<SessionInfo> {
    sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    if offset > 0 {
        let start = offset.min(sessions.len());
        sessions = sessions.split_off(start);
    }
    if let Some(l) = limit {
        if l > 0 {
            sessions.truncate(l);
        }
    }
    sessions
}

/// Lists sessions for a specific project directory (and its worktrees). Mirrors
/// `_list_sessions_for_project`.
fn list_sessions_for_project(
    directory: &Path,
    limit: Option<usize>,
    offset: usize,
    include_worktrees: bool,
) -> Vec<SessionInfo> {
    let canonical = canonicalize_path(directory);

    let worktrees = if include_worktrees {
        worktree_paths(&canonical)
    } else {
        Vec::new()
    };

    // No worktrees (or scanning disabled) — scan the single project dir.
    if worktrees.len() <= 1 {
        return match find_project_dir(&canonical) {
            None => Vec::new(),
            Some(dir) => {
                let sessions = read_sessions_from_dir(&dir, Some(&canonical));
                apply_sort_limit_offset(sessions, limit, offset)
            }
        };
    }

    // Worktree-aware scanning: match all project dirs against any worktree.
    let projects = projects_dir();

    // Longest sanitized prefix first, so more specific matches win.
    let mut indexed: Vec<(String, String)> = worktrees
        .iter()
        .map(|wt| (wt.clone(), sanitize_path(wt)))
        .collect();
    indexed.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let all_dirents: Vec<PathBuf> = match std::fs::read_dir(&projects) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => {
            // Fall back to the single project dir.
            return match find_project_dir(&canonical) {
                None => apply_sort_limit_offset(Vec::new(), limit, offset),
                Some(dir) => {
                    let sessions = read_sessions_from_dir(&dir, Some(&canonical));
                    apply_sort_limit_offset(sessions, limit, offset)
                }
            };
        }
    };

    let mut all_sessions: Vec<SessionInfo> = Vec::new();
    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Always include the user's actual directory (handles subdirectories that
    // won't match a worktree-root prefix).
    if let Some(canonical_project_dir) = find_project_dir(&canonical) {
        let base = canonical_project_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        seen_dirs.insert(base);
        all_sessions.extend(read_sessions_from_dir(
            &canonical_project_dir,
            Some(&canonical),
        ));
    }

    for entry in all_dirents {
        let dir_name = entry
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if seen_dirs.contains(&dir_name) {
            continue;
        }
        for (wt_path, prefix) in &indexed {
            // Prefix-match only for truncated (>MAX) paths; exact otherwise.
            let is_match = &dir_name == prefix
                || (prefix.len() >= MAX_SANITIZED_LENGTH
                    && dir_name.starts_with(&format!("{prefix}-")));
            if is_match {
                seen_dirs.insert(dir_name.clone());
                all_sessions.extend(read_sessions_from_dir(&entry, Some(wt_path)));
                break;
            }
        }
    }

    let deduped = deduplicate_by_session_id(all_sessions);
    apply_sort_limit_offset(deduped, limit, offset)
}

/// Lists sessions across all project directories. Mirrors `_list_all_sessions`.
fn list_all_sessions(limit: Option<usize>, offset: usize) -> Vec<SessionInfo> {
    let projects = projects_dir();
    let project_dirs: Vec<PathBuf> = match std::fs::read_dir(&projects) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => return Vec::new(),
    };

    let mut all_sessions = Vec::new();
    for dir in project_dirs {
        all_sessions.extend(read_sessions_from_dir(&dir, None));
    }
    let deduped = deduplicate_by_session_id(all_sessions);
    apply_sort_limit_offset(deduped, limit, offset)
}

// ---------------------------------------------------------------------------
// Session file reading (full transcript)
// ---------------------------------------------------------------------------

/// Reads a session JSONL file's full content from a project directory (only if
/// non-empty). Mirrors `_try_read_session_file`.
fn try_read_session_file(project_dir: &Path, file_name: &str) -> Option<String> {
    let content = std::fs::read_to_string(project_dir.join(file_name)).ok()?;
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

/// Finds and reads the session JSONL file. Mirrors `_read_session_file`.
fn read_session_file(session_id: &str, directory: Option<&Path>) -> Option<String> {
    let file_name = format!("{session_id}.jsonl");

    if let Some(dir) = directory {
        let canonical = canonicalize_path(dir);

        if let Some(project_dir) = find_project_dir(&canonical) {
            if let Some(content) = try_read_session_file(&project_dir, &file_name) {
                return Some(content);
            }
        }

        for wt in worktree_paths(&canonical) {
            if wt == canonical {
                continue;
            }
            if let Some(wt_project_dir) = find_project_dir(&wt) {
                if let Some(content) = try_read_session_file(&wt_project_dir, &file_name) {
                    return Some(content);
                }
            }
        }
        return None;
    }

    // No directory — search all project directories.
    let entries = std::fs::read_dir(projects_dir()).ok()?;
    for entry in entries.flatten() {
        if let Some(content) = try_read_session_file(&entry.path(), &file_name) {
            return Some(content);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Subagent resolution
// ---------------------------------------------------------------------------

/// Resolves the on-disk path of a session JSONL file (first non-empty match).
/// Mirrors `_resolve_session_file_path`.
fn resolve_session_file_path(session_id: &str, directory: Option<&Path>) -> Option<PathBuf> {
    let file_name = format!("{session_id}.jsonl");

    let stat_candidate = |project_dir: &Path| -> Option<PathBuf> {
        let candidate = project_dir.join(&file_name);
        match std::fs::metadata(&candidate) {
            Ok(m) if m.is_file() && m.len() > 0 => Some(candidate),
            _ => None,
        }
    };

    if let Some(dir) = directory {
        let canonical = canonicalize_path(dir);

        if let Some(project_dir) = find_project_dir(&canonical) {
            if let Some(found) = stat_candidate(&project_dir) {
                return Some(found);
            }
        }
        for wt in worktree_paths(&canonical) {
            if wt == canonical {
                continue;
            }
            if let Some(wt_project_dir) = find_project_dir(&wt) {
                if let Some(found) = stat_candidate(&wt_project_dir) {
                    return Some(found);
                }
            }
        }
        return None;
    }

    let entries = std::fs::read_dir(projects_dir()).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = stat_candidate(&path) {
                return Some(found);
            }
        }
    }
    None
}

/// Resolves `<projectDir>/<sessionId>/subagents/`. Mirrors
/// `_resolve_subagents_dir`.
fn resolve_subagents_dir(session_id: &str, directory: Option<&Path>) -> Option<PathBuf> {
    let resolved = resolve_session_file_path(session_id, directory)?;
    // Strip the .jsonl suffix to derive the session directory.
    let session_dir = resolved.with_extension("");
    Some(session_dir.join("subagents"))
}

/// Recursively collects `agent-*.jsonl` files (sorted by name within each dir).
/// Mirrors `_collect_agent_files`.
fn collect_agent_files(base_dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
            Err(_) => return,
        };
        entries.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()).unwrap_or_default());

        for entry in entries {
            let name = entry
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if entry.is_file() && name.starts_with("agent-") && name.ends_with(".jsonl") {
                let agent_id = name["agent-".len()..name.len() - ".jsonl".len()].to_string();
                out.push((agent_id, entry));
            } else if entry.is_dir() {
                walk(&entry, out);
            }
        }
    }

    let mut out = Vec::new();
    walk(base_dir, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Lists sessions with metadata extracted from `stat` + head/tail reads.
///
/// When `directory` is `Some`, returns sessions for that project directory
/// (and, when `include_worktrees` is `true` and the directory is in a git repo,
/// its worktrees). When `None`, returns sessions across all projects.
///
/// Results are sorted by `last_modified` descending. Use `limit` / `offset`
/// for pagination (`limit = Some(0)` returns all, matching the upstream
/// `limit > 0` check).
///
/// Returns `Ok` with an empty vector when the config directory is missing or
/// no sessions match — a listing degrades gracefully rather than erroring on a
/// missing directory or an unreadable individual file.
///
/// # Example
/// ```no_run
/// use std::path::Path;
/// let sessions = claude_agent_sdk_rs::list_sessions(Some(Path::new("/path/to/project")), None, 0, true)?;
/// for s in sessions {
///     println!("{}: {}", s.session_id, s.summary);
/// }
/// # Ok::<(), claude_agent_sdk_rs::Error>(())
/// ```
pub fn list_sessions(
    directory: Option<&Path>,
    limit: Option<usize>,
    offset: usize,
    include_worktrees: bool,
) -> Result<Vec<SessionInfo>> {
    Ok(match directory {
        Some(dir) => list_sessions_for_project(dir, limit, offset, include_worktrees),
        None => list_all_sessions(limit, offset),
    })
}

/// Reads metadata for a single session by ID (no O(n) directory scan).
///
/// Returns `Ok(None)` if the file is not found, the session is a sidechain
/// session, or it has no extractable summary. Returns
/// [`Error::InvalidSessionId`] if `session_id` is not a valid UUID. When
/// `directory` is omitted, all project directories are searched.
pub fn get_session_info(session_id: &str, directory: Option<&Path>) -> Result<Option<SessionInfo>> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let file_name = format!("{session_id}.jsonl");

    if let Some(dir) = directory {
        let canonical = canonicalize_path(dir);

        if let Some(project_dir) = find_project_dir(&canonical) {
            if let Some(lite) = read_session_lite(&project_dir.join(&file_name)) {
                return Ok(parse_session_info_from_lite(
                    session_id,
                    &lite,
                    Some(&canonical),
                ));
            }
        }

        for wt in worktree_paths(&canonical) {
            if wt == canonical {
                continue;
            }
            if let Some(wt_project_dir) = find_project_dir(&wt) {
                if let Some(lite) = read_session_lite(&wt_project_dir.join(&file_name)) {
                    return Ok(parse_session_info_from_lite(session_id, &lite, Some(&wt)));
                }
            }
        }
        return Ok(None);
    }

    // No directory — search all project directories for the file.
    let entries = match std::fs::read_dir(projects_dir()) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    for entry in entries.flatten() {
        if let Some(lite) = read_session_lite(&entry.path().join(&file_name)) {
            return Ok(parse_session_info_from_lite(session_id, &lite, None));
        }
    }
    Ok(None)
}

/// Reads a session's conversation messages from its JSONL transcript.
///
/// Parses the full JSONL, builds the conversation chain via `parentUuid` links,
/// and returns user/assistant messages in chronological order. Returns
/// `Ok(vec![])` if the session is not found or there are no visible messages,
/// and [`Error::InvalidSessionId`] if `session_id` is invalid. When `directory`
/// is omitted, all project directories are searched.
pub fn get_session_messages(
    session_id: &str,
    directory: Option<&Path>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionMessage>> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let content = match read_session_file(session_id, directory) {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };
    let entries = parse_transcript_entries(&content);
    Ok(entries_to_session_messages(&entries, limit, offset))
}

/// Lists subagent IDs for a session by scanning its `subagents/` directory
/// (recursively, including nested `workflows/<runId>/`).
///
/// Returns `Ok(vec![])` if the session is not found or has no subagents, and
/// [`Error::InvalidSessionId`] if `session_id` is invalid.
pub fn list_subagents(session_id: &str, directory: Option<&Path>) -> Result<Vec<String>> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    let subagents_dir = match resolve_subagents_dir(session_id, directory) {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };
    Ok(collect_agent_files(&subagents_dir)
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// Reads a subagent's conversation messages from its JSONL transcript.
///
/// The agent file may live directly in `subagents/` or in a nested
/// subdirectory. Returns `Ok(vec![])` if the session/subagent is not found,
/// [`Error::InvalidSessionId`] if `session_id` is invalid, and
/// [`Error::InvalidAgentId`] if `agent_id` is empty.
pub fn get_subagent_messages(
    session_id: &str,
    agent_id: &str,
    directory: Option<&Path>,
    limit: Option<usize>,
    offset: usize,
) -> Result<Vec<SessionMessage>> {
    if !validate_uuid(session_id) {
        return Err(Error::InvalidSessionId(session_id.to_string()));
    }
    if agent_id.is_empty() {
        return Err(Error::InvalidAgentId);
    }
    let subagents_dir = match resolve_subagents_dir(session_id, directory) {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };

    let matched = collect_agent_files(&subagents_dir)
        .into_iter()
        .find(|(found_id, _)| found_id == agent_id)
        .map(|(_, path)| path);

    let path = match matched {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) if !c.is_empty() => c,
        _ => return Ok(Vec::new()),
    };
    let entries = parse_transcript_entries(&content);
    Ok(entries_to_subagent_messages(&entries, limit, offset))
}
