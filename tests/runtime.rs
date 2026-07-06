//! Runtime integration tests driven by a scripted mock [`Transport`], so the
//! full `query()` / `Client` control-protocol path is exercised without the
//! real `claude` CLI.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use claude_agent_sdk::runtime::transport::MessageStream;
use claude_agent_sdk::types::{ContentBlock, Message, UserContent};
use claude_agent_sdk::{
    query_with_transport, ClaudeAgentOptions, Client, Prompt, Result, Transport,
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
