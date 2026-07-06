//! # claude-agent-sdk-rs
//!
//! An idiomatic Rust port of Anthropic's
//! [Claude Agent SDK](https://github.com/anthropics/claude-agent-sdk-python)
//! for Python (pinned to v0.2.110). Read local **Claude Code** session history
//! *and* drive the live `claude` runtime over the stream-json protocol.
//!
//! `Result`-based, serde-typed, `tokio`-async; callbacks are `Arc`-wrapped
//! closures. The library is imported as `claude_agent_sdk_rs`.
//!
//! ## What it does
//!
//! **Live runtime** — [`query`] (one-shot / unidirectional streaming) and
//! [`Client`] (bidirectional, interactive) drive the `claude` CLI over the
//! stream-json control protocol, with hooks, permission callbacks
//! ([`CanUseTool`]), and in-process MCP tool servers
//! ([`create_sdk_mcp_server`] / [`tool`]).
//!
//! **Session reader** (filesystem, no CLI) — [`list_sessions`],
//! [`get_session_info`], [`get_session_messages`], [`list_subagents`] /
//! [`get_subagent_messages`].
//!
//! **Session write ops** — [`rename_session`], [`tag_session`],
//! [`delete_session`], [`fork_session`], and the [`InMemorySessionStore`].
//!
//! Reads degrade gracefully — a missing directory, unreadable file, or
//! malformed line yields `Ok` (empty), not an error. The [`Error`] path is
//! reserved for caller mistakes such as a malformed session id.
//!
//! ## Example
//!
//! ```no_run
//! use std::path::Path;
//! use claude_agent_sdk_rs::{list_sessions, get_session_messages, MessageType};
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
//! # Ok::<(), claude_agent_sdk_rs::Error>(())
//! ```
//!
//! ## License
//!
//! MIT, matching the upstream Python SDK. See `LICENSE` and `NOTICE`.

mod chain;
mod error;
pub mod mcp;
mod mutations;
mod parse;
mod paths;
pub mod runtime;
mod sessions;
pub mod store;
mod store_import;
mod store_read;
pub mod types;

pub use error::{Error, Result};
pub use mcp::{create_sdk_mcp_server, tool, SdkMcpTool, ToolAnnotations};
pub use mutations::{
    delete_session, delete_session_via_store, fork_session, fork_session_via_store,
    project_key_for_directory, rename_session, rename_session_via_store, tag_session,
    tag_session_via_store, ForkSessionResult,
};
pub use runtime::{
    parse_message, query, query_with_transport, Client, MessageStream, Prompt,
    SubprocessCliTransport, Transport,
};
pub use sessions::{
    get_session_info, get_session_messages, get_subagent_messages, list_sessions, list_subagents,
};
pub use store::{
    file_path_to_session_key, fold_session_summary, summary_entry_to_sdk_info, InMemorySessionStore,
};
pub use store_import::import_session_to_store;
pub use store_read::{
    get_session_info_from_store, get_session_messages_from_store, get_subagent_messages_from_store,
    list_sessions_from_store, list_subagents_from_store,
};
pub use types::*;
