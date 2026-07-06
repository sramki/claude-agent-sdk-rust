//! Parity port of upstream `tests/test_message_parser.py`
//! (`claude-agent-sdk` Python v0.2.110).
//!
//! Each `#[test]` mirrors an upstream `test_*` case (parametrized cases are
//! collapsed into loops). The Rust parser models the upstream typed
//! `SystemMessage` subclasses via [`SystemMessage::kind`] while keeping
//! `subtype`/`data` populated, so "isinstance(x, SystemMessage)" backward-compat
//! assertions become `Message::System` + `kind` checks here.
//!
//! Where the Rust error *message text* differs from Python's wording (e.g.
//! "expected object" vs "expected dict, got str") we assert the error *arm*
//! (`Error::MessageParse`) rather than the exact string, since the behavioral
//! contract is the raised-vs-returned distinction, not the prose.

use serde_json::{json, Value};

use claude_agent_sdk_rs::{
    parse_message, AssistantMessage, AssistantMessageError, ContentBlock, Error, Message,
    ResultMessage, SystemMessage, SystemMessageKind, TaskNotificationStatus, TaskUpdatedStatus,
    ToolResultContent, UserContent, UserMessage, TERMINAL_TASK_STATUSES,
};

// ---- helpers --------------------------------------------------------------

fn parse(data: &Value) -> Message {
    parse_message(data)
        .expect("expected Ok")
        .expect("expected Some(Message)")
}

fn as_user(data: &Value) -> UserMessage {
    match parse(data) {
        Message::User(u) => u,
        other => panic!("expected UserMessage, got {other:?}"),
    }
}

fn as_assistant(data: &Value) -> AssistantMessage {
    match parse(data) {
        Message::Assistant(a) => a,
        other => panic!("expected AssistantMessage, got {other:?}"),
    }
}

fn as_system(data: &Value) -> SystemMessage {
    match parse(data) {
        Message::System(s) => s,
        other => panic!("expected SystemMessage, got {other:?}"),
    }
}

fn as_result(data: &Value) -> ResultMessage {
    match parse(data) {
        Message::Result(r) => r,
        other => panic!("expected ResultMessage, got {other:?}"),
    }
}

fn blocks(u: &UserMessage) -> &[ContentBlock] {
    match &u.content {
        UserContent::Blocks(b) => b,
        UserContent::Text(_) => panic!("expected block content, got string"),
    }
}

fn terminal(status: &TaskUpdatedStatus) -> bool {
    let s = serde_json::to_value(status).unwrap();
    TERMINAL_TASK_STATUSES.contains(&s.as_str().unwrap())
}

// ---- user messages --------------------------------------------------------

#[test]
fn parse_valid_user_message() {
    let data = json!({
        "type": "user",
        "message": {"content": [{"type": "text", "text": "Hello"}]},
    });
    let u = as_user(&data);
    let bs = blocks(&u);
    assert_eq!(bs.len(), 1);
    assert!(matches!(&bs[0], ContentBlock::Text(t) if t.text == "Hello"));
}

#[test]
fn parse_user_message_with_uuid() {
    let data = json!({
        "type": "user",
        "uuid": "msg-abc123-def456",
        "message": {"content": [{"type": "text", "text": "Hello"}]},
    });
    let u = as_user(&data);
    assert_eq!(u.uuid.as_deref(), Some("msg-abc123-def456"));
    assert_eq!(blocks(&u).len(), 1);
}

