//! Bidirectional control protocol on top of a [`Transport`].
//!
//! Faithful port of `_internal/query.py`. A background read loop consumes the
//! transport's message stream and routes control responses (to pending
//! requests), inbound control requests (to the permission / hook / MCP
//! callbacks), and SDK messages (to the consumer channel). Control requests are
//! sent with a per-request oneshot awaiting the matching response.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use super::transport::Transport;
use crate::error::{Error, Result};
use crate::types::{
    HookCallback, HookContext, HookEvent, HookInput, HookMatcher, McpServerInstance, PermissionMode,
    PermissionResult, PermissionUpdate, ToolPermissionContext,
};

const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);

/// Configuration derived from options and passed into [`Query::new`].
pub struct QueryConfig {
    /// Whether streaming (bidirectional) mode is active.
    pub is_streaming_mode: bool,
    /// The tool-permission callback, if any.
    pub can_use_tool: Option<crate::types::CanUseTool>,
    /// Hook matchers by event.
    pub hooks: HashMap<HookEvent, Vec<HookMatcher>>,
    /// In-process SDK MCP servers by name.
    pub sdk_mcp_servers: HashMap<String, Arc<dyn McpServerInstance>>,
    /// Agent definitions to send via `initialize` (wire dicts).
    pub agents: Option<Map<String, Value>>,
    /// `excludeDynamicSections` preset flag.
    pub exclude_dynamic_sections: Option<bool>,
    /// Explicit skill allowlist to send via `initialize` (only sent when a list).
    pub skills: Option<Vec<String>>,
    /// Timeout for the `initialize` request.
    pub initialize_timeout: Duration,
}

impl Default for QueryConfig {
    fn default() -> Self {
        QueryConfig {
            is_streaming_mode: true,
            can_use_tool: None,
            hooks: HashMap::new(),
            sdk_mcp_servers: HashMap::new(),
            agents: None,
            exclude_dynamic_sections: None,
            skills: None,
            initialize_timeout: DEFAULT_INITIALIZE_TIMEOUT,
        }
    }
}

/// Shared state accessed by the read loop, control-request handlers, and the
/// public control methods.
struct Shared {
    transport: Mutex<Box<dyn Transport>>,
    is_streaming_mode: bool,
    can_use_tool: Option<crate::types::CanUseTool>,
    hook_callbacks: HashMap<String, HookCallback>,
    sdk_mcp_servers: HashMap<String, Arc<dyn McpServerInstance>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>,
    request_counter: AtomicU64,
    first_result: Notify,
    first_result_fired: AtomicBool,
    has_hooks: bool,
}

impl Shared {
    async fn write(&self, data: &str) -> Result<()> {
        let mut t = self.transport.lock().await;
        t.write(data).await
    }

    fn fire_first_result(&self) {
        if !self.first_result_fired.swap(true, Ordering::SeqCst) {
            self.first_result.notify_waiters();
        }
    }
}

/// Handles the bidirectional control protocol over a [`Transport`].
pub struct Query {
    shared: Arc<Shared>,
    hooks_config: Option<Value>,
    agents: Option<Map<String, Value>>,
    exclude_dynamic_sections: Option<bool>,
    skills: Option<Vec<String>>,
    initialize_timeout: Duration,
    read_task: Option<JoinHandle<()>>,
    msg_rx: Option<mpsc::Receiver<Result<Value>>>,
    started: bool,
    closed: bool,
}

