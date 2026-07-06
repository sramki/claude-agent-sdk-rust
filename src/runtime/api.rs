//! Public entry points: the one-shot [`query`] function and the shared setup
//! used by both `query` and [`Client`](super::client::Client).
//!
//! Faithful port of the public `query.py` and the `_internal/client.py`
//! `process_query` setup (minus the session-store resume path, handled
//! elsewhere).

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;

use super::message_parser::parse_message;
use super::query::{Query, QueryConfig};
use super::transport::{SubprocessCliTransport, Transport};
use crate::error::{Error, Result};
use crate::types::{
    ClaudeAgentOptions, McpServerConfig, McpServers, Message, PermissionMode, Skills, SystemPrompt,
};

/// A prompt for [`query`] or [`Client`](super::client::Client): a single string
/// or a sequence of raw input message objects. Mirrors Python's `str |
/// AsyncIterable[dict]` (bounded to a `Vec` here; use the client for open-ended
/// interactive streaming).
#[derive(Debug, Clone)]
pub enum Prompt {
    /// A single user prompt string.
    Text(String),
    /// A sequence of raw input message objects.
    Messages(Vec<Value>),
}

impl From<&str> for Prompt {
    fn from(s: &str) -> Self {
        Prompt::Text(s.to_string())
    }
}
impl From<String> for Prompt {
    fn from(s: String) -> Self {
        Prompt::Text(s)
    }
}
impl From<Vec<Value>> for Prompt {
    fn from(v: Vec<Value>) -> Self {
        Prompt::Messages(v)
    }
}

/// A stream of parsed [`Message`]s. Unknown message types are skipped; a fatal
/// error yields a single `Err` and ends the stream.
pub struct MessageStream {
    inner: ReceiverStream<Result<Message>>,
}

impl MessageStream {
    pub(crate) fn from_receiver(inner: ReceiverStream<Result<Message>>) -> Self {
        MessageStream { inner }
    }

    /// Receives the next message, or `None` at end of stream.
    pub async fn next(&mut self) -> Option<Result<Message>> {
        self.inner.next().await
    }
}