#[test]
fn parse_user_message_with_tool_use() {
    let data = json!({
        "type": "user",
        "message": {"content": [
            {"type": "text", "text": "Let me read this file"},
            {"type": "tool_use", "id": "tool_456", "name": "Read",
             "input": {"file_path": "/example.txt"}},
        ]},
    });
    let u = as_user(&data);
    let bs = blocks(&u);
    assert_eq!(bs.len(), 2);
    assert!(matches!(bs[0], ContentBlock::Text(_)));
    match &bs[1] {
        ContentBlock::ToolUse(t) => {
            assert_eq!(t.id, "tool_456");
            assert_eq!(t.name, "Read");
            assert_eq!(Value::Object(t.input.clone()), json!({"file_path": "/example.txt"}));
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

#[test]
fn parse_user_message_with_tool_result() {
    let data = json!({
        "type": "user",
        "message": {"content": [
            {"type": "tool_result", "tool_use_id": "tool_789", "content": "File contents here"},
        ]},
    });
    let u = as_user(&data);
    let bs = blocks(&u);
    assert_eq!(bs.len(), 1);
    match &bs[0] {
        ContentBlock::ToolResult(t) => {
            assert_eq!(t.tool_use_id, "tool_789");
            assert_eq!(t.content, Some(ToolResultContent::Text("File contents here".into())));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn parse_user_message_with_tool_result_error() {
    let data = json!({
        "type": "user",
        "message": {"content": [
            {"type": "tool_result", "tool_use_id": "tool_error",
             "content": "File not found", "is_error": true},
        ]},
    });
    let u = as_user(&data);
    match &blocks(&u)[0] {
        ContentBlock::ToolResult(t) => {
            assert_eq!(t.tool_use_id, "tool_error");
            assert_eq!(t.content, Some(ToolResultContent::Text("File not found".into())));
            assert_eq!(t.is_error, Some(true));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn parse_user_message_with_mixed_content() {
    let data = json!({
        "type": "user",
        "message": {"content": [
            {"type": "text", "text": "Here's what I found:"},
            {"type": "tool_use", "id": "use_1", "name": "Search", "input": {"query": "test"}},
            {"type": "tool_result", "tool_use_id": "use_1", "content": "Search results"},
            {"type": "text", "text": "What do you think?"},
        ]},
    });
    let u = as_user(&data);
    let bs = blocks(&u);
    assert_eq!(bs.len(), 4);
    assert!(matches!(bs[0], ContentBlock::Text(_)));
    assert!(matches!(bs[1], ContentBlock::ToolUse(_)));
    assert!(matches!(bs[2], ContentBlock::ToolResult(_)));
    assert!(matches!(bs[3], ContentBlock::Text(_)));
}

#[test]
fn parse_user_message_inside_subagent() {
    let data = json!({
        "type": "user",
        "message": {"content": [{"type": "text", "text": "Hello"}]},
        "parent_tool_use_id": "toolu_01Xrwd5Y13sEHtzScxR77So8",
    });
    let u = as_user(&data);
    assert_eq!(u.parent_tool_use_id.as_deref(), Some("toolu_01Xrwd5Y13sEHtzScxR77So8"));
}

#[test]
fn parse_user_message_with_tool_use_result() {
    let tool_result_data = json!({
        "filePath": "/path/to/file.py",
        "oldString": "old code",
        "newString": "new code",
        "originalFile": "full file contents",
        "structuredPatch": [{
            "oldStart": 33, "oldLines": 7, "newStart": 33, "newLines": 7,
            "lines": ["   # comment", "-      old line", "+      new line"],
        }],
        "userModified": false,
        "replaceAll": false,
    });
    let data = json!({
        "type": "user",
        "message": {"role": "user", "content": [
            {"tool_use_id": "toolu_vrtx_01KXWexk3NJdwkjWzPMGQ2F1", "type": "tool_result",
             "content": "The file has been updated."},
        ]},
        "parent_tool_use_id": null,
        "session_id": "84afb479-17ae-49af-8f2b-666ac2530c3a",
        "uuid": "2ace3375-1879-48a0-a421-6bce25a9295a",
        "tool_use_result": tool_result_data,
    });
    let u = as_user(&data);
    let tur = Value::Object(u.tool_use_result.clone().unwrap());
    assert_eq!(tur, tool_result_data);
    assert_eq!(tur["filePath"], json!("/path/to/file.py"));
    assert_eq!(tur["oldString"], json!("old code"));
    assert_eq!(tur["newString"], json!("new code"));
    assert_eq!(tur["structuredPatch"][0]["oldStart"], json!(33));
    assert_eq!(u.uuid.as_deref(), Some("2ace3375-1879-48a0-a421-6bce25a9295a"));
    // parent_tool_use_id was explicit null -> None.
    assert_eq!(u.parent_tool_use_id, None);
}

#[test]
fn parse_user_message_with_string_content_and_tool_use_result() {
    let tool_result_data = json!({"filePath": "/path/to/file.py", "userModified": true});
    let data = json!({
        "type": "user",
        "message": {"content": "Simple string content"},
        "tool_use_result": tool_result_data,
    });
    let u = as_user(&data);
    assert_eq!(u.content, UserContent::Text("Simple string content".into()));
    assert_eq!(Value::Object(u.tool_use_result.clone().unwrap()), tool_result_data);
}

// ---- assistant messages ---------------------------------------------------

#[test]
fn parse_valid_assistant_message() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "text", "text": "Hello"},
            {"type": "tool_use", "id": "tool_123", "name": "Read", "input": {"file_path": "/test.txt"}},
        ], "model": "claude-opus-4-1-20250805"},
    });
    let a = as_assistant(&data);
    assert_eq!(a.content.len(), 2);
    assert!(matches!(a.content[0], ContentBlock::Text(_)));
    assert!(matches!(a.content[1], ContentBlock::ToolUse(_)));
}

#[test]
fn parse_assistant_message_with_thinking() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "thinking", "thinking": "I'm thinking about the answer...", "signature": "sig-123"},
            {"type": "text", "text": "Here's my response"},
        ], "model": "claude-opus-4-1-20250805"},
    });
    let a = as_assistant(&data);
    assert_eq!(a.content.len(), 2);
    match &a.content[0] {
        ContentBlock::Thinking(t) => {
            assert_eq!(t.thinking, "I'm thinking about the answer...");
            assert_eq!(t.signature, "sig-123");
        }
        other => panic!("expected Thinking, got {other:?}"),
    }
    assert!(matches!(&a.content[1], ContentBlock::Text(t) if t.text == "Here's my response"));
}