impl Query {
    /// Builds a `Query`. Assigns hook callback ids and precomputes the wire
    /// hooks config up front (before the read loop starts), so inbound
    /// `hook_callback` requests always resolve.
    pub fn new(transport: Box<dyn Transport>, config: QueryConfig) -> Self {
        let mut hook_callbacks: HashMap<String, HookCallback> = HashMap::new();
        let mut next_id = 0u64;
        let mut hooks_config = Map::new();
        for (event, matchers) in &config.hooks {
            if matchers.is_empty() {
                continue;
            }
            let event_name = serde_json::to_value(event)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let mut event_matchers = Vec::new();
            for matcher in matchers {
                let mut callback_ids = Vec::new();
                for cb in &matcher.hooks {
                    let id = format!("hook_{next_id}");
                    next_id += 1;
                    hook_callbacks.insert(id.clone(), cb.clone());
                    callback_ids.push(Value::String(id));
                }
                let mut m = Map::new();
                m.insert(
                    "matcher".into(),
                    matcher.matcher.clone().map(Value::String).unwrap_or(Value::Null),
                );
                m.insert("hookCallbackIds".into(), Value::Array(callback_ids));
                if let Some(t) = matcher.timeout {
                    m.insert("timeout".into(), json!(t));
                }
                event_matchers.push(Value::Object(m));
            }
            hooks_config.insert(event_name, Value::Array(event_matchers));
        }

        let has_hooks = !hooks_config.is_empty();
        let hooks_config = if has_hooks {
            Some(Value::Object(hooks_config))
        } else {
            None
        };

        let shared = Arc::new(Shared {
            transport: Mutex::new(transport),
            is_streaming_mode: config.is_streaming_mode,
            can_use_tool: config.can_use_tool,
            hook_callbacks,
            has_hooks: !config.hooks.is_empty(),
            sdk_mcp_servers: config.sdk_mcp_servers,
            pending: Mutex::new(HashMap::new()),
            request_counter: AtomicU64::new(0),
            first_result: Notify::new(),
            first_result_fired: AtomicBool::new(false),
        });

        Query {
            shared,
            hooks_config,
            agents: config.agents,
            exclude_dynamic_sections: config.exclude_dynamic_sections,
            skills: config.skills,
            initialize_timeout: config.initialize_timeout,
            read_task: None,
            msg_rx: None,
            started: false,
            closed: false,
        }
    }

