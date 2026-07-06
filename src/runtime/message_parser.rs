//! Parse raw CLI JSON into typed [`Message`]s.
//!
//! Faithful port of `_internal/message_parser.py`. Missing required fields
//! produce [`Error::MessageParse`]; unrecognized top-level message types are
//! skipped (`Ok(None)`) for forward compatibility, matching the upstream
//! `return None` default.

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::types::{
    AssistantMessage, ContentBlock, DeferredToolUse, HookEventMessage, Message, MirrorErrorMessage,
    RateLimitEvent, RateLimitInfo, ResultMessage, ServerToolResultBlock, ServerToolUseBlock,
    StreamEvent, SystemMessage, SystemMessageKind, TaskNotificationMessage, TaskProgressMessage,
    TaskStartedMessage, TaskUpdatedMessage, TextBlock, ThinkingBlock, ToolResultBlock,
    ToolResultContent, ToolUseBlock, UserContent, UserMessage,
};

fn parse_err(msg: impl Into<String>, data: &Value) -> Error {
    Error::message_parse(msg, Some(data.clone()))
}

fn as_object<'a>(data: &'a Value, ctx: &str) -> Result<&'a Map<String, Value>> {
    data.as_object()
        .ok_or_else(|| parse_err(format!("Invalid {ctx} (expected object)"), data))
}

/// Fetches a required field or returns a `MessageParseError`.
fn require<'a>(obj: &'a Map<String, Value>, key: &str, data: &Value, kind: &str) -> Result<&'a Value> {
    obj.get(key)
        .ok_or_else(|| parse_err(format!("Missing required field in {kind} message: '{key}'"), data))
}

fn require_str(obj: &Map<String, Value>, key: &str, data: &Value, kind: &str) -> Result<String> {
    require(obj, key, data, kind)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| parse_err(format!("Field '{key}' in {kind} message is not a string"), data))
}

fn require_i64(obj: &Map<String, Value>, key: &str, data: &Value, kind: &str) -> Result<i64> {
    require(obj, key, data, kind)?
        .as_i64()
        .ok_or_else(|| parse_err(format!("Field '{key}' in {kind} message is not an integer"), data))
}

fn opt_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_string)
}

fn opt_obj(obj: &Map<String, Value>, key: &str) -> Option<Map<String, Value>> {
    obj.get(key).and_then(Value::as_object).cloned()
}

/// Parses a single assistant/user content block. Unknown block types yield
/// `Ok(None)` (skipped), matching the upstream `match` with no default arm.
/// `allow_all` enables the assistant-only block variants.
fn parse_block(block: &Value, data: &Value, allow_all: bool) -> Result<Option<ContentBlock>> {
    let obj = block
        .as_object()
        .ok_or_else(|| parse_err("Invalid content block (expected object)", data))?;
    let btype = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| parse_err("Content block missing 'type'", data))?;
    let block_kind = "content block";
    Ok(match btype {
        "text" => Some(ContentBlock::Text(TextBlock {
            text: require_str(obj, "text", data, block_kind)?,
        })),
        "tool_use" => Some(ContentBlock::ToolUse(ToolUseBlock {
            id: require_str(obj, "id", data, block_kind)?,
            name: require_str(obj, "name", data, block_kind)?,
            input: opt_obj(obj, "input").unwrap_or_default(),
        })),
        "tool_result" => Some(ContentBlock::ToolResult(ToolResultBlock {
            tool_use_id: require_str(obj, "tool_use_id", data, block_kind)?,
            content: obj
                .get("content")
                .filter(|v| !v.is_null())
                .map(parse_tool_result_content),
            is_error: obj.get("is_error").and_then(Value::as_bool),
        })),
        "thinking" if allow_all => Some(ContentBlock::Thinking(ThinkingBlock {
            thinking: require_str(obj, "thinking", data, block_kind)?,
            signature: require_str(obj, "signature", data, block_kind)?,
        })),
        "server_tool_use" if allow_all => Some(ContentBlock::ServerToolUse(ServerToolUseBlock {
            id: require_str(obj, "id", data, block_kind)?,
            name: serde_json::from_value(require(obj, "name", data, block_kind)?.clone())
                .map_err(|_| parse_err("Unknown server tool name", data))?,
            input: opt_obj(obj, "input").unwrap_or_default(),
        })),
        // The wire tag for a server-tool result is `advisor_tool_result`.
        "advisor_tool_result" if allow_all => {
            Some(ContentBlock::ServerToolResult(ServerToolResultBlock {
                tool_use_id: require_str(obj, "tool_use_id", data, block_kind)?,
                content: opt_obj(obj, "content").unwrap_or_default(),
            }))
        }
        _ => None,
    })
}

