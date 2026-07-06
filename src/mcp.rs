//! In-process SDK MCP servers.
//!
//! Port of the `create_sdk_mcp_server` / `tool` / `SdkMcpTool` helpers from the
//! Python `__init__.py`. An SDK MCP server runs in-process; the runtime routes
//! `mcp_message` control requests to it (see [`McpServerInstance`]). Unlike the
//! Python version — which introspects Python type annotations — the Rust `tool`
//! takes a JSON-Schema [`Value`] directly, which is the idiomatic equivalent.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::types::{BoxFuture, McpServerConfig, McpServerInstance};

/// Anthropic/MCP tool annotations. Mirrors the fields used by the SDK's
/// `ToolAnnotations`; `max_result_size_chars` is emitted under `_meta` as
/// `anthropic/maxResultSizeChars`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    /// Human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether the tool is read-only.
    #[serde(
        rename = "readOnlyHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
    /// Whether the tool is destructive.
    #[serde(
        rename = "destructiveHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive_hint: Option<bool>,
    /// Whether the tool is idempotent.
    #[serde(
        rename = "idempotentHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotent_hint: Option<bool>,
    /// Whether the tool touches the open world.
    #[serde(
        rename = "openWorldHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub open_world_hint: Option<bool>,
    /// CLI layer-2 tool-result spill threshold (emitted via `_meta`).
    #[serde(skip)]
    pub max_result_size_chars: Option<i64>,
}

/// The async handler for a tool: `(arguments) -> {content, is_error}`.
pub type ToolHandler =
    Arc<dyn Fn(Map<String, Value>) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

/// Definition of an SDK MCP tool. Mirrors `SdkMcpTool`.
#[derive(Clone)]
pub struct SdkMcpTool {
    /// Unique tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON-Schema for the tool input.
    pub input_schema: Value,
    /// The tool handler.
    pub handler: ToolHandler,
    /// Optional annotations.
    pub annotations: Option<ToolAnnotations>,
}

impl std::fmt::Debug for SdkMcpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkMcpTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("handler", &"<handler>")
            .field("annotations", &self.annotations)
            .finish()
    }
}

/// Defines an SDK MCP tool. Mirrors the `@tool` decorator. `input_schema` is a
/// JSON-Schema object (e.g. `json!({"type":"object","properties":{...}})`); the
/// handler is an async function taking the argument map.
pub fn tool<F, Fut>(
    name: impl Into<String>,
    description: impl Into<String>,
    input_schema: Value,
    handler: F,
) -> SdkMcpTool
where
    F: Fn(Map<String, Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    SdkMcpTool {
        name: name.into(),
        description: description.into(),
        input_schema,
        handler: Arc::new(move |args| Box::pin(handler(args))),
        annotations: None,
    }
}

/// An in-process MCP server instance.
struct SdkMcpServer {
    name: String,
    version: String,
    tools: HashMap<String, SdkMcpTool>,
    tool_list: Vec<Value>,
}

impl std::fmt::Debug for SdkMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkMcpServer")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn jsonrpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

#[async_trait]
impl McpServerInstance for SdkMcpServer {
    fn name(&self) -> &str {
        &self.name
    }

    async fn handle_message(&self, message: Value) -> Value {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => jsonrpc_result(
                id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": self.name, "version": self.version},
                }),
            ),
            "tools/list" => jsonrpc_result(id, json!({"tools": self.tool_list})),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = params
                    .get("arguments")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let Some(tool_def) = self.tools.get(tool_name) else {
                    return jsonrpc_error(id, -32603, format!("Tool '{tool_name}' not found"));
                };
                match (tool_def.handler)(arguments).await {
                    Ok(result) => {
                        let content = result.get("content").cloned().unwrap_or_else(|| json!([]));
                        let mut resp = Map::new();
                        resp.insert("content".into(), content);
                        if result.get("is_error").and_then(Value::as_bool) == Some(true) {
                            resp.insert("isError".into(), json!(true));
                        }
                        jsonrpc_result(id, Value::Object(resp))
                    }
                    Err(e) => jsonrpc_error(id, -32603, e.to_string()),
                }
            }
            "notifications/initialized" => json!({"jsonrpc": "2.0", "result": {}}),
            other => jsonrpc_error(id, -32601, format!("Method '{other}' not found")),
        }
    }
}

fn build_tool_list_entry(tool: &SdkMcpTool) -> Value {
    let mut entry = Map::new();
    entry.insert("name".into(), json!(tool.name));
    entry.insert("description".into(), json!(tool.description));
    entry.insert("inputSchema".into(), tool.input_schema.clone());
    if let Some(ann) = &tool.annotations {
        entry.insert(
            "annotations".into(),
            serde_json::to_value(ann).unwrap_or(Value::Null),
        );
        if let Some(max) = ann.max_result_size_chars {
            entry.insert("_meta".into(), json!({"anthropic/maxResultSizeChars": max}));
        }
    }
    Value::Object(entry)
}