    /// Starts the background read loop (idempotent).
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let stream = {
            // The transport lock is uncontended here (read loop not spawned yet).
            let mut t = self.shared.transport.try_lock().expect("transport not locked");
            t.read_messages()
        };
        let (out_tx, out_rx) = mpsc::channel::<Result<Value>>(100);
        let shared = self.shared.clone();
        self.msg_rx = Some(out_rx);
        self.read_task = Some(tokio::spawn(read_loop(shared, stream, out_tx)));
    }

    /// Runs the initialize handshake (streaming mode only). Returns the
    /// initialize response, or `None` when not in streaming mode.
    pub async fn initialize(&mut self) -> Result<Option<Value>> {
        if !self.shared.is_streaming_mode {
            return Ok(None);
        }
        let mut request = Map::new();
        request.insert("subtype".into(), json!("initialize"));
        request.insert(
            "hooks".into(),
            self.hooks_config.clone().unwrap_or(Value::Null),
        );
        if let Some(agents) = &self.agents {
            request.insert("agents".into(), Value::Object(agents.clone()));
        }
        if let Some(eds) = self.exclude_dynamic_sections {
            request.insert("excludeDynamicSections".into(), json!(eds));
        }
        if let Some(skills) = &self.skills {
            request.insert("skills".into(), json!(skills));
        }
        let response = self
            .send_control_request(Value::Object(request), self.initialize_timeout)
            .await?;
        Ok(Some(response))
    }

    fn next_request_id(&self) -> String {
        let n = self.shared.request_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("req_{n}_{suffix:08x}")
    }

    /// Sends a control request and awaits its response (the inner `response`
    /// dict). Faithful port of `_send_control_request`.
    async fn send_control_request(&self, request: Value, timeout: Duration) -> Result<Value> {
        if !self.shared.is_streaming_mode {
            return Err(Error::connection("Control requests require streaming mode"));
        }
        let request_id = self.next_request_id();
        let (tx, rx) = oneshot::channel::<Result<Value>>();
        self.shared
            .pending
            .lock()
            .await
            .insert(request_id.clone(), tx);

        let control_request = json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        });
        let subtype = request
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if let Err(e) = self.shared.write(&(control_request.to_string() + "\n")).await {
            self.shared.pending.lock().await.remove(&request_id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => {
                // The stored value is the inner control-response dict; return
                // its "response" payload.
                result.map(|resp| {
                    resp.get("response")
                        .cloned()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| Value::Object(Map::new()))
                })
            }
            Ok(Err(_recv)) => {
                self.shared.pending.lock().await.remove(&request_id);
                Err(Error::connection("control response channel closed"))
            }
            Err(_elapsed) => {
                self.shared.pending.lock().await.remove(&request_id);
                Err(Error::connection(format!("Control request timeout: {subtype}")))
            }
        }
    }

    /// Streams input messages, then waits for the first result (if callbacks
    /// require it) and closes stdin. Faithful port of `stream_input`.
    pub async fn stream_input(&self, messages: Vec<Value>) -> Result<()> {
        for message in messages {
            if self.closed {
                break;
            }
            self.shared.write(&(message.to_string() + "\n")).await?;
        }
        self.wait_for_result_and_end_input().await
    }

    /// Waits for the first result (when SDK MCP servers or hooks are present)
    /// then closes stdin. Faithful port of `wait_for_result_and_end_input`.
    pub async fn wait_for_result_and_end_input(&self) -> Result<()> {
        let needs_wait = !self.shared.sdk_mcp_servers.is_empty() || self.shared.has_hooks;
        if needs_wait && !self.shared.first_result_fired.load(Ordering::SeqCst) {
            self.shared.first_result.notified().await;
        }
        let mut t = self.shared.transport.lock().await;
        t.end_input().await
    }

    /// Receives the next SDK message. `Ok(None)` marks end of stream; `Err`
    /// surfaces a fatal error. Faithful to `receive_messages`.
    pub async fn next_message(&mut self) -> Option<Result<Value>> {
        let rx = self.msg_rx.as_mut()?;
        rx.recv().await
    }

    // --- Control methods -------------------------------------------------

    /// Sends an interrupt control request.
    pub async fn interrupt(&self) -> Result<()> {
        self.send_control_request(json!({"subtype": "interrupt"}), DEFAULT_CONTROL_TIMEOUT)
            .await
            .map(|_| ())
    }

    /// Changes the permission mode.
    pub async fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        self.send_control_request(
            json!({"subtype": "set_permission_mode", "mode": mode}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Changes the model.
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.send_control_request(
            json!({"subtype": "set_model", "model": model}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Rewinds tracked files to a user message.
    pub async fn rewind_files(&self, user_message_id: &str) -> Result<()> {
        self.send_control_request(
            json!({"subtype": "rewind_files", "user_message_id": user_message_id}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Reconnects an MCP server.
    pub async fn reconnect_mcp_server(&self, server_name: &str) -> Result<()> {
        self.send_control_request(
            json!({"subtype": "mcp_reconnect", "serverName": server_name}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Enables or disables an MCP server.
    pub async fn toggle_mcp_server(&self, server_name: &str, enabled: bool) -> Result<()> {
        self.send_control_request(
            json!({"subtype": "mcp_toggle", "serverName": server_name, "enabled": enabled}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Stops a running task.
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.send_control_request(
            json!({"subtype": "stop_task", "task_id": task_id}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
        .map(|_| ())
    }

    /// Gets the current MCP server status (raw wire dict).
    pub async fn get_mcp_status(&self) -> Result<Value> {
        self.send_control_request(json!({"subtype": "mcp_status"}), DEFAULT_CONTROL_TIMEOUT)
            .await
    }

    /// Gets the current context-usage breakdown (raw wire dict).
    pub async fn get_context_usage(&self) -> Result<Value> {
        self.send_control_request(
            json!({"subtype": "get_context_usage"}),
            DEFAULT_CONTROL_TIMEOUT,
        )
        .await
    }

    /// Closes the query and its transport.
    pub async fn close(&mut self) -> Result<()> {
        self.closed = true;
        if let Some(handle) = self.read_task.take() {
            handle.abort();
            let _ = handle.await;
        }
        let mut t = self.shared.transport.lock().await;
        t.close().await
    }
}

// ---------------------------------------------------------------------------
// Read loop + control-request handling
// ---------------------------------------------------------------------------

async fn read_loop(
    shared: Arc<Shared>,
    mut stream: super::transport::MessageStream,
    out: mpsc::Sender<Result<Value>>,
) {
    let mut last_error_result_text: Option<String> = None;

    while let Some(item) = stream.next().await {
        let message = match item {
            Ok(m) => m,
            Err(e) => {
                fail_pending(&shared, &e).await;
                let err = maybe_replace_process_error(e, &last_error_result_text);
                let _ = out.send(Err(err)).await;
                shared.fire_first_result();
                return;
            }
        };

        let msg_type = message.get("type").and_then(Value::as_str);
        match msg_type {
            Some("control_response") => {
                let response = message.get("response").cloned().unwrap_or(Value::Null);
                if let Some(request_id) = response.get("request_id").and_then(Value::as_str) {
                    if let Some(tx) = shared.pending.lock().await.remove(request_id) {
                        let result = if response.get("subtype").and_then(Value::as_str)
                            == Some("error")
                        {
                            Err(Error::connection(
                                response
                                    .get("error")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Unknown error")
                                    .to_string(),
                            ))
                        } else {
                            Ok(response.clone())
                        };
                        let _ = tx.send(result);
                    }
                }
            }
            Some("control_request") => {
                let shared2 = shared.clone();
                tokio::spawn(handle_control_request(shared2, message));
            }
            Some("control_cancel_request") => {
                // Cooperative cancellation of in-flight handlers is a no-op here;
                // handlers are cheap and the CLI abandons the request anyway.
            }
            Some("transcript_mirror") => {
                // SessionStore mirroring is handled at a higher layer; drop here.
            }
            Some("result") => {
                shared.fire_first_result();
                if message.get("is_error").and_then(Value::as_bool) == Some(true) {
                    let errors: Vec<String> = message
                        .get("errors")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    last_error_result_text = Some(if errors.is_empty() {
                        message
                            .get("subtype")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string()
                    } else {
                        errors.join("; ")
                    });
                } else {
                    last_error_result_text = None;
                }
                if out.send(Ok(message)).await.is_err() {
                    return;
                }
            }
            other => {
                let is_session_state_changed = other == Some("system")
                    && message.get("subtype").and_then(Value::as_str)
                        == Some("session_state_changed");
                if !is_session_state_changed {
                    last_error_result_text = None;
                }
                if out.send(Ok(message)).await.is_err() {
                    return;
                }
            }
        }
    }

    // Stream ended (clean EOF). Unblock waiters; dropping `out` signals end.
    shared.fire_first_result();
    fail_pending(&shared, &Error::connection("transport closed")).await;
}

async fn fail_pending(shared: &Arc<Shared>, error: &Error) {
    let mut pending = shared.pending.lock().await;
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(Error::connection(error.to_string())));
    }
}

fn maybe_replace_process_error(error: Error, last_error_result_text: &Option<String>) -> Error {
    if let (Error::Process { .. }, Some(text)) = (&error, last_error_result_text) {
        Error::connection(format!("Claude Code returned an error result: {text}"))
    } else {
        error
    }
}

async fn handle_control_request(shared: Arc<Shared>, request: Value) {
    let request_id = request
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let request_data = request.get("request").cloned().unwrap_or(Value::Null);
    let subtype = request_data
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or("");

    let result: Result<Value> = match subtype {
        "can_use_tool" => handle_can_use_tool(&shared, &request_data).await,
        "hook_callback" => handle_hook_callback(&shared, &request_data).await,
        "mcp_message" => handle_mcp_message(&shared, &request_data).await,
        other => Err(Error::connection(format!(
            "Unsupported control request subtype: {other}"
        ))),
    };

    let response = match result {
        Ok(data) => json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": data},
        }),
        Err(e) => json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": request_id, "error": e.to_string()},
        }),
    };
    let _ = shared.write(&(response.to_string() + "\n")).await;
}

async fn handle_can_use_tool(shared: &Arc<Shared>, request_data: &Value) -> Result<Value> {
    let can_use_tool = shared
        .can_use_tool
        .as_ref()
        .ok_or_else(|| Error::connection("canUseTool callback is not provided"))?;

    let original_input = request_data
        .get("input")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tool_name = request_data
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let suggestions = request_data
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(PermissionUpdate::from_wire).collect())
        .unwrap_or_default();
    let context = ToolPermissionContext {
        suggestions,
        tool_use_id: request_data.get("tool_use_id").and_then(Value::as_str).map(str::to_string),
        agent_id: request_data.get("agent_id").and_then(Value::as_str).map(str::to_string),
        blocked_path: request_data.get("blocked_path").and_then(Value::as_str).map(str::to_string),
        decision_reason: request_data.get("decision_reason").and_then(Value::as_str).map(str::to_string),
        title: request_data.get("title").and_then(Value::as_str).map(str::to_string),
        display_name: request_data.get("display_name").and_then(Value::as_str).map(str::to_string),
        description: request_data.get("description").and_then(Value::as_str).map(str::to_string),
    };

    let result = can_use_tool(tool_name, original_input.clone(), context).await?;
    Ok(match result {
        PermissionResult::Allow(allow) => {
            let mut data = Map::new();
            data.insert("behavior".into(), json!("allow"));
            data.insert(
                "updatedInput".into(),
                Value::Object(allow.updated_input.unwrap_or(original_input)),
            );
            if let Some(perms) = allow.updated_permissions {
                data.insert(
                    "updatedPermissions".into(),
                    Value::Array(perms.iter().map(PermissionUpdate::to_wire).collect()),
                );
            }
            Value::Object(data)
        }
        PermissionResult::Deny(deny) => {
            let mut data = Map::new();
            data.insert("behavior".into(), json!("deny"));
            data.insert("message".into(), json!(deny.message));
            if deny.interrupt {
                data.insert("interrupt".into(), json!(true));
            }
            Value::Object(data)
        }
    })
}

async fn handle_hook_callback(shared: &Arc<Shared>, request_data: &Value) -> Result<Value> {
    let callback_id = request_data
        .get("callback_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::connection("hook_callback missing callback_id"))?;
    let callback = shared
        .hook_callbacks
        .get(callback_id)
        .ok_or_else(|| Error::connection(format!("No hook callback found for ID: {callback_id}")))?;

    let input_value = request_data.get("input").cloned().unwrap_or(Value::Null);
    let input: HookInput = serde_json::from_value(input_value)
        .map_err(|e| Error::message_parse(format!("Invalid hook input: {e}"), None))?;
    let tool_use_id = request_data
        .get("tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    let output = callback(input, tool_use_id, HookContext::default()).await?;
    Ok(output.to_wire())
}

async fn handle_mcp_message(shared: &Arc<Shared>, request_data: &Value) -> Result<Value> {
    let server_name = request_data
        .get("server_name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::connection("Missing server_name for MCP request"))?;
    let message = request_data
        .get("message")
        .cloned()
        .filter(Value::is_object)
        .ok_or_else(|| Error::connection("Missing message for MCP request"))?;

    let mcp_response = match shared.sdk_mcp_servers.get(server_name) {
        Some(server) => server.handle_message(message).await,
        None => json!({
            "jsonrpc": "2.0",
            "id": request_data.get("message").and_then(|m| m.get("id")).cloned().unwrap_or(Value::Null),
            "error": {"code": -32601, "message": format!("Server '{server_name}' not found")},
        }),
    };
    Ok(json!({ "mcp_response": mcp_response }))
}
