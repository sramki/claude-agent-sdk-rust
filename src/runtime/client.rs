//! [`Client`] — bidirectional, interactive conversations.
//!
//! Faithful port of `client.py` (`ClaudeSDKClient`). Holds a connected
//! [`Query`] and exposes send/receive plus the control operations. The message
//! stream is taken once via [`messages`](Client::messages); control methods
//! (`query`, `interrupt`, ...) borrow `&self` so they can be called alongside a
//! live message stream.

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::api::{forward, setup_query, MessageStream, Prompt};
use super::query::Query;
use super::transport::Transport;
use crate::error::{Error, Result};
use crate::types::{
    ClaudeAgentOptions, ContextUsageResponse, McpStatusResponse, Message, PermissionMode,
};

/// Client for bidirectional, interactive conversations with Claude Code.
///
/// Create with [`Client::new`], [`connect`](Client::connect) with an optional
/// initial prompt, drive with [`query`](Client::query), and read replies via
/// [`messages`](Client::messages) (a single continuous stream — watch for a
/// [`Message::Result`] to know a response is complete).
pub struct Client {
    options: Option<ClaudeAgentOptions>,
    custom_transport: Option<Box<dyn Transport>>,
    query: Option<Query>,
}

impl Client {
    /// Creates a client with the given options (not yet connected).
    pub fn new(options: ClaudeAgentOptions) -> Self {
        Client {
            options: Some(options),
            custom_transport: None,
            query: None,
        }
    }

    /// Creates a client that uses a custom transport instead of spawning the
    /// CLI. Faithful to `ClaudeSDKClient(transport=...)`.
    pub fn with_transport(options: ClaudeAgentOptions, transport: Box<dyn Transport>) -> Self {
        Client {
            options: Some(options),
            custom_transport: Some(transport),
            query: None,
        }
    }

    /// Connects to Claude, optionally sending an initial prompt.
    ///
    /// Pass `None` for interactive use (stdin stays open for later
    /// [`query`](Client::query) calls), `Some(Prompt::Text(..))` to send one
    /// message, or `Some(Prompt::Messages(..))` to stream a bounded sequence
    /// (stdin closes after the first result).
    pub async fn connect(&mut self, prompt: Option<Prompt>) -> Result<()> {
        let options = self
            .options
            .clone()
            .ok_or_else(|| Error::connection("Client already connected"))?;
        let prompt_is_string = matches!(prompt, Some(Prompt::Text(_)));
        let custom = self.custom_transport.take();

        let query = setup_query(options, prompt_is_string, custom).await?;

        match prompt {
            Some(Prompt::Text(s)) => {
                query.write_user_message(&s, "default").await?;
            }
            Some(Prompt::Messages(messages)) => {
                query.spawn_stream_input(messages);
            }
            None => {}
        }

        self.query = Some(query);
        Ok(())
    }

    fn query_ref(&self) -> Result<&Query> {
        self.query
            .as_ref()
            .ok_or_else(|| Error::connection("Not connected. Call connect() first."))
    }

    /// Sends a new request in streaming mode. A string is wrapped as a user
    /// message; a sequence is written verbatim (with `session_id` injected).
    pub async fn query(&self, prompt: impl Into<Prompt>, session_id: &str) -> Result<()> {
        let query = self.query_ref()?;
        match prompt.into() {
            Prompt::Text(s) => query.write_user_message(&s, session_id).await,
            Prompt::Messages(messages) => query.write_messages(messages, session_id).await,
        }
    }

    /// Takes the message stream (callable once). Yields parsed [`Message`]s;
    /// unknown types are skipped and a fatal error ends the stream.
    pub fn messages(&mut self) -> MessageStream {
        let (tx, rx) = mpsc::channel::<Result<Message>>(100);
        if let Some(query) = self.query.as_mut() {
            if let Some(mut raw) = query.take_messages() {
                tokio::spawn(async move {
                    while let Some(item) = raw.recv().await {
                        if !forward(item, &tx).await {
                            break;
                        }
                    }
                });
            }
        }
        MessageStream::from_receiver(ReceiverStream::new(rx))
    }

    /// Sends an interrupt signal.
    pub async fn interrupt(&self) -> Result<()> {
        self.query_ref()?.interrupt().await
    }

    /// Changes the permission mode mid-conversation.
    pub async fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        self.query_ref()?.set_permission_mode(mode).await
    }

    /// Changes the model mid-conversation.
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.query_ref()?.set_model(model).await
    }

    /// Rewinds tracked files to a user message.
    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        self.query_ref()?.rewind_files(user_message_id).await
    }

    /// Reconnects a disconnected or failed MCP server.
    pub async fn reconnect_mcp_server(&self, server_name: &str) -> Result<()> {
        self.query_ref()?.reconnect_mcp_server(server_name).await
    }

    /// Enables or disables an MCP server.
    pub async fn toggle_mcp_server(&self, server_name: &str, enabled: bool) -> Result<()> {
        self.query_ref()?
            .toggle_mcp_server(server_name, enabled)
            .await
    }

    /// Stops a running task.
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.query_ref()?.stop_task(task_id).await
    }

    /// Gets the current MCP server connection status.
    pub async fn get_mcp_status(&self) -> Result<McpStatusResponse> {
        let raw = self.query_ref()?.get_mcp_status().await?;
        serde_json::from_value(raw)
            .map_err(|e| Error::message_parse(format!("Invalid mcp_status response: {e}"), None))
    }

    /// Gets the current context-usage breakdown.
    pub async fn get_context_usage(&self) -> Result<ContextUsageResponse> {
        let raw = self.query_ref()?.get_context_usage().await?;
        serde_json::from_value(raw).map_err(|e| {
            Error::message_parse(format!("Invalid context_usage response: {e}"), None)
        })
    }

    /// Returns the server initialization info captured during connect.
    pub fn get_server_info(&self) -> Option<&Value> {
        self.query.as_ref().and_then(Query::server_info)
    }

    /// Disconnects, closing the query and its transport.
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut query) = self.query.take() {
            query.close().await?;
        }
        Ok(())
    }
}