/// Creates an in-process SDK MCP server config for use in
/// [`ClaudeAgentOptions::mcp_servers`](crate::ClaudeAgentOptions). Mirrors
/// `create_sdk_mcp_server`.
pub fn create_sdk_mcp_server(
    name: impl Into<String>,
    version: impl Into<String>,
    tools: Vec<SdkMcpTool>,
) -> McpServerConfig {
    let name = name.into();
    let tool_list = tools.iter().map(build_tool_list_entry).collect();
    let tool_map = tools.into_iter().map(|t| (t.name.clone(), t)).collect();
    let server = SdkMcpServer {
        name: name.clone(),
        version: version.into(),
        tools: tool_map,
        tool_list,
    };
    McpServerConfig::Sdk {
        name,
        instance: Arc::new(server),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn greet_server() -> McpServerConfig {
        let greet = tool(
            "greet",
            "Greet a user",
            json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
            |args| async move {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
                Ok(json!({"content": [{"type": "text", "text": format!("Hello, {name}!")}]}))
            },
        );
        create_sdk_mcp_server("greeter", "1.0.0", vec![greet])
    }

    fn instance(config: &McpServerConfig) -> Arc<dyn McpServerInstance> {
        match config {
            McpServerConfig::Sdk { instance, .. } => instance.clone(),
            _ => panic!("expected sdk config"),
        }
    }

    #[tokio::test]
    async fn initialize_and_list_tools() {
        let config = greet_server();
        let server = instance(&config);
        let init = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "greeter");

        let list = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await;
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "greet");
    }

    #[tokio::test]
    async fn call_tool_runs_handler() {
        let config = greet_server();
        let server = instance(&config);
        let resp = server
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "greet", "arguments": {"name": "Ada"}},
            }))
            .await;
        assert_eq!(resp["result"]["content"][0]["text"], "Hello, Ada!");
    }

    #[tokio::test]
    async fn call_unknown_tool_errors() {
        let config = greet_server();
        let server = instance(&config);
        let resp = server
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "nope", "arguments": {}},
            }))
            .await;
        assert_eq!(resp["error"]["code"], -32603);
    }

    #[tokio::test]
    async fn unknown_method_errors() {
        let config = greet_server();
        let server = instance(&config);
        let resp = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list"}))
            .await;
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tool_is_error_result_and_handler_error() {
        // A handler that returns an is_error result -> isError in the response.
        let err_result = tool(
            "boom",
            "always errors via result",
            json!({"type": "object"}),
            |_| async move { Ok(json!({"content": [], "is_error": true})) },
        );
        // A handler that returns Err -> jsonrpc error.
        let handler_err = tool(
            "throw",
            "returns Err",
            json!({"type": "object"}),
            |_| async move { Err(crate::Error::connection("nope")) },
        );
        let config = create_sdk_mcp_server("s", "1.0.0", vec![err_result, handler_err]);
        let server = instance(&config);

        let r1 = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "boom"}}))
            .await;
        assert_eq!(r1["result"]["isError"], json!(true));

        let r2 = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "throw"}}))
            .await;
        assert_eq!(r2["error"]["code"], -32603);
        assert!(r2["error"]["message"].as_str().unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn notifications_initialized_is_acked() {
        let config = greet_server();
        let server = instance(&config);
        let resp = server
            .handle_message(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn tool_list_entry_includes_annotations_and_meta() {
        let mut t = tool("annotated", "has annotations", json!({"type": "object"}), |_| async move {
            Ok(json!({"content": []}))
        });
        t.annotations = Some(ToolAnnotations {
            title: Some("Annotated".into()),
            read_only_hint: Some(true),
            max_result_size_chars: Some(4096),
            ..Default::default()
        });
        let config = create_sdk_mcp_server("s", "1.0.0", vec![t]);
        let server = instance(&config);
        let list = server
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await;
        let entry = &list["result"]["tools"][0];
        assert_eq!(entry["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(entry["_meta"]["anthropic/maxResultSizeChars"], json!(4096));
    }

    #[test]
    fn debug_impls_render() {
        let greet = tool("greet", "d", json!({"type": "object"}), |_| async move { Ok(json!({})) });
        assert!(format!("{greet:?}").contains("greet"));
        let config = create_sdk_mcp_server("dbg", "1.0.0", vec![greet]);
        if let McpServerConfig::Sdk { instance, .. } = &config {
            assert!(format!("{instance:?}").contains("dbg"));
        }
    }
}