fn parse_tool_result_content(v: &Value) -> ToolResultContent {
    match v {
        Value::String(s) => ToolResultContent::Text(s.clone()),
        Value::Array(a) => ToolResultContent::Blocks(a.clone()),
        other => ToolResultContent::Blocks(vec![other.clone()]),
    }
}

/// Parses one raw CLI message. Returns `Ok(None)` for unrecognized message
/// types (forward-compatible skip). Faithful port of `parse_message`.
pub fn parse_message(data: &Value) -> Result<Option<Message>> {
    let obj = as_object(data, "message data")?;

    // Hook events arrive as system/hook_started|hook_response.
    if obj.get("type").and_then(Value::as_str) == Some("system") {
        if let Some(subtype) = obj.get("subtype").and_then(Value::as_str) {
            if subtype == "hook_started" || subtype == "hook_response" {
                let hook_event_name = obj
                    .get("hook_event")
                    .or_else(|| obj.get("hook_name"))
                    .or_else(|| obj.get("hook_event_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                return Ok(Some(Message::System(SystemMessage {
                    subtype: subtype.to_string(),
                    data: obj.clone(),
                    kind: Some(SystemMessageKind::HookEvent(HookEventMessage {
                        hook_event_name,
                        session_id: opt_str(obj, "session_id"),
                        uuid: opt_str(obj, "uuid"),
                    })),
                })));
            }
        }
    }

    let message_type = match obj.get("type").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t,
        _ => return Err(parse_err("Message missing 'type' field", data)),
    };

    match message_type {
        "user" => {
            let message = require(obj, "message", data, "user")?;
            let content = require(
                message.as_object().ok_or_else(|| {
                    parse_err("Field 'message' in user message is not an object", data)
                })?,
                "content",
                data,
                "user",
            )?;
            let user_content = match content {
                Value::Array(arr) => {
                    let mut blocks = Vec::new();
                    for block in arr {
                        if let Some(b) = parse_block(block, data, false)? {
                            blocks.push(b);
                        }
                    }
                    UserContent::Blocks(blocks)
                }
                Value::String(s) => UserContent::Text(s.clone()),
                _ => {
                    return Err(parse_err(
                        "Invalid user content (expected string or list)",
                        data,
                    ))
                }
            };
            Ok(Some(Message::User(UserMessage {
                content: user_content,
                uuid: opt_str(obj, "uuid"),
                parent_tool_use_id: opt_str(obj, "parent_tool_use_id"),
                tool_use_result: opt_obj(obj, "tool_use_result"),
            })))
        }

        "assistant" => {
            let message = require(obj, "message", data, "assistant")?
                .as_object()
                .ok_or_else(|| {
                    parse_err("Field 'message' in assistant message is not an object", data)
                })?;
            let raw_content = require(message, "content", data, "assistant")?;
            let arr = raw_content
                .as_array()
                .ok_or_else(|| parse_err("Invalid assistant content (expected list)", data))?;
            let mut blocks = Vec::new();
            for block in arr {
                if let Some(b) = parse_block(block, data, true)? {
                    blocks.push(b);
                }
            }
            Ok(Some(Message::Assistant(AssistantMessage {
                content: blocks,
                model: require_str(message, "model", data, "assistant")?,
                parent_tool_use_id: opt_str(obj, "parent_tool_use_id"),
                error: obj
                    .get("error")
                    .and_then(|e| serde_json::from_value(e.clone()).ok()),
                usage: opt_obj(message, "usage"),
                message_id: opt_str(message, "id"),
                stop_reason: opt_str(message, "stop_reason"),
                session_id: opt_str(obj, "session_id"),
                uuid: opt_str(obj, "uuid"),
            })))
        }

        "system" => {
            let subtype = require_str(obj, "subtype", data, "system")?;
            let kind = match subtype.as_str() {
                "task_started" => Some(SystemMessageKind::TaskStarted(TaskStartedMessage {
                    task_id: require_str(obj, "task_id", data, "system")?,
                    description: require_str(obj, "description", data, "system")?,
                    uuid: require_str(obj, "uuid", data, "system")?,
                    session_id: require_str(obj, "session_id", data, "system")?,
                    tool_use_id: opt_str(obj, "tool_use_id"),
                    task_type: opt_str(obj, "task_type"),
                })),
                "task_progress" => Some(SystemMessageKind::TaskProgress(TaskProgressMessage {
                    task_id: require_str(obj, "task_id", data, "system")?,
                    description: require_str(obj, "description", data, "system")?,
                    usage: serde_json::from_value(require(obj, "usage", data, "system")?.clone())
                        .map_err(|_| parse_err("Invalid task usage", data))?,
                    uuid: require_str(obj, "uuid", data, "system")?,
                    session_id: require_str(obj, "session_id", data, "system")?,
                    tool_use_id: opt_str(obj, "tool_use_id"),
                    last_tool_name: opt_str(obj, "last_tool_name"),
                })),
                "task_notification" => {
                    Some(SystemMessageKind::TaskNotification(TaskNotificationMessage {
                        task_id: require_str(obj, "task_id", data, "system")?,
                        status: serde_json::from_value(
                            require(obj, "status", data, "system")?.clone(),
                        )
                        .map_err(|_| parse_err("Invalid task notification status", data))?,
                        output_file: require_str(obj, "output_file", data, "system")?,
                        summary: require_str(obj, "summary", data, "system")?,
                        uuid: require_str(obj, "uuid", data, "system")?,
                        session_id: require_str(obj, "session_id", data, "system")?,
                        tool_use_id: opt_str(obj, "tool_use_id"),
                        usage: obj
                            .get("usage")
                            .and_then(|u| serde_json::from_value(u.clone()).ok()),
                    }))
                }
                "task_updated" => {
                    // Parsed defensively — a lifecycle event must never fail to parse.
                    let patch = opt_obj(obj, "patch").unwrap_or_default();
                    let status = patch
                        .get("status")
                        .and_then(|s| serde_json::from_value(s.clone()).ok());
                    Some(SystemMessageKind::TaskUpdated(TaskUpdatedMessage {
                        task_id: opt_str(obj, "task_id").unwrap_or_default(),
                        patch,
                        status,
                        session_id: opt_str(obj, "session_id"),
                        uuid: opt_str(obj, "uuid"),
                    }))
                }
                "mirror_error" => Some(SystemMessageKind::MirrorError(MirrorErrorMessage {
                    key: obj
                        .get("key")
                        .and_then(|k| serde_json::from_value(k.clone()).ok()),
                    error: opt_str(obj, "error").unwrap_or_default(),
                })),
                _ => None,
            };
            Ok(Some(Message::System(SystemMessage {
                subtype,
                data: obj.clone(),
                kind,
            })))
        }

        "result" => {
            let deferred_tool_use = match obj.get("deferred_tool_use") {
                Some(d) if d.is_object() => {
                    let d = d.as_object().unwrap();
                    Some(DeferredToolUse {
                        id: require_str(d, "id", data, "result")?,
                        name: require_str(d, "name", data, "result")?,
                        input: opt_obj(d, "input").unwrap_or_default(),
                    })
                }
                _ => None,
            };
            Ok(Some(Message::Result(ResultMessage {
                subtype: require_str(obj, "subtype", data, "result")?,
                duration_ms: require_i64(obj, "duration_ms", data, "result")?,
                duration_api_ms: require_i64(obj, "duration_api_ms", data, "result")?,
                is_error: require(obj, "is_error", data, "result")?
                    .as_bool()
                    .ok_or_else(|| parse_err("Field 'is_error' is not a boolean", data))?,
                num_turns: require_i64(obj, "num_turns", data, "result")?,
                session_id: require_str(obj, "session_id", data, "result")?,
                stop_reason: opt_str(obj, "stop_reason"),
                total_cost_usd: obj.get("total_cost_usd").and_then(Value::as_f64),
                usage: opt_obj(obj, "usage"),
                result: opt_str(obj, "result"),
                structured_output: obj.get("structured_output").cloned(),
                model_usage: opt_obj(obj, "modelUsage"),
                permission_denials: obj
                    .get("permission_denials")
                    .and_then(Value::as_array)
                    .cloned(),
                deferred_tool_use,
                errors: obj.get("errors").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|e| e.as_str().map(str::to_string))
                        .collect()
                }),
                api_error_status: obj.get("api_error_status").and_then(Value::as_i64),
                uuid: opt_str(obj, "uuid"),
            })))
        }

        "stream_event" => Ok(Some(Message::StreamEvent(StreamEvent {
            uuid: require_str(obj, "uuid", data, "stream_event")?,
            session_id: require_str(obj, "session_id", data, "stream_event")?,
            event: opt_obj(obj, "event")
                .ok_or_else(|| parse_err("Missing required field in stream_event message: 'event'", data))?,
            parent_tool_use_id: opt_str(obj, "parent_tool_use_id"),
        }))),

        "rate_limit_event" => {
            let info = require(obj, "rate_limit_info", data, "rate_limit_event")?
                .as_object()
                .ok_or_else(|| parse_err("Field 'rate_limit_info' is not an object", data))?;
            Ok(Some(Message::RateLimit(RateLimitEvent {
                rate_limit_info: RateLimitInfo {
                    status: serde_json::from_value(
                        require(info, "status", data, "rate_limit_event")?.clone(),
                    )
                    .map_err(|_| parse_err("Invalid rate limit status", data))?,
                    resets_at: info.get("resetsAt").and_then(Value::as_i64),
                    rate_limit_type: info
                        .get("rateLimitType")
                        .and_then(|t| serde_json::from_value(t.clone()).ok()),
                    utilization: info.get("utilization").and_then(Value::as_f64),
                    overage_status: info
                        .get("overageStatus")
                        .and_then(|s| serde_json::from_value(s.clone()).ok()),
                    overage_resets_at: info.get("overageResetsAt").and_then(Value::as_i64),
                    overage_disabled_reason: opt_str(info, "overageDisabledReason"),
                    raw: info.clone(),
                },
                uuid: require_str(obj, "uuid", data, "rate_limit_event")?,
                session_id: require_str(obj, "session_id", data, "rate_limit_event")?,
            })))
        }

        // Forward-compatible: skip unrecognized message types.
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_assistant_text() {
        let data = json!({
            "type": "assistant",
            "message": {"model": "claude-sonnet-4-5", "content": [{"type": "text", "text": "hi"}]},
            "session_id": "s1",
        });
        let msg = parse_message(&data).unwrap().unwrap();
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.model, "claude-sonnet-4-5");
                assert_eq!(a.content.len(), 1);
                assert_eq!(a.session_id.as_deref(), Some("s1"));
            }
            _ => panic!("wrong"),
        }
    }

    #[test]
    fn parse_assistant_all_block_types() {
        let data = json!({
            "type": "assistant",
            "message": {"model": "m", "content": [
                {"type": "text", "text": "t"},
                {"type": "thinking", "thinking": "th", "signature": "sig"},
                {"type": "tool_use", "id": "u1", "name": "Bash", "input": {"cmd": "ls"}},
                {"type": "tool_result", "tool_use_id": "u1", "content": "ok"},
                {"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {}},
                {"type": "advisor_tool_result", "tool_use_id": "s1", "content": {"x": 1}},
                {"type": "unknown_future_block", "foo": "bar"},
            ]},
        });
        let msg = parse_message(&data).unwrap().unwrap();
        let Message::Assistant(a) = msg else { panic!() };
        assert_eq!(a.content.len(), 6); // unknown skipped
        assert!(matches!(a.content[1], ContentBlock::Thinking(_)));
        assert!(matches!(a.content[5], ContentBlock::ServerToolResult(_)));
    }

    #[test]
    fn parse_user_string_content() {
        let data = json!({"type": "user", "message": {"content": "hello"}});
        let Message::User(u) = parse_message(&data).unwrap().unwrap() else { panic!() };
        assert_eq!(u.content, UserContent::Text("hello".into()));
    }

    #[test]
    fn parse_result() {
        let data = json!({
            "type": "result", "subtype": "success", "duration_ms": 10,
            "duration_api_ms": 5, "is_error": false, "num_turns": 1, "session_id": "s",
            "total_cost_usd": 0.01, "modelUsage": {"a": 1},
        });
        let Message::Result(r) = parse_message(&data).unwrap().unwrap() else { panic!() };
        assert_eq!(r.subtype, "success");
        assert_eq!(r.total_cost_usd, Some(0.01));
        assert!(r.model_usage.is_some());
    }

    #[test]
    fn parse_system_task_started() {
        let data = json!({
            "type": "system", "subtype": "task_started", "task_id": "t1",
            "description": "do it", "uuid": "u", "session_id": "s",
        });
        let Message::System(s) = parse_message(&data).unwrap().unwrap() else { panic!() };
        assert_eq!(s.subtype, "task_started");
        assert!(matches!(s.kind, Some(SystemMessageKind::TaskStarted(_))));
    }

    #[test]
    fn parse_hook_event() {
        let data = json!({
            "type": "system", "subtype": "hook_started", "hook_event": "PreToolUse",
            "session_id": "s", "uuid": "u",
        });
        let Message::System(s) = parse_message(&data).unwrap().unwrap() else { panic!() };
        match s.kind {
            Some(SystemMessageKind::HookEvent(h)) => assert_eq!(h.hook_event_name, "PreToolUse"),
            _ => panic!(),
        }
    }

    #[test]
    fn parse_generic_system() {
        let data = json!({"type": "system", "subtype": "init", "cwd": "/x"});
        let Message::System(s) = parse_message(&data).unwrap().unwrap() else { panic!() };
        assert_eq!(s.subtype, "init");
        assert!(s.kind.is_none());
    }

    #[test]
    fn unknown_type_skipped() {
        let data = json!({"type": "brand_new_type", "foo": 1});
        assert!(parse_message(&data).unwrap().is_none());
    }

    #[test]
    fn missing_type_errors() {
        let data = json!({"foo": 1});
        assert!(matches!(parse_message(&data), Err(Error::MessageParse { .. })));
    }

    #[test]
    fn assistant_non_list_content_errors() {
        let data = json!({"type": "assistant", "message": {"model": "m", "content": "oops"}});
        assert!(parse_message(&data).is_err());
    }

    #[test]
    fn rate_limit_event() {
        let data = json!({
            "type": "rate_limit_event", "uuid": "u", "session_id": "s",
            "rate_limit_info": {"status": "allowed_warning", "resetsAt": 123, "rateLimitType": "five_hour"},
        });
        let Message::RateLimit(e) = parse_message(&data).unwrap().unwrap() else { panic!() };
        assert_eq!(e.rate_limit_info.resets_at, Some(123));
    }
}
