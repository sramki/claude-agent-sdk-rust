//! Runtime integration tests driven by a scripted mock [`Transport`], so the
//! full `query()` / `Client` control-protocol path is exercised without the
//! real `claude` CLI.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use claude_agent_sdk::runtime::transport::MessageStream;
use claude_agent_sdk::types::{
    ContentBlock, HookEvent, HookJSONOutput, HookMatcher, McpServers, Message, PermissionMode,
    PermissionResult, PermissionResultAllow, SyncHookOutput, UserContent,
};
use claude_agent_sdk::{
    create_sdk_mcp_server, query_with_transport, tool, ClaudeAgentOptions, Client, Prompt, Result,
    Transport,
};

/// A scripted transport: answers the `initialize` control request and, for each
/// user message, emits a canned assistant + result pair, then ends the stream.
struct MockTransport {
    tx: mpsc::Sender<Result<Value>>,
    rx: Option<mpsc::Receiver<Result<Value>>>,
    written: Arc<Mutex<Vec<Value>>>,
    ready: bool,
}

impl MockTransport {
    fn new() -> (Self, Arc<Mutex<Vec<Value>>>) {
        let (tx, rx) = mpsc::channel(64);
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            MockTransport {
                tx,
                rx: Some(rx),
                written: written.clone(),
                ready: false,
            },
            written,
        )
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn connect(&mut self) -> Result<()> {
        self.ready = true;
        Ok(())
    }

    async fn write(&mut self, data: &str) -> Result<()> {
        let v: Value = serde_json::from_str(data.trim()).expect("valid json line");
        self.written.lock().unwrap().push(v.clone());
        match v.get("type").and_then(Value::as_str) {
            Some("control_request") => {
                let rid = v.get("request_id").and_then(Value::as_str).unwrap_or("");
                let resp = json!({
                    "type": "control_response",
                    "response": {"subtype": "success", "request_id": rid, "response": {"commands": []}},
                });
                let _ = self.tx.send(Ok(resp)).await;
            }
            Some("user") => {
                let assistant = json!({
                    "type": "assistant",
                    "message": {"model": "claude-mock", "content": [{"type": "text", "text": "Hi there!"}]},
                    "session_id": "s1",
                });
                let result = json!({
                    "type": "result", "subtype": "success", "duration_ms": 1, "duration_api_ms": 1,
                    "is_error": false, "num_turns": 1, "session_id": "s1", "result": "Hi there!",
                });
                let _ = self.tx.send(Ok(assistant)).await;
                let _ = self.tx.send(Ok(result)).await;
                // End the transcript stream so the query completes.
                self.tx = mpsc::channel(1).0;
            }
            _ => {}
        }
        Ok(())
    }

    async fn end_input(&mut self) -> Result<()> {
        Ok(())
    }

    fn read_messages(&mut self) -> MessageStream {
        match self.rx.take() {
            Some(rx) => Box::pin(ReceiverStream::new(rx)),
            None => Box::pin(tokio_stream::empty()),
        }
    }

