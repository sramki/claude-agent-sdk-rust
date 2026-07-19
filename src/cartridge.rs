//! The **Claude cartridge** — a non-upstream extension surface: paths + pure
//! per-entry functions + a blob resolver, for plugging Claude session data into
//! an external streaming/merge engine. See `docs/cartridge-spec.md`.
//!
//! The SDK contributes **data and pure functions, nothing stateful** — it does
//! not read, stream, merge, cursor, filter, or watch. Interpret functions are
//! **total** (return `None`/empty on a malformed-but-complete line, never panic).

use std::path::PathBuf;

use serde_json::Value;

use crate::error::Result;
use crate::types::TranscriptEntry;

pub use crate::project_key_for_directory;
pub use crate::runtime::content_blocks;

/// The Claude Code schema version the interpret functions assume. Re-synced on
/// upstream release (the faithful-port mission carried into the adapter role).
pub const UPSTREAM_VERSION: &str = "0.2.110";

/// Claude projects root, honoring `CLAUDE_CONFIG_DIR` (else `~/.claude`).
pub fn projects_dir() -> PathBuf {
    crate::paths::projects_dir()
}

/// The `.jsonl` transcript path for a `(project, session_key)` — the inverse of the
/// [`TranscriptFile`] `project` + `session_key` a consumer already holds. A subagent
/// session key is composite (`<parent>/subagents/agent-<id>`), so the path nests
/// accordingly: `<projects>/<project>/<session_key>.jsonl`. Lets a consumer read a
/// specific transcript's sibling `.meta.json` (via [`read_agent_meta`]) ON DEMAND —
/// e.g. a subagent that appears after the initial discovery snapshot — WITHOUT a full
/// re-`discover_transcripts` walk. Path-only; does not touch the filesystem.
pub fn transcript_path(project: &str, session_key: &str) -> PathBuf {
    projects_dir().join(project).join(format!("{session_key}.jsonl"))
}

// ---------------------------------------------------------------------------
// 1. Locate
// ---------------------------------------------------------------------------

/// A project folder under the projects root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    /// The sanitized project directory **name** (e.g. `-Users-me-git-project`)
    /// — the stable folder key (correct even for the long-path hash form).
    pub name: String,
    /// Absolute path of the project directory (what a watcher watches).
    pub path: PathBuf,
    /// Count of top-level `*.jsonl` session files in the folder.
    pub session_count: usize,
}

/// A discovered transcript file (a locator — no content is read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    /// Absolute path of the `.jsonl` file.
    pub path: PathBuf,
    /// The project directory name it lives under.
    pub project: String,
    /// The owning session id (the top-level session uuid; for a subagent file,
    /// the parent session's id — the `<uuid>/` directory it sits beneath).
    pub session_id: String,
    /// For a subagent/workflow transcript, the path from the session directory
    /// to the file with the `.jsonl` stripped (e.g. `subagents/agent-abc`);
    /// `None` for a top-level session file.
    pub subpath: Option<String>,
    /// Whether this is a nested subagent/workflow transcript.
    pub is_subagent: bool,
}

/// Reads the ENTIRE sibling `<name>.meta.json` blob of a subagent transcript
/// (`agent-abc.jsonl` → `agent-abc.meta.json`) as a JSON value — every field
/// verbatim (`agentType`, `description`, `spawnDepth`, `toolUseId`, `worktreePath`,
/// `name`, … forward-compatible). `None` when the sidecar is absent or unparseable.
///
/// **Deliberately SEPARATE from [`discover_transcripts`]** (which stays a cheap,
/// stateless, path-only snapshot the follow loop can re-run freely). The sidecar is
/// immutable — written once when the agent spawns — so a consumer reads it EXACTLY
/// once per file, on demand, at the point of need (never per discovery pass, never
/// per streamed line).
pub fn read_agent_meta(jsonl_path: &std::path::Path) -> Option<serde_json::Value> {
    let meta_path = jsonl_path.with_extension("meta.json");
    let bytes = std::fs::read(&meta_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Enumerates the project folders under the projects root (one `read_dir`).
pub fn list_projects() -> Result<Vec<ProjectInfo>> {
    let root = projects_dir();
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Ok(out), // missing config dir → empty, not an error
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let session_count = std::fs::read_dir(&path)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.path().is_file()
                            && e.path().extension().and_then(|x| x.to_str()) == Some("jsonl")
                    })
                    .count()
            })
            .unwrap_or(0);
        out.push(ProjectInfo { name, path, session_count });
    }
    Ok(out)
}