#[test]
fn parse_assistant_message_with_server_tool_use() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "server_tool_use", "id": "srvtoolu_01ABC", "name": "advisor", "input": {}},
        ], "model": "claude-sonnet-4-5"},
    });
    let a = as_assistant(&data);
    assert_eq!(a.content.len(), 1);
    match &a.content[0] {
        ContentBlock::ServerToolUse(s) => {
            assert_eq!(s.id, "srvtoolu_01ABC");
            // name is a typed ServerToolName enum; "advisor" round-trips to snake_case.
            assert_eq!(serde_json::to_value(s.name).unwrap(), json!("advisor"));
            assert!(s.input.is_empty());
        }
        other => panic!("expected ServerToolUse, got {other:?}"),
    }
}

#[test]
fn parse_assistant_message_with_server_tool_result() {
    // Wire tag `advisor_tool_result` maps to ContentBlock::ServerToolResult.
    let data = json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "advisor_tool_result", "tool_use_id": "srvtoolu_01ABC",
             "content": {"type": "advisor_result", "text": "Consider edge cases around empty input."}},
        ], "model": "claude-sonnet-4-5"},
    });
    let a = as_assistant(&data);
    assert_eq!(a.content.len(), 1);
    match &a.content[0] {
        ContentBlock::ServerToolResult(r) => {
            assert_eq!(r.tool_use_id, "srvtoolu_01ABC");
            assert_eq!(
                Value::Object(r.content.clone()),
                json!({"type": "advisor_result", "text": "Consider edge cases around empty input."})
            );
        }
        other => panic!("expected ServerToolResult, got {other:?}"),
    }
}

#[test]
fn parse_assistant_message_with_redacted_advisor_result() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "advisor_tool_result", "tool_use_id": "srvtoolu_01ABC",
             "content": {"type": "advisor_redacted_result", "encrypted_content": "EuYDCioIDhgC..."}},
        ], "model": "claude-sonnet-4-5"},
    });
    let a = as_assistant(&data);
    match &a.content[0] {
        ContentBlock::ServerToolResult(r) => {
            assert_eq!(r.content["type"], json!("advisor_redacted_result"));
            assert_eq!(r.content["encrypted_content"], json!("EuYDCioIDhgC..."));
        }
        other => panic!("expected ServerToolResult, got {other:?}"),
    }
}

#[test]
fn parse_assistant_message_with_usage() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [{"type": "text", "text": "hi"}],
            "model": "claude-opus-4-5",
            "usage": {"input_tokens": 100, "output_tokens": 50,
                      "cache_read_input_tokens": 2000, "cache_creation_input_tokens": 500},
        },
    });
    let a = as_assistant(&data);
    assert_eq!(
        Value::Object(a.usage.clone().unwrap()),
        json!({"input_tokens": 100, "output_tokens": 50,
               "cache_read_input_tokens": 2000, "cache_creation_input_tokens": 500})
    );
}

#[test]
fn parse_assistant_message_without_usage() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": "hi"}], "model": "claude-opus-4-5"},
    });
    assert!(as_assistant(&data).usage.is_none());
}

#[test]
fn parse_assistant_message_inside_subagent() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [
            {"type": "text", "text": "Hello"},
            {"type": "tool_use", "id": "tool_123", "name": "Read", "input": {"file_path": "/test.txt"}},
        ], "model": "claude-opus-4-1-20250805"},
        "parent_tool_use_id": "toolu_01Xrwd5Y13sEHtzScxR77So8",
    });
    assert_eq!(
        as_assistant(&data).parent_tool_use_id.as_deref(),
        Some("toolu_01Xrwd5Y13sEHtzScxR77So8")
    );
}

#[test]
fn parse_assistant_message_without_error() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": "Hello"}], "model": "claude-opus-4-5-20251101"},
    });
    assert_eq!(as_assistant(&data).error, None);
}