    async fn close(&mut self) -> Result<()> {
        self.ready = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

#[tokio::test]
async fn query_string_prompt_yields_assistant_and_result() {
    let (transport, written) = MockTransport::new();
    let mut stream = query_with_transport(
        "hello",
        ClaudeAgentOptions::default(),
        Box::new(transport),
    )
    .await
    .expect("query starts");

    let mut messages = Vec::new();
    while let Some(item) = stream.next().await {
        messages.push(item.expect("no error"));
    }

    // Assistant text then a result.
    assert_eq!(messages.len(), 2);
    match &messages[0] {
        Message::Assistant(a) => {
            assert_eq!(a.model, "claude-mock");
            assert!(matches!(a.content[0], ContentBlock::Text(_)));
        }
        other => panic!("expected assistant, got {other:?}"),
    }
    assert!(matches!(&messages[1], Message::Result(_)));

    // The control-protocol handshake happened: an initialize request and the
    // user message were both written.
    let w = written.lock().unwrap();
    assert!(w.iter().any(|m| m["request"]["subtype"] == "initialize"));
    assert!(w.iter().any(|m| m["type"] == "user"));
}

#[tokio::test]
async fn query_message_sequence_prompt() {
    let (transport, _written) = MockTransport::new();
    let prompt = Prompt::Messages(vec![json!({
        "type": "user",
        "message": {"role": "user", "content": "hi"},
    })]);
    let mut stream =
        query_with_transport(prompt, ClaudeAgentOptions::default(), Box::new(transport))
            .await
            .expect("query starts");

    let mut saw_result = false;
    while let Some(item) = stream.next().await {
        if let Message::Result(_) = item.expect("no error") {
            saw_result = true;
        }
    }
    assert!(saw_result);
}

#[tokio::test]
async fn client_connect_query_and_receive() {
    let (transport, _written) = MockTransport::new();
    let mut client = Client::with_transport(ClaudeAgentOptions::default(), Box::new(transport));
    client.connect(None).await.expect("connect");

    let mut stream = client.messages();
    client.query("hello", "default").await.expect("query write");

    let mut got_user_echo = false;
    let mut got_result = false;
    while let Some(item) = stream.next().await {
        match item.expect("no error") {
            Message::Assistant(a) => {
                assert_eq!(a.model, "claude-mock");
            }
            Message::User(u) => {
                // Not expected from the mock, but assert shape if it appears.
                if let UserContent::Text(_) = u.content {
                    got_user_echo = true;
                }
            }
            Message::Result(_) => {
                got_result = true;
                break;
            }
            _ => {}
        }
    }
    let _ = got_user_echo;
    assert!(got_result);
    client.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Control-protocol tests: a richer mock that auto-answers SDK-initiated control
// requests and lets the test inject inbound control requests (can_use_tool,
// hook_callback, mcp_message) to exercise callback dispatch.
// ---------------------------------------------------------------------------

struct ControlMock {
    tx: mpsc::Sender<Result<Value>>,
    rx: Option<mpsc::Receiver<Result<Value>>>,
    written: Arc<Mutex<Vec<Value>>>,
    ready: bool,
}

impl ControlMock {
    /// Returns (transport, written-log, inject-sender).
    #[allow(clippy::type_complexity)]
    fn new() -> (Self, Arc<Mutex<Vec<Value>>>, mpsc::Sender<Result<Value>>) {
        let (tx, rx) = mpsc::channel(64);
        let written = Arc::new(Mutex::new(Vec::new()));
        let inject = tx.clone();
        (
            ControlMock {
                tx,
                rx: Some(rx),
                written: written.clone(),
                ready: false,
            },
            written,
            inject,
        )
    }
}

#[async_trait]
impl Transport for ControlMock {
    async fn connect(&mut self) -> Result<()> {
        self.ready = true;
        Ok(())
    }

    async fn write(&mut self, data: &str) -> Result<()> {
        let v: Value = serde_json::from_str(data.trim()).expect("valid json");
        self.written.lock().unwrap().push(v.clone());
        // Auto-answer SDK-initiated control requests (initialize + control ops).
        if v.get("type").and_then(Value::as_str) == Some("control_request") {
            let rid = v.get("request_id").and_then(Value::as_str).unwrap_or("");
            let resp = json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": rid, "response": {"commands": []}},
            });
            let _ = self.tx.send(Ok(resp)).await;
        }
        Ok(())
    }

    async fn end_input(&mut self) -> Result<()> {
        Ok(())
    }

    fn read_messages(&mut self) -> MessageStream {
        match self.rx.take() {
            Some(rx) => Box::pin(ReceiverStream::new(rx)),
            None => Box::pin(tokio_stream::empty()),
        }
    }

    async fn close(&mut self) -> Result<()> {
        self.ready = false;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

/// Polls the write log until `pred` matches or times out.
async fn wait_written(
    written: &Arc<Mutex<Vec<Value>>>,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..400 {
        let found = {
            let w = written.lock().unwrap();
            w.iter().find(|v| pred(v)).cloned()
        };
        if let Some(v) = found {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for a matching written frame");
}

fn is_response_for<'a>(v: &'a Value, rid: &str) -> Option<&'a Value> {
    let resp = v.get("response")?;
    if v.get("type")?.as_str()? == "control_response" && resp.get("request_id")?.as_str()? == rid {
        Some(resp)
    } else {
        None
    }
}

#[tokio::test]
async fn can_use_tool_callback_dispatch() {
    let (transport, written, inject) = ControlMock::new();
    let can_use_tool: claude_agent_sdk::types::CanUseTool = Arc::new(|_name, _input, _ctx| {
        Box::pin(async move {
            Ok(PermissionResult::Allow(PermissionResultAllow {
                updated_input: Some(json!({"command": "ls -la"}).as_object().unwrap().clone()),
                updated_permissions: None,
            }))
        })
    });
    let options = ClaudeAgentOptions {
        can_use_tool: Some(can_use_tool),
        ..Default::default()
    };

    let mut client = Client::with_transport(options, Box::new(transport));
    client.connect(None).await.expect("connect");

    inject
        .send(Ok(json!({
            "type": "control_request", "request_id": "perm1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                        "input": {"command": "ls"}, "tool_use_id": "tu1"},
        })))
        .await
        .unwrap();

    let resp = wait_written(&written, |v| is_response_for(v, "perm1").is_some()).await;
    let inner = is_response_for(&resp, "perm1").unwrap();
    assert_eq!(inner["subtype"], "success");
    assert_eq!(inner["response"]["behavior"], "allow");
    assert_eq!(inner["response"]["updatedInput"]["command"], "ls -la");
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn hook_callback_dispatch() {
    let (transport, written, inject) = ControlMock::new();
    let cb: claude_agent_sdk::types::HookCallback = Arc::new(|_input, _tuid, _ctx| {
        Box::pin(async move {
            Ok(HookJSONOutput::Sync(SyncHookOutput {
                continue_: Some(true),
                ..Default::default()
            }))
        })
    });
    let mut hooks = HashMap::new();
    hooks.insert(HookEvent::PreToolUse, vec![HookMatcher::new(None, vec![cb])]);
    let options = ClaudeAgentOptions {
        hooks: Some(hooks),
        ..Default::default()
    };

    let mut client = Client::with_transport(options, Box::new(transport));
    client.connect(None).await.expect("connect");

    // First callback id is deterministically "hook_0".
    inject
        .send(Ok(json!({
            "type": "control_request", "request_id": "hook1",
            "request": {"subtype": "hook_callback", "callback_id": "hook_0", "tool_use_id": "x",
                        "input": {"hook_event_name": "PreToolUse", "session_id": "s",
                                  "transcript_path": "/t", "cwd": "/c", "tool_name": "Bash",
                                  "tool_input": {}, "tool_use_id": "x"}},
        })))
        .await
        .unwrap();

    let resp = wait_written(&written, |v| is_response_for(v, "hook1").is_some()).await;
    let inner = is_response_for(&resp, "hook1").unwrap();
    assert_eq!(inner["subtype"], "success");
    assert_eq!(inner["response"]["continue"], true);
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn mcp_message_dispatch_routes_to_sdk_server() {
    let (transport, written, inject) = ControlMock::new();
    let greet = tool(
        "greet",
        "Greet",
        json!({"type": "object", "properties": {}}),
        |_args| async move { Ok(json!({"content": [{"type": "text", "text": "hi"}]})) },
    );
    let mut servers = HashMap::new();
    servers.insert("calc".to_string(), create_sdk_mcp_server("calc", "1.0.0", vec![greet]));
    let options = ClaudeAgentOptions {
        mcp_servers: McpServers::Map(servers),
        ..Default::default()
    };

    let mut client = Client::with_transport(options, Box::new(transport));
    client.connect(None).await.expect("connect");

    inject
        .send(Ok(json!({
            "type": "control_request", "request_id": "mcp1",
            "request": {"subtype": "mcp_message", "server_name": "calc",
                        "message": {"jsonrpc": "2.0", "id": 7, "method": "tools/list"}},
        })))
        .await
        .unwrap();

    let resp = wait_written(&written, |v| is_response_for(v, "mcp1").is_some()).await;
    let inner = is_response_for(&resp, "mcp1").unwrap();
    let tools = &inner["response"]["mcp_response"]["result"]["tools"];
    assert_eq!(tools[0]["name"], "greet");
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn control_methods_send_and_resolve() {
    let (transport, written, _inject) = ControlMock::new();
    let mut client = Client::with_transport(ClaudeAgentOptions::default(), Box::new(transport));
    client.connect(None).await.expect("connect");

    client.interrupt().await.expect("interrupt ok");
    client
        .set_permission_mode(PermissionMode::AcceptEdits)
        .await
        .expect("set mode ok");
    client.stop_task("task-1").await.expect("stop_task ok");

    {
        let w = written.lock().unwrap();
        assert!(w.iter().any(|m| m["request"]["subtype"] == "interrupt"));
        assert!(w.iter().any(|m| m["request"]["subtype"] == "set_permission_mode"));
        assert!(w.iter().any(|m| m["request"]["subtype"] == "stop_task"));
    }
    client.disconnect().await.unwrap();
}
