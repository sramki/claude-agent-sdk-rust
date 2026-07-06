//! Public data types returned by the session-reading API.
//!
//! These mirror the `SDKSessionInfo` and `SessionMessage` dataclasses from the
//! Python SDK's `types.py`, with idiomatic Rust field types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The type of a conversation message — either a user turn or an assistant turn.
///
/// Mirrors the `Literal["user", "assistant"]` on Python's `SessionMessage.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    /// A user message.
    User,
    /// An assistant message.
    Assistant,
}

/// A user or assistant message from a session transcript.
///
/// Returned by [`get_session_messages`](crate::get_session_messages) and
/// [`get_subagent_messages`](crate::get_subagent_messages). The [`message`]
/// field is the raw Anthropic API message value (role, content, ...), preserved
/// exactly as it appears on disk — this crate never rewrites it.
///
/// [`message`]: SessionMessage::message
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessage {
    /// Message type — [`MessageType::User`] or [`MessageType::Assistant`].
    #[serde(rename = "type")]
    pub message_type: MessageType,
    /// Unique message identifier (the transcript entry `uuid`).
    pub uuid: String,
    /// ID of the session this message belongs to (the entry `sessionId`).
    pub session_id: String,
    /// Raw Anthropic API message value (`role`, `content`, ...). Untouched.
    pub message: Value,
    /// Always `None` for top-level conversation messages. Present for API
    /// parity with the Python `parent_tool_use_id` field.
    pub parent_tool_use_id: Option<String>,
}

/// Session metadata extracted from `stat` + head/tail reads.
///
/// Returned by [`list_sessions`](crate::list_sessions) and
/// [`get_session_info`](crate::get_session_info). Contains only data
/// obtainable without a full JSONL parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionInfo {
    /// Unique session identifier (UUID).
    pub session_id: String,
    /// Display title — custom title, auto-generated summary, or first prompt.
    pub summary: String,
    /// Last-modified time in milliseconds since the Unix epoch.
    pub last_modified: i64,
    /// Session file size in bytes.
    pub file_size: Option<u64>,
    /// User-set custom title, or the AI-generated title, if any.
    pub custom_title: Option<String>,
    /// First meaningful user prompt in the session.
    pub first_prompt: Option<String>,
    /// Git branch recorded for the session.
    pub git_branch: Option<String>,
    /// Working directory recorded for the session.
    pub cwd: Option<String>,
    /// User-set session tag.
    pub tag: Option<String>,
    /// Creation time in milliseconds since the Unix epoch, parsed from the
    /// first entry's ISO `timestamp` field.
    pub created_at: Option<i64>,
}