impl Stream for MessageStream {
    type Item = Result<Message>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// Computes the initialize timeout from `CLAUDE_CODE_STREAM_CLOSE_TIMEOUT`
/// (milliseconds), clamped to a 60s minimum. Mirrors the upstream calculation.
pub(crate) fn initialize_timeout() -> Duration {
    let ms: u64 = std::env::var("CLAUDE_CODE_STREAM_CLOSE_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000);
    Duration::from_millis(ms.max(60_000))
}

/// Extracts the SDK MCP servers, agent wire dicts, `excludeDynamicSections`,
/// and explicit skills list from options for the `initialize` request.
fn derive_query_config(
    options: &ClaudeAgentOptions,
    is_streaming_mode: bool,
) -> QueryConfig {
    let mut sdk_mcp_servers = HashMap::new();
    if let McpServers::Map(map) = &options.mcp_servers {
        for (name, config) in map {
            if let McpServerConfig::Sdk { instance, .. } = config {
                sdk_mcp_servers.insert(name.clone(), instance.clone());
            }
        }
    }

    let agents = options.agents.as_ref().map(|agents| {
        let mut m = Map::new();
        for (name, def) in agents {
            m.insert(name.clone(), def.to_wire());
        }
        m
    });

    let exclude_dynamic_sections = match &options.system_prompt {
        Some(SystemPrompt::Preset(p)) => p.exclude_dynamic_sections,
        _ => None,
    };

    let skills = match &options.skills {
        Some(Skills::List(list)) => Some(list.clone()),
        _ => None,
    };

    QueryConfig {
        is_streaming_mode,
        can_use_tool: options.can_use_tool.clone(),
        hooks: options.hooks.clone().unwrap_or_default(),
        sdk_mcp_servers,
        agents,
        exclude_dynamic_sections,
        skills,
        initialize_timeout: initialize_timeout(),
    }
}

/// Validates permission options, configures `permission_prompt_tool_name`,
/// builds and connects the transport, and starts + initializes the `Query`.
/// Shared by [`query`] and the client. `prompt_is_string` is used only for the
/// `can_use_tool`-requires-streaming check.
pub(crate) async fn setup_query(
    mut options: ClaudeAgentOptions,
    prompt_is_string: bool,
    custom_transport: Option<Box<dyn Transport>>,
) -> Result<Query> {
    if options.can_use_tool.is_some() {
        if prompt_is_string {
            return Err(Error::connection(
                "can_use_tool callback requires streaming mode. Provide a message-sequence prompt instead of a string.",
            ));
        }
        if options.permission_prompt_tool_name.is_some() {
            return Err(Error::connection(
                "can_use_tool callback cannot be used with permission_prompt_tool_name. Use one or the other.",
            ));
        }
        warn_if_can_use_tool_shadowed(&options);
        options.permission_prompt_tool_name = Some("stdio".to_string());
    }

    let config = derive_query_config(&options, true);

    let mut transport: Box<dyn Transport> = match custom_transport {
        Some(t) => t,
        None => Box::new(SubprocessCliTransport::new(options)),
    };
    transport.connect().await?;

    let mut query = Query::new(transport, config);
    query.start();
    query.initialize().await?;
    Ok(query)
}

/// Drives a fully-set-up `Query` to completion, forwarding parsed messages to
/// `out`, then closes the query. Used by [`query`].
async fn run_to_completion(mut query: Query, out: mpsc::Sender<Result<Message>>) {
    if let Some(mut rx) = query.take_messages() {
        while let Some(item) = rx.recv().await {
            if !forward(item, &out).await {
                break;
            }
        }
    }
    let _ = query.close().await;
}

/// Forwards one raw item as a parsed message. Returns `false` to stop.
pub(crate) async fn forward(item: Result<Value>, out: &mpsc::Sender<Result<Message>>) -> bool {
    match item {
        Ok(value) => match parse_message(&value) {
            Ok(Some(message)) => out.send(Ok(message)).await.is_ok(),
            Ok(None) => true,
            Err(e) => {
                let _ = out.send(Err(e)).await;
                false
            }
        },
        Err(e) => {
            let _ = out.send(Err(e)).await;
            false
        }
    }
}

/// Queries Claude Code for a one-shot (or unidirectional streaming) interaction.
///
/// Returns a [`MessageStream`] of parsed messages. For interactive, stateful
/// conversations use [`Client`](super::client::Client) instead. Faithful port
/// of the public `query()`.
pub async fn query(prompt: impl Into<Prompt>, options: ClaudeAgentOptions) -> Result<MessageStream> {
    let prompt = prompt.into();
    let prompt_is_string = matches!(prompt, Prompt::Text(_));
    let query = setup_query(options, prompt_is_string, None).await?;

    match prompt {
        Prompt::Text(s) => {
            query.write_user_message(&s, "").await?;
            query.spawn_wait_and_end();
        }
        Prompt::Messages(messages) => {
            query.spawn_stream_input(messages);
        }
    }

    let (tx, rx) = mpsc::channel::<Result<Message>>(100);
    tokio::spawn(run_to_completion(query, tx));
    Ok(MessageStream {
        inner: ReceiverStream::new(rx),
    })
}

/// Queries with a custom transport. Faithful to `query(transport=...)`.
pub async fn query_with_transport(
    prompt: impl Into<Prompt>,
    options: ClaudeAgentOptions,
    transport: Box<dyn Transport>,
) -> Result<MessageStream> {
    let prompt = prompt.into();
    let prompt_is_string = matches!(prompt, Prompt::Text(_));
    let query = setup_query(options, prompt_is_string, Some(transport)).await?;

    match prompt {
        Prompt::Text(s) => {
            query.write_user_message(&s, "").await?;
            query.spawn_wait_and_end();
        }
        Prompt::Messages(messages) => {
            query.spawn_stream_input(messages);
        }
    }

    let (tx, rx) = mpsc::channel::<Result<Message>>(100);
    tokio::spawn(run_to_completion(query, tx));
    Ok(MessageStream {
        inner: ReceiverStream::new(rx),
    })
}

// ---------------------------------------------------------------------------
// can_use_tool shadowing advisory (port of types.py helpers)
// ---------------------------------------------------------------------------

/// Returns the whole tool an `allowed_tools` entry allows outright, else `None`.
/// Mirrors `_whole_tool_allowed`.
fn whole_tool_allowed(entry: &str) -> Option<&str> {
    if entry.trim().is_empty() {
        return None;
    }
    match entry.find('(') {
        None => Some(entry),
        Some(0) => None,
        Some(open) => {
            if !entry.ends_with(')') {
                return None;
            }
            let inner = &entry[open + 1..entry.len() - 1];
            if inner.is_empty() || inner == "*" {
                Some(&entry[..open])
            } else {
                None
            }
        }
    }
}

/// Emits an advisory warning if `can_use_tool` is visibly shadowed by the
/// options. Mirrors `_warn_if_can_use_tool_shadowed` (as a stderr warning).
pub(crate) fn warn_if_can_use_tool_shadowed(options: &ClaudeAgentOptions) {
    if options.can_use_tool.is_none() {
        return;
    }
    let mut allowed = options.allowed_tools.clone();
    if matches!(options.skills, Some(Skills::All)) && !allowed.iter().any(|t| t == "Skill") {
        allowed.push("Skill".to_string());
    }

    if options.permission_mode == Some(PermissionMode::BypassPermissions) {
        warn_once(
            "can_use_tool will not be invoked: permission_mode 'bypassPermissions' auto-approves every tool call before the callback is consulted. Use a PreToolUse hook to gate every tool call.",
        );
        return;
    }

    let mut shadowed: Vec<&str> = Vec::new();
    for entry in &allowed {
        if let Some(tool) = whole_tool_allowed(entry) {
            if !shadowed.contains(&tool) {
                shadowed.push(tool);
            }
        }
    }
    if !shadowed.is_empty() {
        warn_once(&format!(
            "can_use_tool will not be invoked for: {}. An allowed_tools entry that allows a whole tool auto-approves it before the callback is consulted.",
            shadowed.join(", ")
        ));
    }
}

/// Emits a warning to stderr at most once per process per distinct message,
/// mirroring Python's default `warnings` once-per-message behavior.
fn warn_once(message: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if seen.lock().unwrap().insert(message.to_string()) {
        eprintln!("warning: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_tool_allowed_cases() {
        // Ported from test_option_warnings.py::test_whole_tool_allowed.
        let cases: &[(&str, Option<&str>)] = &[
            ("Read", Some("Read")),
            ("mcp__server__tool", Some("mcp__server__tool")),
            ("Read(*)", Some("Read")),
            ("Read()", Some("Read")),
            ("mcp__server__tool(*)", Some("mcp__server__tool")),
            ("Bash(ls:*)", None),
            ("Bash(git log:*)", None),
            ("Bash(*.py)", None),
            ("", None),
            ("   ", None),
            ("Bash(ls:*", None), // never closes
            ("Bash(ls)x", None), // trailing after close
            ("(foo)", None),     // no tool name before the paren
            ("(*)", None),       // empty tool name guard
            ("Read(*x", None),   // never closes
        ];
        for (entry, expected) in cases {
            assert_eq!(whole_tool_allowed(entry), *expected, "entry: {entry:?}");
        }
    }

    #[test]
    fn warn_once_dedupes_and_no_panic() {
        // Emitting is a stderr side effect; assert it doesn't panic and dedupes
        // (the second call is a no-op — we can only observe it doesn't crash).
        warn_once("test parity warning message");
        warn_once("test parity warning message");
    }

    #[test]
    fn prompt_from_conversions() {
        assert!(matches!(Prompt::from("hi"), Prompt::Text(_)));
        assert!(matches!(Prompt::from(vec![]), Prompt::Messages(_)));
    }
}
