//! # claude-agent-sdk (Rust)
//!
//! Read local **Claude Code** session history from
//! `~/.claude/projects/**/*.jsonl`. This crate is a faithful Rust port of the
//! *session-reading* functionality of Anthropic's
//! [`claude-agent-sdk`](https://github.com/anthropics/claude-agent-sdk-python)
//! for Python (the filesystem path in `_internal/sessions.py`). It does **not**
//! port the live-streaming `query()` client or the async `SessionStore`
//! backend — reading local transcript files is the entire scope.
//!
//! ## What it does
//!
//! - [`list_sessions`] — enumerate sessions (metadata from `stat` + head/tail
//!   reads, no full parse), sorted newest-first, with pagination.
//! - [`get_session_info`] — metadata for one session by UUID.
//! - [`get_session_messages`] — the reconstructed user/assistant conversation,
//!   in chronological order (the transcript DAG is walked via `parentUuid`
//!   links and collapsed to a single most-recent branch).
//! - [`list_subagents`] / [`get_subagent_messages`] — subagent transcripts
//!   stored under `<session>/subagents/`.
//!
//! Config home is `$CLAUDE_CONFIG_DIR` (if set) else `~/.claude`. Reads degrade
//! gracefully — a missing directory, unreadable file, or malformed line yields
//! an empty result (`Ok`), not an error. The [`Error`] path is reserved for
//! caller mistakes such as a malformed session id.
//!
//! ## Example
//!
//! ```no_run
//! use std::path::Path;
//! use claude_agent_sdk::{list_sessions, get_session_messages, MessageType};
//!
//! // Newest 20 sessions for a project (scanning git worktrees too).
//! let dir = Path::new("/path/to/project");
//! for info in list_sessions(Some(dir), Some(20), 0, true)? {
//!     println!("{}  {}", info.session_id, info.summary);
//! }
//!
//! // The full conversation of one session.
//! for msg in get_session_messages("550e8400-e29b-41d4-a716-446655440000", Some(dir), None, 0)? {
//!     let who = match msg.message_type {
//!         MessageType::User => "user",
//!         MessageType::Assistant => "assistant",
//!     };
//!     println!("[{who}] {}", msg.message);
//! }
//! # Ok::<(), claude_agent_sdk::Error>(())
//! ```
//!
//! ## License
//!
//! MIT, matching the upstream Python SDK. See `LICENSE` and `NOTICE`.

mod chain;
mod error;
mod parse;
mod paths;
pub mod runtime;
mod sessions;
pub mod types;

pub use error::{Error, Result};
pub use runtime::parse_message;
pub use sessions::{
    get_session_info, get_session_messages, get_subagent_messages, list_sessions, list_subagents,
};
pub use types::*;
