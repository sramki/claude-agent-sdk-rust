//! Transcript-entry parsing and conversation-chain reconstruction.
//!
//! Ported from the transcript helpers in the Python
//! `_internal/sessions.py`: `_parse_transcript_entries`,
//! `_build_conversation_chain`, `_build_subagent_chain`, `_is_visible_message`,
//! and `_to_session_message`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::types::{MessageType, SessionMessage};

/// Transcript entry types that carry `uuid` + `parentUuid` chain links.
const TRANSCRIPT_ENTRY_TYPES: [&str; 5] = ["user", "assistant", "progress", "system", "attachment"];

// ---------------------------------------------------------------------------
// Value accessors
// ---------------------------------------------------------------------------

fn entry_type(e: &Value) -> Option<&str> {
    e.get("type").and_then(Value::as_str)
}

fn entry_uuid(e: &Value) -> &str {
    e.get("uuid").and_then(Value::as_str).unwrap_or("")
}

/// The `parentUuid`, treated as absent when null/empty (Python `if parent:`).
fn entry_parent(e: &Value) -> Option<&str> {
    e.get("parentUuid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Python truthiness for the boolean-ish flag fields (`isMeta`, `isSidechain`,
/// `teamName`): present + non-null + non-false + non-empty.
fn truthy(e: &Value, key: &str) -> bool {
    match e.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn is_user_or_assistant(e: &Value) -> bool {
    matches!(entry_type(e), Some("user") | Some("assistant"))
}

/// Picks the candidate with the highest file position (most recent). Mirrors
/// the inner `_pick_best` in `_build_conversation_chain`.
fn pick_best<'a>(candidates: &[&'a Value], entry_index: &HashMap<&str, usize>) -> &'a Value {
    let idx_of = |e: &Value| -> i64 { entry_index.get(entry_uuid(e)).map_or(-1, |&i| i as i64) };
    let mut best = candidates[0];
    let mut best_idx = idx_of(best);
    for &cur in &candidates[1..] {
        let cur_idx = idx_of(cur);
        if cur_idx > best_idx {
            best = cur;
            best_idx = cur_idx;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parses JSONL content into transcript entries. Keeps only entries with a
/// string `uuid` whose `type` is a transcript message type. Skips corrupt
/// lines. Mirrors `_parse_transcript_entries`.
pub(crate) fn parse_transcript_entries(content: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    for raw_line in content.split('\n') {
        let line = raw_line.trim();
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
        let type_ok = entry_type(&entry).is_some_and(|t| TRANSCRIPT_ENTRY_TYPES.contains(&t));
        let uuid_ok = entry.get("uuid").and_then(Value::as_str).is_some();
        if type_ok && uuid_ok {
            entries.push(entry);
        }
    }
    entries
}

// ---------------------------------------------------------------------------
// Chain building
// ---------------------------------------------------------------------------

/// Builds the conversation chain by finding the leaf and walking `parentUuid`
/// back to the root, in chronological order. Mirrors `_build_conversation_chain`.
///
/// `logicalParentUuid` is intentionally NOT followed.
pub(crate) fn build_conversation_chain(entries: &[Value]) -> Vec<Value> {
    if entries.is_empty() {
        return Vec::new();
    }

    // Index by uuid (last occurrence wins) and record file position.
    let mut by_uuid: HashMap<&str, &Value> = HashMap::new();
    let mut entry_index: HashMap<&str, usize> = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        let uid = entry_uuid(entry);
        by_uuid.insert(uid, entry);
        entry_index.insert(uid, i);
    }

    // Terminals: uuids never referenced as any entry's parentUuid.
    let mut parent_uuids: HashSet<&str> = HashSet::new();
    for entry in entries {
        if let Some(parent) = entry_parent(entry) {
            parent_uuids.insert(parent);
        }
    }

    // From each terminal, walk back to the nearest user/assistant leaf.
    let mut leaves: Vec<&Value> = Vec::new();
    for terminal in entries.iter().filter(|e| !parent_uuids.contains(entry_uuid(e))) {
        let mut cur: Option<&Value> = Some(terminal);
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(node) = cur {
            let uid = entry_uuid(node);
            if !seen.insert(uid) {
                break;
            }
            if is_user_or_assistant(node) {
                leaves.push(node);
                break;
            }
            cur = entry_parent(node).and_then(|p| by_uuid.get(p).copied());
        }
    }

    if leaves.is_empty() {
        return Vec::new();
    }

    // Prefer a main-chain leaf (not sidechain/team/meta), highest file position.
    let main_leaves: Vec<&Value> = leaves
        .iter()
        .copied()
        .filter(|leaf| !truthy(leaf, "isSidechain") && !truthy(leaf, "teamName") && !truthy(leaf, "isMeta"))
        .collect();

    let leaf = if main_leaves.is_empty() {
        pick_best(&leaves, &entry_index)
    } else {
        pick_best(&main_leaves, &entry_index)
    };

    // Walk leaf → root, then reverse.
    let mut chain: Vec<Value> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur: Option<&Value> = Some(leaf);
    while let Some(node) = cur {
        let uid = entry_uuid(node);
        if !seen.insert(uid) {
            break;
        }
        chain.push(node.clone());
        cur = entry_parent(node).and_then(|p| by_uuid.get(p).copied());
    }
    chain.reverse();
    chain
}

/// Builds the (linear) chain for a subagent transcript. Mirrors
/// `_build_subagent_chain`: last user/assistant entry is the leaf; walk
/// `parentUuid` back to root.
pub(crate) fn build_subagent_chain(entries: &[Value]) -> Vec<Value> {
    if entries.is_empty() {
        return Vec::new();
    }

    let mut by_uuid: HashMap<&str, &Value> = HashMap::new();
    for entry in entries {
        by_uuid.insert(entry_uuid(entry), entry);
    }

    let leaf = match entries.iter().rev().find(|e| is_user_or_assistant(e)) {
        Some(l) => l,
        None => return Vec::new(),
    };

    let mut chain: Vec<Value> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur: Option<&Value> = Some(leaf);
    while let Some(node) = cur {
        let uid = entry_uuid(node);
        if !seen.insert(uid) {
            break;
        }
        chain.push(node.clone());
        cur = entry_parent(node).and_then(|p| by_uuid.get(p).copied());
    }
    chain.reverse();
    chain
}

// ---------------------------------------------------------------------------
// Visibility filter + conversion
// ---------------------------------------------------------------------------

/// Whether an entry should be included in returned messages. Mirrors
/// `_is_visible_message`: user/assistant only, drop meta/sidechain/team, KEEP
/// `isCompactSummary`.
fn is_visible_message(e: &Value) -> bool {
    is_user_or_assistant(e) && !truthy(e, "isMeta") && !truthy(e, "isSidechain") && !truthy(e, "teamName")
}

/// Converts a transcript entry into a [`SessionMessage`]. Mirrors
/// `_to_session_message`.
fn to_session_message(e: &Value) -> SessionMessage {
    let message_type = if entry_type(e) == Some("user") {
        MessageType::User
    } else {
        MessageType::Assistant
    };
    SessionMessage {
        message_type,
        uuid: entry_uuid(e).to_string(),
        session_id: e.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string(),
        message: e.get("message").cloned().unwrap_or(Value::Null),
        parent_tool_use_id: None,
    }
}

/// Applies Python-style `[offset:offset+limit]` / `[offset:]` paging.
fn paginate(messages: Vec<SessionMessage>, limit: Option<usize>, offset: usize) -> Vec<SessionMessage> {
    let len = messages.len();
    if let Some(l) = limit {
        if l > 0 {
            let start = offset.min(len);
            let end = offset.saturating_add(l).min(len);
            return messages[start..end].to_vec();
        }
    }
    if offset > 0 {
        let start = offset.min(len);
        return messages[start..].to_vec();
    }
    messages
}

/// Builds the main-session chain, applies the visibility filter, converts, and
/// pages. Mirrors `_entries_to_session_messages`.
pub(crate) fn entries_to_session_messages(
    entries: &[Value],
    limit: Option<usize>,
    offset: usize,
) -> Vec<SessionMessage> {
    let chain = build_conversation_chain(entries);
    let messages: Vec<SessionMessage> = chain
        .iter()
        .filter(|e| is_visible_message(e))
        .map(to_session_message)
        .collect();
    paginate(messages, limit, offset)
}

/// Builds the subagent chain, keeps user/assistant, converts, and pages.
/// Mirrors `_entries_to_subagent_messages`.
pub(crate) fn entries_to_subagent_messages(
    entries: &[Value],
    limit: Option<usize>,
    offset: usize,
) -> Vec<SessionMessage> {
    let chain = build_subagent_chain(entries);
    let messages: Vec<SessionMessage> = chain
        .iter()
        .filter(|e| is_user_or_assistant(e))
        .map(to_session_message)
        .collect();
    paginate(messages, limit, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uuids(chain: &[Value]) -> Vec<String> {
        chain.iter().map(|e| entry_uuid(e).to_string()).collect()
    }

    #[test]
    fn empty_input() {
        assert!(build_conversation_chain(&[]).is_empty());
    }

    #[test]
    fn single_entry() {
        let entries = vec![json!({"type": "user", "uuid": "a", "parentUuid": null})];
        let result = build_conversation_chain(&entries);
        assert_eq!(uuids(&result), vec!["a"]);
    }

    #[test]
    fn linear_chain() {
        let entries = vec![
            json!({"type": "user", "uuid": "a", "parentUuid": null}),
            json!({"type": "assistant", "uuid": "b", "parentUuid": "a"}),
            json!({"type": "user", "uuid": "c", "parentUuid": "b"}),
        ];
        let result = build_conversation_chain(&entries);
        assert_eq!(uuids(&result), vec!["a", "b", "c"]);
    }

    #[test]
    fn only_progress_entries_returns_empty() {
        let entries = vec![
            json!({"type": "progress", "uuid": "a", "parentUuid": null}),
            json!({"type": "progress", "uuid": "b", "parentUuid": "a"}),
        ];
        assert!(build_conversation_chain(&entries).is_empty());
    }

    #[test]
    fn picks_latest_leaf_by_file_position() {
        // Both leaves branch from root; new_leaf appears later in the file.
        let entries = vec![
            json!({"type": "user", "uuid": "root", "parentUuid": null}),
            json!({"type": "assistant", "uuid": "old", "parentUuid": "root"}),
            json!({"type": "assistant", "uuid": "new", "parentUuid": "root"}),
        ];
        let result = build_conversation_chain(&entries);
        assert_eq!(uuids(&result), vec!["root", "new"]);
    }

    #[test]
    fn picks_main_over_sidechain() {
        let entries = vec![
            json!({"type": "user", "uuid": "root", "parentUuid": null}),
            json!({"type": "assistant", "uuid": "main", "parentUuid": "root"}),
            json!({"type": "assistant", "uuid": "side", "parentUuid": "root", "isSidechain": true}),
        ];
        let result = build_conversation_chain(&entries);
        assert_eq!(uuids(&result), vec!["root", "main"]);
    }

    #[test]
    fn cycle_detection_returns_empty() {
        // a1 -> u1 -> a1: both are parents => no terminals => empty.
        let entries = vec![
            json!({"type": "user", "uuid": "u1", "parentUuid": "a1"}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1"}),
        ];
        assert!(build_conversation_chain(&entries).is_empty());
    }

    #[test]
    fn terminal_progress_walked_back() {
        let entries = vec![
            json!({"type": "user", "uuid": "u1", "parentUuid": null}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1"}),
            json!({"type": "progress", "uuid": "prog", "parentUuid": "a1"}),
        ];
        let result = build_conversation_chain(&entries);
        assert_eq!(uuids(&result), vec!["u1", "a1"]);
    }

    #[test]
    fn visibility_keeps_compact_summary_drops_meta() {
        let entries = vec![
            json!({"type": "user", "uuid": "u1", "parentUuid": null, "isCompactSummary": true, "message": {"content": "cs"}}),
            json!({"type": "user", "uuid": "meta", "parentUuid": "u1", "isMeta": true, "message": {"content": "m"}}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "meta", "message": {"content": "hi"}}),
        ];
        let msgs = entries_to_session_messages(&entries, None, 0);
        let ids: Vec<&str> = msgs.iter().map(|m| m.uuid.as_str()).collect();
        assert_eq!(ids, vec!["u1", "a1"]); // compact kept, meta dropped
    }

    #[test]
    fn parse_transcript_skips_non_transcript_and_corrupt() {
        let content = "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null}\n\
             not valid json {{{\n\
             \n\
             {\"type\":\"summary\",\"summary\":\"x\"}\n\
             {\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\"}\n";
        let entries = parse_transcript_entries(content);
        assert_eq!(uuids(&entries), vec!["u1", "a1"]);
    }

    #[test]
    fn subagent_chain_linear() {
        let entries = vec![
            json!({"type": "user", "uuid": "u1", "parentUuid": null, "sessionId": "s", "message": {"content": "task"}}),
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1", "sessionId": "s", "message": {"content": "done"}}),
        ];
        let msgs = entries_to_subagent_messages(&entries, None, 0);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].message_type, MessageType::User);
        assert_eq!(msgs[0].session_id, "s");
    }
}