#[test]
fn parse_assistant_message_error_classifications() {
    // The `error` field is at the top level (not inside `message`).
    let auth = json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": "Invalid API key"}], "model": "<synthetic>"},
        "session_id": "test-session",
        "error": "authentication_failed",
    });
    let a = as_assistant(&auth);
    assert_eq!(a.error, Some(AssistantMessageError::AuthenticationFailed));
    assert_eq!(a.content.len(), 1);
    assert!(matches!(a.content[0], ContentBlock::Text(_)));

    let unknown = json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": "API Error: 500"}], "model": "<synthetic>"},
        "session_id": "test-session",
        "error": "unknown",
    });
    assert_eq!(as_assistant(&unknown).error, Some(AssistantMessageError::Unknown));

    let rate = json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": "Rate limit exceeded"}], "model": "<synthetic>"},
        "error": "rate_limit",
    });
    assert_eq!(as_assistant(&rate).error, Some(AssistantMessageError::RateLimit));
}

#[test]
fn parse_assistant_message_with_all_fields() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [{"type": "text", "text": "Hello"}],
            "model": "claude-sonnet-4-5-20250929",
            "id": "msg_01HRq7YZE3apPqSHydvG77Ve",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5},
        },
        "session_id": "fdf2d90a-fd9e-4736-ae35-806edd13643f",
        "uuid": "0dbd2453-1209-4fe9-bd51-4102f64e33df",
    });
    let a = as_assistant(&data);
    assert_eq!(a.message_id.as_deref(), Some("msg_01HRq7YZE3apPqSHydvG77Ve"));
    assert_eq!(a.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(a.session_id.as_deref(), Some("fdf2d90a-fd9e-4736-ae35-806edd13643f"));
    assert_eq!(a.uuid.as_deref(), Some("0dbd2453-1209-4fe9-bd51-4102f64e33df"));
    assert_eq!(
        Value::Object(a.usage.clone().unwrap()),
        json!({"input_tokens": 10, "output_tokens": 5})
    );
}

#[test]
fn parse_assistant_message_optional_fields_absent() {
    let data = json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": "hi"}], "model": "claude-opus-4-5"},
    });
    let a = as_assistant(&data);
    assert_eq!(a.message_id, None);
    assert_eq!(a.stop_reason, None);
    assert_eq!(a.session_id, None);
    assert_eq!(a.uuid, None);
}

// ---- system messages ------------------------------------------------------

#[test]
fn parse_valid_system_message() {
    let data = json!({"type": "system", "subtype": "start"});
    let s = as_system(&data);
    assert_eq!(s.subtype, "start");
    assert!(s.kind.is_none());
}