/// Discovers transcript files under the projects root. `recursive=false` yields
/// only top-level session files; `recursive=true` also descends into nested
/// `subagents/`/`workflows/` transcripts. A stateless snapshot — no content
/// read, dedup, sort, or filtering; re-callable (the engine owns any watching).
pub fn discover_transcripts(recursive: bool) -> Result<Vec<TranscriptFile>> {
    let root = projects_dir();
    let mut out = Vec::new();
    let projects = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    for project in projects.flatten() {
        let proj_path = project.path();
        if !proj_path.is_dir() {
            continue;
        }
        let project_name = project.file_name().to_string_lossy().into_owned();
        collect_jsonl(&proj_path, &proj_path, &project_name, recursive, &mut out);
    }
    Ok(out)
}

fn collect_jsonl(
    dir: &std::path::Path,
    project_root: &std::path::Path,
    project_name: &str,
    recursive: bool,
    out: &mut Vec<TranscriptFile>,
) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_jsonl(&path, project_root, project_name, recursive, out);
            }
            continue;
        }
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        // Relative path from the project dir decides identity.
        let rel = match path.strip_prefix(project_root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let comps: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if comps.len() == 1 {
            // <session>.jsonl — a top-level session file.
            let session_id = comps[0].trim_end_matches(".jsonl").to_string();
            out.push(TranscriptFile {
                path,
                project: project_name.to_string(),
                session_id,
                subpath: None,
                is_subagent: false,
            });
        } else {
            // <session>/rest.../file.jsonl — a nested subagent/workflow transcript.
            let session_id = comps[0].clone();
            let mut sub = comps[1..].join("/");
            if let Some(stripped) = sub.strip_suffix(".jsonl") {
                sub = stripped.to_string();
            }
            out.push(TranscriptFile {
                path,
                project: project_name.to_string(),
                session_id,
                subpath: Some(sub),
                is_subagent: true,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Interpret — hot path (byte-scan)
// ---------------------------------------------------------------------------

/// Byte-scans a raw line for the native top-level `uuid`. Total — returns `None`
/// on a malformed line or an absent field; never allocates a DOM.
pub fn entry_id(line: &[u8]) -> Option<String> {
    scan_top_level_str(line, "uuid").map(str::to_string)
}

/// Byte-scans a raw line for the top-level `type`. Returns a borrow into `line`.
/// Total — `None` on malformed/absent.
pub fn entry_kind(line: &[u8]) -> Option<&str> {
    scan_top_level_str(line, "type")
}

/// Returns the string value of a **top-level** (brace-depth-1) string key.
///
/// Tracks object/array nesting and string state in a single byte-scan, so a
/// nested key of the same name inside content (e.g. `"uuid"` in a tool result or
/// pasted JSON) cannot spoof the match — the real top-level `uuid` sits at the
/// end of a Claude line, after `message`. No unescaping (the value is returned
/// verbatim as a borrow — `uuid`/`type` never contain escapes).
fn scan_top_level_str<'a>(line: &'a [u8], key: &str) -> Option<&'a str> {
    let n = line.len();
    let mut i = 0usize;
    let mut depth: i32 = 0;
    while i < n {
        match line[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                let (tok, after) = read_json_string(line, i)?;
                let mut j = after;
                while j < n && line[j].is_ascii_whitespace() {
                    j += 1;
                }
                // A string immediately followed by `:` at depth 1 is a top-level key.
                if depth == 1 && j < n && line[j] == b':' {
                    if tok == key.as_bytes() {
                        let mut k = j + 1;
                        while k < n && line[k].is_ascii_whitespace() {
                            k += 1;
                        }
                        if k < n && line[k] == b'"' {
                            let (val, _) = read_json_string(line, k)?;
                            return std::str::from_utf8(val).ok();
                        }
                        return None; // value is not a string
                    }
                    i = j + 1; // not our key — its value is handled by the loop
                } else {
                    i = after; // a value string (or nested) — advance past it
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Reads a JSON string token starting at `start` (a `"`), returning the inner
/// bytes (still escaped) and the index just past the closing quote. Backslash
/// escapes are skipped so an escaped quote does not end the string. `None` on an
/// unterminated string.
fn read_json_string(line: &[u8], start: usize) -> Option<(&[u8], usize)> {
    let inner = start + 1;
    let mut i = inner;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 2,
            b'"' => return Some((&line[inner..i], i + 1)),
            _ => i += 1,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 2. Interpret — downstream (&Value)
// ---------------------------------------------------------------------------

/// Cheap lineage/metadata fields read from a parsed entry (no full typing).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Envelope {
    /// Entry `type`.
    pub entry_type: Option<String>,
    /// Entry `uuid`.
    pub uuid: Option<String>,
    /// `parentUuid`.
    pub parent_uuid: Option<String>,
    /// `logicalParentUuid` (set on forked/edited branches).
    pub logical_parent_uuid: Option<String>,
    /// `sessionId`.
    pub session_id: Option<String>,
    /// ISO-8601 `timestamp` (raw; parse if you need epoch).
    pub timestamp: Option<String>,
    /// `isSidechain`.
    pub is_sidechain: bool,
    /// `isMeta`.
    pub is_meta: bool,
    /// `isCompactSummary`.
    pub is_compact_summary: bool,
}

/// Reads the [`Envelope`] fields from a parsed entry value.
pub fn envelope(value: &Value) -> Envelope {
    let s = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_string);
    let b = |k: &str| value.get(k) == Some(&Value::Bool(true));
    Envelope {
        entry_type: s("type"),
        uuid: s("uuid"),
        parent_uuid: s("parentUuid"),
        logical_parent_uuid: s("logicalParentUuid"),
        session_id: s("sessionId"),
        timestamp: s("timestamp"),
        is_sidechain: b("isSidechain"),
        is_meta: b("isMeta"),
        is_compact_summary: b("isCompactSummary"),
    }
}

/// Typed lens over a parsed entry (typed envelope + flattened `extra` = lossless).
pub fn to_typed(value: &Value) -> Option<TranscriptEntry> {
    serde_json::from_value::<TranscriptEntry>(value.clone()).ok()
}

/// Extracts blob reference tokens the entry carries (`imagePasteId` /
/// `imagePasteIds`), for resolution via [`resolve_blob`]. Total — empty on none.
pub fn blob_refs(value: &Value) -> Vec<String> {
    let mut refs = Vec::new();
    collect_blob_refs(value, &mut refs);
    refs
}

fn collect_blob_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v) {
                    ("imagePasteId", _) => push_ref(v, out),
                    ("imagePasteIds", Value::Array(arr)) => {
                        for e in arr {
                            push_ref(e, out);
                        }
                    }
                    _ => collect_blob_refs(v, out),
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_blob_refs(v, out);
            }
        }
        _ => {}
    }
}

fn push_ref(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Number(n) => out.push(n.to_string()),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 3. Dereference — blobs
// ---------------------------------------------------------------------------

/// A resolved blob — a path to the on-disk file, or inline bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    /// The blob lives at this path.
    Path(PathBuf),
    /// The blob's bytes, inlined.
    Bytes(Vec<u8>),
}

/// Resolves a blob reference by its **native store key** to a path.
///
/// - a `paste-cache` hex id → `<config>/paste-cache/<key>.txt`
/// - a `file-history` key → `<config>/file-history/<key>`
///
/// Returns `Path` when the file/dir exists, else `None`. **On-demand only** —
/// never call this in the hot pipeline.
///
/// Known open item: sessions may reference pastes by an integer *ordinal*
/// (`imagePasteIds:[1]`) rather than the hex store key; that ordinal→key
/// indirection is not yet reverse-engineered and resolves to `None`.
pub fn resolve_blob(reference: &str) -> Option<Blob> {
    // Reference is used as a path component — reject anything that could escape.
    if reference.is_empty()
        || reference.contains('/')
        || reference.contains('\\')
        || reference.contains("..")
        || reference.contains('\0')
    {
        return None;
    }
    let config = crate::paths::claude_config_home_dir();
    let paste = config.join("paste-cache").join(format!("{reference}.txt"));
    if paste.is_file() {
        return Some(Blob::Path(paste));
    }
    let file_history = config.join("file-history").join(reference);
    if file_history.exists() {
        return Some(Blob::Path(file_history));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentBlock;
    use serde_json::json;

    #[test]
    fn entry_id_and_kind_byte_scan() {
        let line = br#"{"type":"assistant","uuid":"11111111-2222-4333-8444-555555555555","parentUuid":"aa","message":{"content":[{"type":"text"}]}}"#;
        assert_eq!(entry_kind(line), Some("assistant"));
        assert_eq!(entry_id(line).as_deref(), Some("11111111-2222-4333-8444-555555555555"));
        // parentUuid must not be mistaken for uuid.
        assert_ne!(entry_id(line).as_deref(), Some("aa"));
    }

    #[test]
    fn byte_scan_ignores_nested_keys_in_content() {
        // The real top-level uuid sits at the END, after `message`; content
        // contains a spoofing "uuid" and a nested "type". Neither may win.
        let line = br#"{"type":"assistant","message":{"content":[{"type":"tool_result","content":"{\"uuid\":\"SPOOF\",\"type\":\"evil\"}"}]},"uuid":"REAL-uuid-at-end"}"#;
        assert_eq!(entry_kind(line), Some("assistant")); // not "tool_result"/"evil"
        assert_eq!(entry_id(line).as_deref(), Some("REAL-uuid-at-end")); // not "SPOOF"
        // Escaped quotes inside the nested string don't derail the scan.
        assert_ne!(entry_id(line).as_deref(), Some("SPOOF"));
    }

    #[test]
    fn interpret_fns_are_total_on_garbage() {
        assert_eq!(entry_id(b"not json at all"), None);
        assert_eq!(entry_kind(b""), None);
        assert_eq!(entry_kind(br#"{"nope":1}"#), None);
        assert_eq!(envelope(&json!({})).uuid, None);
        assert!(blob_refs(&json!(42)).is_empty());
        assert!(to_typed(&json!("not-an-object")).is_none());
    }

    #[test]
    fn envelope_reads_fields() {
        let v = json!({
            "type":"user","uuid":"u1","parentUuid":"p1","sessionId":"s1",
            "timestamp":"2024-01-01T00:00:00.000Z","isSidechain":true,
            "logicalParentUuid":"lp1","isMeta":false
        });
        let e = envelope(&v);
        assert_eq!(e.entry_type.as_deref(), Some("user"));
        assert_eq!(e.parent_uuid.as_deref(), Some("p1"));
        assert_eq!(e.logical_parent_uuid.as_deref(), Some("lp1"));
        assert!(e.is_sidechain);
        assert!(!e.is_meta);
        assert_eq!(e.timestamp.as_deref(), Some("2024-01-01T00:00:00.000Z"));
    }

    #[test]
    fn blob_refs_extracts_paste_ids() {
        let v = json!({"message":{"imagePasteIds":[1,2],"content":"x"},"imagePasteId":"abc123"});
        let mut refs = blob_refs(&v);
        refs.sort();
        assert_eq!(refs, vec!["1".to_string(), "2".to_string(), "abc123".to_string()]);
    }

    // read_agent_meta lifts the WHOLE sibling `<name>.meta.json` blob
    // (agent-abc.jsonl → agent-abc.meta.json); absent/garbage sidecar → None,
    // never an error. Every field is preserved verbatim (lose-no-detail).
    #[test]
    fn read_agent_meta_from_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("agent-abc.meta-check.jsonl");
        std::fs::write(&jsonl, b"{}\n").unwrap();

        // No sidecar yet → None.
        assert_eq!(read_agent_meta(&jsonl), None);

        // Sibling meta.json → the whole blob, all fields intact.
        let meta = dir.path().join("agent-abc.meta-check.meta.json");
        std::fs::write(
            &meta,
            br#"{"agentType":"general-purpose","description":"Build ingest/placement backend","toolUseId":"toolu_01Y6wY2DB7KVYPVjHcg4b1M8","spawnDepth":1,"worktreePath":"/wt/agent-abc"}"#,
        )
        .unwrap();
        let v = read_agent_meta(&jsonl).expect("sidecar present");
        assert_eq!(v.get("toolUseId").and_then(|x| x.as_str()), Some("toolu_01Y6wY2DB7KVYPVjHcg4b1M8"));
        assert_eq!(v.get("description").and_then(|x| x.as_str()), Some("Build ingest/placement backend"));
        assert_eq!(v.get("agentType").and_then(|x| x.as_str()), Some("general-purpose"));
        assert_eq!(v.get("spawnDepth").and_then(|x| x.as_i64()), Some(1));
        // Rare/unknown fields survive verbatim — forward-compatible.
        assert_eq!(v.get("worktreePath").and_then(|x| x.as_str()), Some("/wt/agent-abc"));

        // Garbage sidecar → None, not a panic.
        std::fs::write(&meta, b"not json").unwrap();
        assert_eq!(read_agent_meta(&jsonl), None);
    }

    // transcript_path reconstructs `<projects>/<project>/<session_key>.jsonl` — a
    // composite subagent key nests, a top-level key is one component.
    #[test]
    fn transcript_path_reconstructs_composite_key() {
        let sub = transcript_path("proj", "parent-sid/subagents/agent-x");
        assert!(
            sub.ends_with("proj/parent-sid/subagents/agent-x.jsonl"),
            "{sub:?}"
        );
        let top = transcript_path("proj", "top-sid");
        assert!(top.ends_with("proj/top-sid.jsonl"), "{top:?}");
    }

    #[test]
    fn resolve_blob_rejects_traversal_and_missing() {
        assert!(resolve_blob("../etc/passwd").is_none());
        assert!(resolve_blob("a/b").is_none());
        assert!(resolve_blob("").is_none());
        // A well-formed key that doesn't exist → None (no panic).
        assert!(resolve_blob("deadbeefdeadbeef").is_none());
    }

    #[test]
    fn to_typed_round_trips_and_content_blocks() {
        let v = json!({"type":"assistant","uuid":"u1","gitBranch":"main","customField":"x","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}});
        let t = to_typed(&v).unwrap();
        assert_eq!(t.entry_type.as_deref(), Some("assistant"));
        // gitBranch is a typed field; an unknown field lands in `extra` (lossless).
        assert_eq!(t.git_branch.as_deref(), Some("main"));
        assert_eq!(t.extra.get("customField").and_then(Value::as_str), Some("x"));
        let blocks = content_blocks(t.message.as_ref().unwrap());
        assert!(matches!(blocks.first(), Some(ContentBlock::Text(_))));
    }
}
