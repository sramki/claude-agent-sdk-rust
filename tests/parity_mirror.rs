//! Parity tests for the transcript-mirror batcher wired into the runtime,
//! ported from upstream `test_transcript_mirror.py`. Drives `Query` directly
//! with a mock transport so the mirror `filePath` keying is deterministic.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use claude_agent_sdk_rs::runtime::query::{Query, QueryConfig};
use claude_agent_sdk_rs::runtime::transport::MessageStream;
use claude_agent_sdk_rs::types::{
    SessionKey, SessionStore, SessionStoreEntry, SessionStoreFlushMode,
};
use claude_agent_sdk_rs::{InMemorySessionStore, Result, Transport};

const PROJECTS: &str = "/projects";
const PKEY: &str = "-workspace-proj";
const SID: &str = "11111111-1111-4111-8111-111111111111";

/// A mock transport that answers `initialize` and lets the test inject frames.
struct MirrorMock {
    tx: mpsc::Sender<Result<Value>>,
    rx: Option<mpsc::Receiver<Result<Value>>>,
    ready: bool,
}

impl MirrorMock {
    fn new() -> (Self, mpsc::Sender<Result<Value>>) {
        let (tx, rx) = mpsc::channel(64);
        let inject = tx.clone();
        (
            MirrorMock {
                tx,
                rx: Some(rx),
                ready: false,
            },
            inject,
        )
    }
}

#[async_trait]
impl Transport for MirrorMock {
    async fn connect(&mut self) -> Result<()> {
        self.ready = true;
        Ok(())
    }
    async fn write(&mut self, data: &str) -> Result<()> {
        let v: Value = serde_json::from_str(data.trim()).expect("json");
        if v.get("type").and_then(Value::as_str) == Some("control_request") {
            let rid = v.get("request_id").and_then(Value::as_str).unwrap_or("");
            let _ = self
                .tx
                .send(Ok(json!({
                    "type": "control_response",
                    "response": {"subtype": "success", "request_id": rid, "response": {}},
                })))
                .await;
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

fn config(store: Arc<dyn SessionStore>, flush: SessionStoreFlushMode) -> QueryConfig {
    QueryConfig {
        session_store: Some(store),
        session_store_flush: flush,
        mirror_projects_dir: PROJECTS.to_string(),
        ..Default::default()
    }
}

fn user_entry(uid: &str) -> Value {
    json!({"type": "user", "uuid": uid, "parentUuid": null, "sessionId": SID, "message": {"content": "hi"}})
}

fn mirror_frame(entries: Vec<Value>) -> Value {
    json!({
        "type": "transcript_mirror",
        "filePath": format!("{PROJECTS}/{PKEY}/{SID}.jsonl"),
        "entries": entries,
    })
}

fn result_frame() -> Value {
    json!({
        "type": "result", "subtype": "success", "duration_ms": 1, "duration_api_ms": 1,
        "is_error": false, "num_turns": 1, "session_id": SID,
    })
}

async fn drive_to_result(query: &mut Query) -> Vec<Value> {
    let mut seen = Vec::new();
    while let Some(item) = query.next_message().await {
        let v = item.expect("no error");
        let is_result = v.get("type").and_then(Value::as_str) == Some("result");
        seen.push(v);
        if is_result {
            break;
        }
    }
    seen
}

#[tokio::test]
async fn mirror_frames_are_flushed_to_store_on_result() {
    let store = Arc::new(InMemorySessionStore::new());
    let (transport, inject) = MirrorMock::new();
    let mut query = Query::new(Box::new(transport), config(store.clone(), SessionStoreFlushMode::Batched));
    query.start();
    query.initialize().await.unwrap();

    inject.send(Ok(mirror_frame(vec![user_entry("u1"), user_entry("u2")]))).await.unwrap();
    inject.send(Ok(result_frame())).await.unwrap();

    let seen = drive_to_result(&mut query).await;
    // The mirror frame is NOT forwarded to the consumer; the result is.
    assert!(seen.iter().all(|v| v["type"] != "transcript_mirror"));
    assert!(seen.iter().any(|v| v["type"] == "result"));

    // Entries were flushed to the store, keyed by project/session.
    let key = SessionKey {
        project_key: PKEY.to_string(),
        session_id: SID.to_string(),
        subpath: None,
    };
    let entries = store.get_entries(&key);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["uuid"], "u1");

    query.close().await.unwrap();
}

/// A store whose append always fails, to exercise the retry + mirror_error path.
#[derive(Default)]
struct FailStore;

#[async_trait]
impl SessionStore for FailStore {
    async fn append(&self, _k: &SessionKey, _e: &[SessionStoreEntry]) -> Result<()> {
        Err(claude_agent_sdk_rs::Error::connection("boom"))
    }
    async fn load(&self, _k: &SessionKey) -> Result<Option<Vec<SessionStoreEntry>>> {
        Ok(None)
    }
}

#[tokio::test]
async fn failed_mirror_append_surfaces_mirror_error() {
    let store: Arc<dyn SessionStore> = Arc::new(FailStore);
    let (transport, inject) = MirrorMock::new();
    let mut query = Query::new(Box::new(transport), config(store, SessionStoreFlushMode::Batched));
    query.start();
    query.initialize().await.unwrap();

    inject.send(Ok(mirror_frame(vec![user_entry("u1")]))).await.unwrap();
    inject.send(Ok(result_frame())).await.unwrap();

    let seen = drive_to_result(&mut query).await;
    // A mirror_error system message is surfaced (after retries are exhausted).
    assert!(
        seen.iter().any(|v| v["type"] == "system"
            && v["subtype"] == "mirror_error"
            && v["error"].as_str().unwrap().contains("boom")),
        "expected a mirror_error system message, got: {seen:?}"
    );

    query.close().await.unwrap();
}