#[test]
fn parse_task_started_message() {
    let data = json!({
        "type": "system", "subtype": "task_started",
        "task_id": "task-abc", "tool_use_id": "toolu_01",
        "description": "Reticulating splines", "task_type": "background",
        "uuid": "uuid-1", "session_id": "session-1",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskStarted(t)) = &s.kind else {
        panic!("expected TaskStarted, got {:?}", s.kind)
    };
    assert_eq!(t.task_id, "task-abc");
    assert_eq!(t.description, "Reticulating splines");
    assert_eq!(t.uuid, "uuid-1");
    assert_eq!(t.session_id, "session-1");
    assert_eq!(t.tool_use_id.as_deref(), Some("toolu_01"));
    assert_eq!(t.task_type.as_deref(), Some("background"));
}

#[test]
fn parse_task_started_message_optional_fields_absent() {
    let data = json!({
        "type": "system", "subtype": "task_started",
        "task_id": "task-abc", "description": "Working",
        "uuid": "uuid-1", "session_id": "session-1",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskStarted(t)) = &s.kind else { panic!() };
    assert_eq!(t.tool_use_id, None);
    assert_eq!(t.task_type, None);
}

#[test]
fn parse_task_progress_message() {
    let data = json!({
        "type": "system", "subtype": "task_progress",
        "task_id": "task-abc", "tool_use_id": "toolu_01", "description": "Halfway there",
        "usage": {"total_tokens": 1234, "tool_uses": 5, "duration_ms": 9876},
        "last_tool_name": "Read", "uuid": "uuid-2", "session_id": "session-1",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskProgress(t)) = &s.kind else { panic!() };
    assert_eq!(t.task_id, "task-abc");
    assert_eq!(t.description, "Halfway there");
    assert_eq!(t.usage.total_tokens, 1234);
    assert_eq!(t.usage.tool_uses, 5);
    assert_eq!(t.usage.duration_ms, 9876);
    assert_eq!(t.last_tool_name.as_deref(), Some("Read"));
    assert_eq!(t.tool_use_id.as_deref(), Some("toolu_01"));
    assert_eq!(t.uuid, "uuid-2");
    assert_eq!(t.session_id, "session-1");
}

#[test]
fn parse_task_notification_message() {
    let data = json!({
        "type": "system", "subtype": "task_notification",
        "task_id": "task-abc", "tool_use_id": "toolu_01",
        "status": "completed", "output_file": "/tmp/out.md", "summary": "All done",
        "usage": {"total_tokens": 2000, "tool_uses": 7, "duration_ms": 12345},
        "uuid": "uuid-3", "session_id": "session-1",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskNotification(t)) = &s.kind else { panic!() };
    assert_eq!(t.task_id, "task-abc");
    assert_eq!(t.status, TaskNotificationStatus::Completed);
    assert_eq!(t.output_file, "/tmp/out.md");
    assert_eq!(t.summary, "All done");
    let usage = t.usage.unwrap();
    assert_eq!(usage.total_tokens, 2000);
    assert_eq!(usage.tool_uses, 7);
    assert_eq!(usage.duration_ms, 12345);
    assert_eq!(t.tool_use_id.as_deref(), Some("toolu_01"));
    assert_eq!(t.uuid, "uuid-3");
    assert_eq!(t.session_id, "session-1");
}

#[test]
fn parse_task_notification_message_optional_fields_absent() {
    let data = json!({
        "type": "system", "subtype": "task_notification",
        "task_id": "task-abc", "status": "failed",
        "output_file": "/tmp/out.md", "summary": "Boom",
        "uuid": "uuid-3", "session_id": "session-1",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskNotification(t)) = &s.kind else { panic!() };
    assert_eq!(t.status, TaskNotificationStatus::Failed);
    assert_eq!(t.usage, None);
    assert_eq!(t.tool_use_id, None);
}

#[test]
fn parse_task_updated_message_terminal() {
    let data = json!({
        "type": "system", "subtype": "task_updated",
        "task_id": "task-abc",
        "patch": {"status": "completed", "end_time": 1_780_405_729_183i64},
        "uuid": "uuid-4", "session_id": "session-1",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
    assert_eq!(t.task_id, "task-abc");
    assert_eq!(
        Value::Object(t.patch.clone()),
        json!({"status": "completed", "end_time": 1_780_405_729_183i64})
    );
    assert_eq!(t.status, Some(TaskUpdatedStatus::Completed));
    assert_eq!(t.uuid.as_deref(), Some("uuid-4"));
    assert_eq!(t.session_id.as_deref(), Some("session-1"));
    assert!(terminal(&t.status.unwrap()));
}

#[test]
fn parse_task_updated_message_minimal() {
    let data = json!({
        "type": "system", "subtype": "task_updated",
        "task_id": "b1m21w89v",
        "patch": {"status": "completed", "end_time": 1_780_405_729_183i64},
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
    assert_eq!(t.task_id, "b1m21w89v");
    assert_eq!(t.status, Some(TaskUpdatedStatus::Completed));
    assert_eq!(t.uuid, None);
    assert_eq!(t.session_id, None);
}

#[test]
fn parse_task_updated_message_non_terminal_statuses() {
    for (status, expected) in [
        ("pending", TaskUpdatedStatus::Pending),
        ("running", TaskUpdatedStatus::Running),
        ("paused", TaskUpdatedStatus::Paused),
    ] {
        let data = json!({
            "type": "system", "subtype": "task_updated",
            "task_id": "task-abc", "patch": {"status": status},
        });
        let s = as_system(&data);
        let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
        assert_eq!(t.status, Some(expected));
        assert!(!terminal(&t.status.unwrap()));
    }
}

#[test]
fn parse_task_updated_message_no_patch() {
    let data = json!({"type": "system", "subtype": "task_updated", "task_id": "task-abc"});
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
    assert!(t.patch.is_empty());
    assert_eq!(t.status, None);
}

#[test]
fn parse_task_updated_message_patch_without_status() {
    let data = json!({
        "type": "system", "subtype": "task_updated", "task_id": "task-abc",
        "patch": {"end_time": 1_780_405_729_183i64},
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
    assert_eq!(Value::Object(t.patch.clone()), json!({"end_time": 1_780_405_729_183i64}));
    assert_eq!(t.status, None);
}

#[test]
fn parse_task_updated_message_non_dict_patch() {
    // A non-dict (or missing/null) patch never raises; patch falls back to {}.
    for patch in [json!("completed"), json!(["completed"]), json!(42), Value::Null] {
        let data = json!({
            "type": "system", "subtype": "task_updated", "task_id": "task-abc", "patch": patch,
        });
        let s = as_system(&data);
        let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
        assert!(t.patch.is_empty());
        assert_eq!(t.status, None);
    }
}

#[test]
fn parse_task_updated_message_terminal_statuses() {
    for (status, expected) in [
        ("completed", TaskUpdatedStatus::Completed),
        ("failed", TaskUpdatedStatus::Failed),
        ("killed", TaskUpdatedStatus::Killed),
    ] {
        let data = json!({
            "type": "system", "subtype": "task_updated",
            "task_id": "task-abc", "patch": {"status": status},
        });
        let s = as_system(&data);
        let Some(SystemMessageKind::TaskUpdated(t)) = &s.kind else { panic!() };
        assert_eq!(t.status, Some(expected));
        assert!(terminal(&t.status.unwrap()));
    }
}

#[test]
fn task_updated_backward_compat_base_fields() {
    // TaskUpdatedMessage is still a SystemMessage with subtype/data populated.
    let data = json!({
        "type": "system", "subtype": "task_updated",
        "task_id": "t1", "patch": {"status": "failed"},
        "uuid": "u1", "session_id": "s1",
    });
    let s = as_system(&data);
    assert!(matches!(s.kind, Some(SystemMessageKind::TaskUpdated(_))));
    assert_eq!(s.subtype, "task_updated");
    assert_eq!(Value::Object(s.data.clone()), data);
}

#[test]
fn task_message_backward_compat_base_fields() {
    let data = json!({
        "type": "system", "subtype": "task_started",
        "task_id": "t1", "description": "desc", "uuid": "u1", "session_id": "s1",
    });
    let s = as_system(&data);
    assert!(matches!(s.kind, Some(SystemMessageKind::TaskStarted(_))));
    assert_eq!(s.subtype, "task_started");
    assert_eq!(Value::Object(s.data.clone()), data);
    assert_eq!(s.data["task_id"], json!("t1"));
}

#[test]
fn unknown_system_subtype_yields_generic() {
    let data = json!({"type": "system", "subtype": "some_future_subtype", "foo": "bar"});
    let s = as_system(&data);
    assert!(s.kind.is_none());
    assert_eq!(s.subtype, "some_future_subtype");
    assert_eq!(Value::Object(s.data.clone()), data);
}

// ---- result messages ------------------------------------------------------

#[test]
fn parse_valid_result_message() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 1000, "duration_api_ms": 500,
        "is_error": false, "num_turns": 2, "session_id": "session_123",
    });
    let r = as_result(&data);
    assert_eq!(r.subtype, "success");
    assert_eq!(r.stop_reason, None);
}

#[test]
fn parse_result_message_with_stop_reason() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 1000, "duration_api_ms": 500,
        "is_error": false, "num_turns": 2, "session_id": "session_123",
        "stop_reason": "end_turn", "result": "Done",
    });
    let r = as_result(&data);
    assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(r.result.as_deref(), Some("Done"));
}

#[test]
fn parse_result_message_with_null_stop_reason() {
    let data = json!({
        "type": "result", "subtype": "error_max_turns",
        "duration_ms": 1000, "duration_api_ms": 500,
        "is_error": true, "num_turns": 10, "session_id": "session_123",
        "stop_reason": null,
    });
    assert_eq!(as_result(&data).stop_reason, None);
}

#[test]
fn parse_result_message_with_model_usage() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 3000, "duration_api_ms": 2000,
        "is_error": false, "num_turns": 1, "session_id": "fdf2d90a-fd9e-4736-ae35-806edd13643f",
        "stop_reason": "end_turn", "total_cost_usd": 0.0106,
        "usage": {"input_tokens": 3, "output_tokens": 24}, "result": "Hello",
        "modelUsage": {"claude-sonnet-4-5-20250929": {
            "inputTokens": 3, "outputTokens": 24, "cacheReadInputTokens": 20012,
            "costUSD": 0.0106, "contextWindow": 200000, "maxOutputTokens": 64000,
        }},
        "permission_denials": [],
        "uuid": "d379c496-f33a-4ea4-b920-3c5483baa6f7",
    });
    let r = as_result(&data);
    let mu = r.model_usage.clone().unwrap();
    assert!(mu.contains_key("claude-sonnet-4-5-20250929"));
    assert_eq!(mu["claude-sonnet-4-5-20250929"]["costUSD"], json!(0.0106));
    assert_eq!(r.permission_denials, Some(vec![]));
    assert_eq!(r.uuid.as_deref(), Some("d379c496-f33a-4ea4-b920-3c5483baa6f7"));
}

#[test]
fn parse_result_message_optional_fields_absent() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 1000, "duration_api_ms": 500,
        "is_error": false, "num_turns": 1, "session_id": "session_123",
    });
    let r = as_result(&data);
    assert_eq!(r.model_usage, None);
    assert_eq!(r.permission_denials, None);
    assert_eq!(r.deferred_tool_use, None);
    assert_eq!(r.errors, None);
    assert_eq!(r.api_error_status, None);
    assert_eq!(r.uuid, None);
}

