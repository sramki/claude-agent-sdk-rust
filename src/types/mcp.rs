//! MCP (Model Context Protocol) server configuration and status types.
//!
//! Ported from the MCP section of the Python `types.py`. The three transport
//! configs (stdio/SSE/HTTP) are serializable wire types passed to the CLI; the
//! `sdk` variant carries an in-process server instance (see [`McpServerInstance`]
//! and the `mcp` module) that the runtime dispatches `mcp_message` control
//! requests to. The status/context types are output-only and use wire-format
//! (camelCase) field names.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// MCP stdio server configuration. Mirrors `McpStdioServerConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpStdioServerConfig {
    /// Command to launch the server.
    pub command: String,
    /// Command arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment overrides.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

/// MCP SSE server configuration. Mirrors `McpSSEServerConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpSseServerConfig {
    /// Server URL.
    pub url: String,
    /// Optional headers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

/// MCP HTTP server configuration. Mirrors `McpHttpServerConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpHttpServerConfig {
    /// Server URL.
    pub url: String,
    /// Optional headers.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

/// An in-process SDK MCP server that the runtime dispatches JSON-RPC messages
/// to. Implemented by the concrete server in the `mcp` module (see
/// [`create_sdk_mcp_server`](crate::create_sdk_mcp_server)).
#[async_trait]
pub trait McpServerInstance: Debug + Send + Sync {
    /// The server's configured name.
    fn name(&self) -> &str;

    /// Handles one JSON-RPC request and returns the JSON-RPC response.
    async fn handle_message(&self, message: Value) -> Value;
}

/// MCP server configuration. Mirrors the `McpServerConfig` union.
#[derive(Debug, Clone)]
pub enum McpServerConfig {
    /// A stdio server.
    Stdio(McpStdioServerConfig),
    /// An SSE server.
    Sse(McpSseServerConfig),
    /// An HTTP server.
    Http(McpHttpServerConfig),
    /// An in-process SDK server.
    Sdk {
        /// Server name.
        name: String,
        /// The server instance.
        instance: Arc<dyn McpServerInstance>,
    },
}

impl McpServerConfig {
    /// Serializes the config to the CLI wire format. The `sdk` variant is
    /// represented by only its serializable fields (`{"type": "sdk", "name":
    /// ...}`) — the in-process instance is wired separately over the control
    /// protocol.
    pub fn to_wire(&self) -> Value {
        match self {
            McpServerConfig::Stdio(c) => {
                let mut v = serde_json::to_value(c).unwrap_or_else(|_| Value::Object(Map::new()));
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("type".into(), Value::String("stdio".into()));
                }
                v
            }
            McpServerConfig::Sse(c) => {
                let mut v = serde_json::to_value(c).unwrap_or_else(|_| Value::Object(Map::new()));
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("type".into(), Value::String("sse".into()));
                }
                v
            }
            McpServerConfig::Http(c) => {
                let mut v = serde_json::to_value(c).unwrap_or_else(|_| Value::Object(Map::new()));
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("type".into(), Value::String("http".into()));
                }
                v
            }
            McpServerConfig::Sdk { name, .. } => {
                serde_json::json!({"type": "sdk", "name": name})
            }
        }
    }
}

/// Tool annotations returned in MCP server status. Mirrors
/// `McpToolAnnotations` (wire camelCase).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct McpToolAnnotations {
    /// Whether the tool is read-only.
    #[serde(rename = "readOnly", default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    /// Whether the tool is destructive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    /// Whether the tool touches the open world.
    #[serde(rename = "openWorld", default, skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
}

/// Information about a tool provided by an MCP server. Mirrors `McpToolInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// Tool name.
    pub name: String,
    /// Tool description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tool annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
}

/// Server info from the MCP initialize handshake. Mirrors `McpServerInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerInfo {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
}

/// Connection status for an MCP server. Mirrors `McpServerConnectionStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpServerConnectionStatus {
    /// Connected.
    Connected,
    /// Failed.
    Failed,
    /// Needs authentication.
    NeedsAuth,
    /// Pending.
    Pending,
    /// Disabled.
    Disabled,
}

/// Status information for an MCP server connection. Mirrors `McpServerStatus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerStatus {
    /// Server name as configured.
    pub name: String,
    /// Current connection status.
    pub status: McpServerConnectionStatus,
    /// Server info from the handshake (when connected).
    #[serde(rename = "serverInfo", default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<McpServerInfo>,
    /// Error message (when failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Server configuration (raw, includes URL for HTTP/SSE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// Configuration scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Tools provided (when connected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<McpToolInfo>>,
}

/// Response from `Client::get_mcp_status`. Mirrors `McpStatusResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpStatusResponse {
    /// The per-server statuses.
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<McpServerStatus>,
}

/// A single context-usage category. Mirrors `ContextUsageCategory`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsageCategory {
    /// Category name.
    pub name: String,
    /// Tokens in this category.
    pub tokens: i64,
    /// Display color.
    pub color: String,
    /// Whether this category is deferred.
    #[serde(rename = "isDeferred", default, skip_serializing_if = "Option::is_none")]
    pub is_deferred: Option<bool>,
}

/// Response from `Client::get_context_usage`. Mirrors `ContextUsageResponse`.
/// Only the stable, always-present fields are modeled explicitly; the many
/// optional CLI-visualization fields are captured in [`extra`](Self::extra).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsageResponse {
    /// Token usage by category.
    pub categories: Vec<ContextUsageCategory>,
    /// Total tokens currently in the context window.
    #[serde(rename = "totalTokens")]
    pub total_tokens: i64,
    /// Effective maximum tokens.
    #[serde(rename = "maxTokens")]
    pub max_tokens: i64,
    /// Raw model context window size.
    #[serde(rename = "rawMaxTokens")]
    pub raw_max_tokens: i64,
    /// Percentage of the context window used (0–100).
    pub percentage: f64,
    /// Model name.
    pub model: String,
    /// Whether autocompact is enabled.
    #[serde(rename = "isAutoCompactEnabled")]
    pub is_auto_compact_enabled: bool,
    /// Any additional CLI visualization fields, captured verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stdio_to_wire_includes_type() {
        let c = McpServerConfig::Stdio(McpStdioServerConfig {
            command: "node".into(),
            args: vec!["server.js".into()],
            env: HashMap::new(),
        });
        assert_eq!(
            c.to_wire(),
            json!({"type": "stdio", "command": "node", "args": ["server.js"]})
        );
    }

    #[test]
    fn sdk_to_wire_omits_instance() {
        #[derive(Debug)]
        struct Dummy;
        #[async_trait]
        impl McpServerInstance for Dummy {
            fn name(&self) -> &str {
                "dummy"
            }
            async fn handle_message(&self, _m: Value) -> Value {
                Value::Null
            }
        }
        let c = McpServerConfig::Sdk {
            name: "dummy".into(),
            instance: Arc::new(Dummy),
        };
        assert_eq!(c.to_wire(), json!({"type": "sdk", "name": "dummy"}));
    }

    #[test]
    fn connection_status_kebab() {
        assert_eq!(
            serde_json::to_value(McpServerConnectionStatus::NeedsAuth).unwrap(),
            json!("needs-auth")
        );
    }
}