#[test]
fn parse_result_message_with_deferred_tool_use() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 1200, "duration_api_ms": 900,
        "is_error": false, "num_turns": 1, "session_id": "session_123",
        "deferred_tool_use": {"id": "toolu_01abc", "name": "Bash",
                              "input": {"command": "rm -rf /tmp/scratch"}},
    });
    let r = as_result(&data);
    let d = r.deferred_tool_use.clone().unwrap();
    assert_eq!(d.id, "toolu_01abc");
    assert_eq!(d.name, "Bash");
    assert_eq!(Value::Object(d.input.clone()), json!({"command": "rm -rf /tmp/scratch"}));
}

#[test]
fn parse_result_message_with_errors() {
    let data = json!({
        "type": "result", "subtype": "error_during_execution",
        "duration_ms": 5000, "duration_api_ms": 3000,
        "is_error": true, "num_turns": 3, "session_id": "session_456",
        "errors": ["Tool execution failed: permission denied", "Unable to write to /etc/hosts"],
        "uuid": "err-uuid-789",
    });
    let r = as_result(&data);
    assert_eq!(
        r.errors,
        Some(vec![
            "Tool execution failed: permission denied".to_string(),
            "Unable to write to /etc/hosts".to_string(),
        ])
    );
    assert!(r.is_error);
    assert_eq!(r.subtype, "error_during_execution");
    assert_eq!(r.uuid.as_deref(), Some("err-uuid-789"));
}

#[test]
fn parse_result_message_with_api_error_status() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 2000, "duration_api_ms": 1500,
        "is_error": true, "num_turns": 1, "session_id": "session_overload",
        "api_error_status": 529,
    });
    let r = as_result(&data);
    assert_eq!(r.api_error_status, Some(529));
    assert!(r.is_error);
    assert_eq!(r.subtype, "success");
}

#[test]
fn parse_result_message_success_no_errors() {
    let data = json!({
        "type": "result", "subtype": "success",
        "duration_ms": 1000, "duration_api_ms": 500,
        "is_error": false, "num_turns": 1, "session_id": "session_789",
        "result": "Task completed successfully",
    });
    let r = as_result(&data);
    assert_eq!(r.errors, None);
    assert_eq!(r.result.as_deref(), Some("Task completed successfully"));
}

// ---- rate limit event -----------------------------------------------------

#[test]
fn parse_rate_limit_event() {
    let data = json!({
        "type": "rate_limit_event",
        "rate_limit_info": {"status": "allowed_warning", "resetsAt": 1_700_000_000i64,
                            "rateLimitType": "five_hour", "utilization": 0.91},
        "uuid": "abc-123", "session_id": "session_xyz",
    });
    let Message::RateLimit(e) = parse(&data) else { panic!("expected RateLimit") };
    assert_eq!(e.uuid, "abc-123");
    assert_eq!(e.session_id, "session_xyz");
    let info = &e.rate_limit_info;
    assert_eq!(serde_json::to_value(info.status).unwrap(), json!("allowed_warning"));
    assert_eq!(info.resets_at, Some(1_700_000_000));
    assert_eq!(serde_json::to_value(info.rate_limit_type.unwrap()).unwrap(), json!("five_hour"));
    assert_eq!(info.utilization, Some(0.91));
}

// ---- hook events ----------------------------------------------------------

#[test]
fn parse_hook_event_message() {
    let data = json!({
        "type": "system", "subtype": "hook_started",
        "hook_event": "PreToolUse", "hook_name": "PreToolUse",
        "session_id": "sess-123", "uuid": "uuid-456",
        "tool_name": "Bash", "tool_input": {"command": "ls"},
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::HookEvent(h)) = &s.kind else {
        panic!("expected HookEvent, got {:?}", s.kind)
    };
    assert_eq!(s.subtype, "hook_started");
    assert_eq!(h.hook_event_name, "PreToolUse");
    assert_eq!(h.session_id.as_deref(), Some("sess-123"));
    assert_eq!(h.uuid.as_deref(), Some("uuid-456"));
    assert_eq!(Value::Object(s.data.clone()), data);
}

#[test]
fn parse_hook_event_message_response() {
    let data = json!({
        "type": "system", "subtype": "hook_response",
        "hook_event": "PostToolUse", "hook_name": "PostToolUse",
        "session_id": "sess-123", "uuid": "uuid-789",
        "output": "", "exit_code": 0, "outcome": "success",
    });
    let s = as_system(&data);
    let Some(SystemMessageKind::HookEvent(h)) = &s.kind else { panic!() };
    assert_eq!(s.subtype, "hook_response");
    assert_eq!(h.hook_event_name, "PostToolUse");
    assert_eq!(h.session_id.as_deref(), Some("sess-123"));
    assert_eq!(h.uuid.as_deref(), Some("uuid-789"));
    assert_eq!(s.data["output"], json!(""));
    assert_eq!(s.data["exit_code"], json!(0));
    assert_eq!(s.data["outcome"], json!("success"));
}

#[test]
fn parse_hook_event_message_minimal() {
    // No session_id/uuid/hook_event; hook_event_name falls back to hook_name.
    let data = json!({"type": "system", "subtype": "hook_started", "hook_name": "Stop"});
    let s = as_system(&data);
    let Some(SystemMessageKind::HookEvent(h)) = &s.kind else { panic!() };
    assert_eq!(s.subtype, "hook_started");
    assert_eq!(h.hook_event_name, "Stop");
    assert_eq!(h.session_id, None);
    assert_eq!(h.uuid, None);
}

// ---- error / forward-compat arms ------------------------------------------

#[test]
fn parse_invalid_data_type() {
    // Non-object input raises MessageParse (Python: "Invalid message data type").
    assert!(matches!(
        parse_message(&json!("not a dict")),
        Err(Error::MessageParse { .. })
    ));
}

#[test]
fn parse_missing_type_field() {
    match parse_message(&json!({"message": {"content": []}})) {
        Err(Error::MessageParse { message, .. }) => {
            assert!(message.contains("Message missing 'type' field"), "{message}");
        }
        other => panic!("expected MessageParse, got {other:?}"),
    }
}

#[test]
fn parse_unknown_message_type() {
    // Forward-compatible skip: unknown top-level type -> Ok(None).
    assert!(parse_message(&json!({"type": "unknown_type"})).unwrap().is_none());
}

#[test]
fn parse_user_message_missing_fields() {
    match parse_message(&json!({"type": "user"})) {
        Err(Error::MessageParse { message, .. }) => {
            assert!(message.contains("Missing required field in user message"), "{message}");
        }
        other => panic!("expected MessageParse, got {other:?}"),
    }
}

#[test]
fn parse_assistant_message_missing_fields() {
    match parse_message(&json!({"type": "assistant"})) {
        Err(Error::MessageParse { message, .. }) => {
            assert!(message.contains("Missing required field in assistant message"), "{message}");
        }
        other => panic!("expected MessageParse, got {other:?}"),
    }
}

#[test]
fn parse_assistant_string_content_raises() {
    // Assistant content as a bare string is an error, not a raw panic.
    assert!(matches!(
        parse_message(&json!({"type": "assistant", "message": {"model": "m", "content": "hi"}})),
        Err(Error::MessageParse { .. })
    ));
}

#[test]
fn non_dict_content_block_raises() {
    // A non-dict block is an error for both roles.
    for role in ["assistant", "user"] {
        let mut message = json!({"content": ["oops"]});
        if role == "assistant" {
            message["model"] = json!("m");
        }
        assert!(
            matches!(
                parse_message(&json!({"type": role, "message": message})),
                Err(Error::MessageParse { .. })
            ),
            "role={role}"
        );
    }
}

#[test]
fn parse_system_message_missing_fields() {
    match parse_message(&json!({"type": "system"})) {
        Err(Error::MessageParse { message, .. }) => {
            assert!(message.contains("Missing required field in system message"), "{message}");
        }
        other => panic!("expected MessageParse, got {other:?}"),
    }
}

#[test]
fn parse_result_message_missing_fields() {
    match parse_message(&json!({"type": "result", "subtype": "success"})) {
        Err(Error::MessageParse { message, .. }) => {
            assert!(message.contains("Missing required field in result message"), "{message}");
        }
        other => panic!("expected MessageParse, got {other:?}"),
    }
}

#[test]
fn message_parse_error_contains_data() {
    // Malformed known type: the error carries the original data verbatim.
    let data = json!({"type": "assistant"});
    match parse_message(&data) {
        Err(Error::MessageParse { data: Some(d), .. }) => assert_eq!(d, data),
        other => panic!("expected MessageParse with data, got {other:?}"),
    }
}
