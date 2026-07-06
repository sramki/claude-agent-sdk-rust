//! Integration parity tests ported from upstream `tests/test_types.py`
//! (claude-agent-sdk-python v0.2.110).
//!
//! Covers the SDK type-definition contracts: `PermissionUpdate` wire-format
//! conversion (`to_wire`/`from_wire`, upstream `to_dict`/`from_dict`), enum wire
//! values, message/content-block construction, `ClaudeAgentOptions` defaults,
//! and `AgentDefinition::to_wire` camelCase keys.
//!
//! Python-only mechanics with no Rust analog are intentionally skipped:
//! `TypedDict`/dataclass introspection (`get_args`, `asdict`, `__required_keys__`),
//! `isinstance`, and the hook-input / MCP-status `TypedDict` construction tests
//! (those types are dicts in Python; the store integration is out of scope).

use claude_agent_sdk_rs::types::{
    AgentDefinition, AssistantMessage, ClaudeAgentOptions, ContentBlock, EffortLevel,
    PermissionBehavior, PermissionMode, PermissionRuleValue, PermissionUpdate,
    PermissionUpdateDestination, PermissionUpdateType, ResultMessage, TextBlock, ThinkingBlock,
    ToolResultBlock, ToolResultContent, ToolUseBlock, UserContent, UserMessage,
};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// test_effort_level_is_exported — Rust analog: the enum's wire values.
// ---------------------------------------------------------------------------

#[test]
fn effort_level_wire_values() {
    // Python asserts get_args(EffortLevel) == {"low","medium","high","xhigh","max"}.
    // The Rust analog is the serde wire value of each variant.
    assert_eq!(
        serde_json::to_value(EffortLevel::Low).unwrap(),
        json!("low")
    );
    assert_eq!(
        serde_json::to_value(EffortLevel::Medium).unwrap(),
        json!("medium")
    );
    assert_eq!(
        serde_json::to_value(EffortLevel::High).unwrap(),
        json!("high")
    );
    assert_eq!(
        serde_json::to_value(EffortLevel::Xhigh).unwrap(),
        json!("xhigh")
    );
    assert_eq!(
        serde_json::to_value(EffortLevel::Max).unwrap(),
        json!("max")
    );
}

// ---------------------------------------------------------------------------
// TestPermissionUpdate — wire-format conversion (to_wire/from_wire).
// ---------------------------------------------------------------------------

#[test]
fn from_wire_to_wire_roundtrip_add_rules() {
    let wire = json!({
        "type": "addRules",
        "destination": "localSettings",
        "behavior": "allow",
        "rules": [
            {"toolName": "Bash", "ruleContent": "npm *"},
            {"toolName": "Read", "ruleContent": null},
        ],
    });
    let update = PermissionUpdate::from_wire(&wire).unwrap();
    assert_eq!(update.update_type, Some(PermissionUpdateType::AddRules));
    assert_eq!(
        update.destination,
        Some(PermissionUpdateDestination::LocalSettings)
    );
    assert_eq!(update.behavior, Some(PermissionBehavior::Allow));
    assert_eq!(
        update.rules,
        Some(vec![
            PermissionRuleValue {
                tool_name: "Bash".into(),
                rule_content: Some("npm *".into()),
            },
            PermissionRuleValue {
                tool_name: "Read".into(),
                rule_content: None,
            },
        ])
    );
    // to_wire must reproduce the original wire dict exactly (object compare is
    // key-order independent).
    assert_eq!(update.to_wire(), wire);
}

#[test]
fn from_wire_set_mode() {
    let wire = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
    let update = PermissionUpdate::from_wire(&wire).unwrap();
    assert_eq!(update.update_type, Some(PermissionUpdateType::SetMode));
    assert_eq!(update.mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(update.rules, None);
    assert_eq!(update.to_wire(), wire);
}

#[test]
fn from_wire_directories() {
    let wire = json!({
        "type": "addDirectories",
        "directories": ["/tmp/a", "/tmp/b"],
        "destination": "userSettings",
    });
    let update = PermissionUpdate::from_wire(&wire).unwrap();
    assert_eq!(
        update.update_type,
        Some(PermissionUpdateType::AddDirectories)
    );
    assert_eq!(
        update.directories,
        Some(vec!["/tmp/a".to_string(), "/tmp/b".to_string()])
    );
    assert_eq!(update.to_wire(), wire);
}

#[test]
fn from_wire_remove_rules_roundtrip() {
    // Not in upstream verbatim but exercises the removeRules variant on the same
    // rules code path.
    let wire = json!({
        "type": "removeRules",
        "destination": "projectSettings",
        "behavior": "deny",
        "rules": [{"toolName": "Write", "ruleContent": null}],
    });
    let update = PermissionUpdate::from_wire(&wire).unwrap();
    assert_eq!(update.update_type, Some(PermissionUpdateType::RemoveRules));
    assert_eq!(update.behavior, Some(PermissionBehavior::Deny));
    assert_eq!(update.to_wire(), wire);
}

#[test]
fn permission_update_replace_and_remove_directories_wire_types() {
    // Exercises the remaining PermissionUpdateType wire strings for full coverage.
    let replace = PermissionUpdate {
        update_type: Some(PermissionUpdateType::ReplaceRules),
        rules: Some(vec![]),
        ..Default::default()
    };
    assert_eq!(replace.to_wire()["type"], json!("replaceRules"));

    let remove_dirs = PermissionUpdate {
        update_type: Some(PermissionUpdateType::RemoveDirectories),
        directories: Some(vec!["/x".into()]),
        ..Default::default()
    };
    assert_eq!(
        remove_dirs.to_wire(),
        json!({"type": "removeDirectories", "directories": ["/x"]})
    );
}

// ---------------------------------------------------------------------------
// Enum wire values (serde renames).
// ---------------------------------------------------------------------------

#[test]
fn permission_mode_wire_values() {
    assert_eq!(
        serde_json::to_value(PermissionMode::Default).unwrap(),
        json!("default")
    );
    assert_eq!(
        serde_json::to_value(PermissionMode::AcceptEdits).unwrap(),
        json!("acceptEdits")
    );
    assert_eq!(
        serde_json::to_value(PermissionMode::Plan).unwrap(),
        json!("plan")
    );
    assert_eq!(
        serde_json::to_value(PermissionMode::BypassPermissions).unwrap(),
        json!("bypassPermissions")
    );
    assert_eq!(
        serde_json::to_value(PermissionMode::DontAsk).unwrap(),
        json!("dontAsk")
    );
    assert_eq!(
        serde_json::to_value(PermissionMode::Auto).unwrap(),
        json!("auto")
    );
}

#[test]
fn permission_behavior_wire_values() {
    assert_eq!(
        serde_json::to_value(PermissionBehavior::Allow).unwrap(),
        json!("allow")
    );
    assert_eq!(
        serde_json::to_value(PermissionBehavior::Deny).unwrap(),
        json!("deny")
    );
    assert_eq!(
        serde_json::to_value(PermissionBehavior::Ask).unwrap(),
        json!("ask")
    );
}

#[test]
fn permission_update_destination_wire_values() {
    assert_eq!(
        serde_json::to_value(PermissionUpdateDestination::UserSettings).unwrap(),
        json!("userSettings")
    );
    assert_eq!(
        serde_json::to_value(PermissionUpdateDestination::ProjectSettings).unwrap(),
        json!("projectSettings")
    );
    assert_eq!(
        serde_json::to_value(PermissionUpdateDestination::LocalSettings).unwrap(),
        json!("localSettings")
    );
    assert_eq!(
        serde_json::to_value(PermissionUpdateDestination::Session).unwrap(),
        json!("session")
    );
}

// ---------------------------------------------------------------------------
// TestMessageTypes — message / content-block construction.
// ---------------------------------------------------------------------------

#[test]
fn user_message_creation() {
    let msg = UserMessage {
        content: UserContent::Text("Hello, Claude!".into()),
        uuid: None,
        parent_tool_use_id: None,
        tool_use_result: None,
    };
    assert_eq!(msg.content, UserContent::Text("Hello, Claude!".into()));
}

#[test]
fn assistant_message_with_text() {
    let text_block = ContentBlock::Text(TextBlock {
        text: "Hello, human!".into(),
    });
    let msg = AssistantMessage {
        content: vec![text_block],
        model: "claude-opus-4-1-20250805".into(),
        parent_tool_use_id: None,
        error: None,
        usage: None,
        message_id: None,
        stop_reason: None,
        session_id: None,
        uuid: None,
    };
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::Text(b) => assert_eq!(b.text, "Hello, human!"),
        _ => panic!("expected TextBlock"),
    }
    assert_eq!(msg.model, "claude-opus-4-1-20250805");
}

#[test]
fn assistant_message_with_thinking() {
    let thinking_block = ContentBlock::Thinking(ThinkingBlock {
        thinking: "I'm thinking...".into(),
        signature: "sig-123".into(),
    });
    let msg = AssistantMessage {
        content: vec![thinking_block],
        model: "claude-opus-4-1-20250805".into(),
        parent_tool_use_id: None,
        error: None,
        usage: None,
        message_id: None,
        stop_reason: None,
        session_id: None,
        uuid: None,
    };
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::Thinking(b) => {
            assert_eq!(b.thinking, "I'm thinking...");
            assert_eq!(b.signature, "sig-123");
        }
        _ => panic!("expected ThinkingBlock"),
    }
}

#[test]
fn tool_use_block() {
    let mut input = Map::new();
    input.insert("file_path".into(), json!("/test.txt"));
    let block = ToolUseBlock {
        id: "tool-123".into(),
        name: "Read".into(),
        input,
    };
    assert_eq!(block.id, "tool-123");
    assert_eq!(block.name, "Read");
    assert_eq!(block.input["file_path"], json!("/test.txt"));
}

#[test]
fn tool_result_block() {
    let block = ToolResultBlock {
        tool_use_id: "tool-123".into(),
        content: Some(ToolResultContent::Text("File contents here".into())),
        is_error: Some(false),
    };
    assert_eq!(block.tool_use_id, "tool-123");
    assert_eq!(
        block.content,
        Some(ToolResultContent::Text("File contents here".into()))
    );
    assert_eq!(block.is_error, Some(false));
}

#[test]
fn result_message() {
    let msg = ResultMessage {
        subtype: "success".into(),
        duration_ms: 1500,
        duration_api_ms: 1200,
        is_error: false,
        num_turns: 1,
        session_id: "session-123".into(),
        total_cost_usd: Some(0.01),
        stop_reason: None,
        usage: None,
        result: None,
        structured_output: None,
        model_usage: None,
        permission_denials: None,
        deferred_tool_use: None,
        errors: None,
        api_error_status: None,
        uuid: None,
    };
    assert_eq!(msg.subtype, "success");
    assert_eq!(msg.total_cost_usd, Some(0.01));
    assert_eq!(msg.session_id, "session-123");
}

// Content-block tag round-trips (verifies the serde `type` discriminator).
#[test]
fn content_block_tags_roundtrip() {
    let v = serde_json::to_value(ContentBlock::Text(TextBlock { text: "hi".into() })).unwrap();
    assert_eq!(v, json!({"type": "text", "text": "hi"}));

    let v = serde_json::to_value(ContentBlock::Thinking(ThinkingBlock {
        thinking: "t".into(),
        signature: "s".into(),
    }))
    .unwrap();
    assert_eq!(
        v,
        json!({"type": "thinking", "thinking": "t", "signature": "s"})
    );
}

// ---------------------------------------------------------------------------
// TestOptions — ClaudeAgentOptions defaults & field construction.
// ---------------------------------------------------------------------------

#[test]
fn default_options() {
    let options = ClaudeAgentOptions::default();
    assert_eq!(options.allowed_tools, Vec::<String>::new());
    assert!(options.system_prompt.is_none());
    assert!(options.permission_mode.is_none());
    assert!(!options.continue_conversation);
    assert_eq!(options.disallowed_tools, Vec::<String>::new());
}

#[test]
fn options_with_tools() {
    let options = ClaudeAgentOptions {
        allowed_tools: vec!["Read".into(), "Write".into(), "Edit".into()],
        disallowed_tools: vec!["Bash".into()],
        ..Default::default()
    };
    assert_eq!(options.allowed_tools, vec!["Read", "Write", "Edit"]);
    assert_eq!(options.disallowed_tools, vec!["Bash"]);
}

#[test]
fn options_with_permission_mode() {
    for mode in [
        PermissionMode::BypassPermissions,
        PermissionMode::Plan,
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
    ] {
        let options = ClaudeAgentOptions {
            permission_mode: Some(mode),
            ..Default::default()
        };
        assert_eq!(options.permission_mode, Some(mode));
    }
}

#[test]
fn options_with_session_continuation() {
    let options = ClaudeAgentOptions {
        continue_conversation: true,
        resume: Some("session-123".into()),
        ..Default::default()
    };
    assert!(options.continue_conversation);
    assert_eq!(options.resume.as_deref(), Some("session-123"));
}

#[test]
fn options_with_model_specification() {
    let options = ClaudeAgentOptions {
        model: Some("claude-sonnet-4-5".into()),
        permission_prompt_tool_name: Some("CustomTool".into()),
        ..Default::default()
    };
    assert_eq!(options.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(
        options.permission_prompt_tool_name.as_deref(),
        Some("CustomTool")
    );
}

// ---------------------------------------------------------------------------
// TestAgentDefinition — to_wire serialization contract (camelCase CLI keys).
// ---------------------------------------------------------------------------

#[test]
fn agent_minimal_definition_omits_unset_fields() {
    let agent = AgentDefinition::new("test", "You are a test");
    let payload = agent.to_wire();
    assert_eq!(
        payload,
        json!({"description": "test", "prompt": "You are a test"})
    );
}

#[test]
fn agent_skills_and_memory_serialize() {
    let mut agent = AgentDefinition::new("test", "p");
    agent.skills = Some(vec!["skill-a".into(), "skill-b".into()]);
    agent.memory = Some(claude_agent_sdk_rs::types::SettingSource::Project);
    let payload = agent.to_wire();
    assert_eq!(payload["skills"], json!(["skill-a", "skill-b"]));
    assert_eq!(payload["memory"], json!("project"));
}

#[test]
fn agent_mcp_servers_serializes_as_camelcase() {
    use claude_agent_sdk_rs::types::AgentMcpServer;
    let mut local = Map::new();
    let mut cfg = Map::new();
    cfg.insert("command".into(), json!("python"));
    cfg.insert("args".into(), json!(["server.py"]));
    local.insert("local".into(), Value::Object(cfg));

    let mut agent = AgentDefinition::new("test", "p");
    agent.mcp_servers = Some(vec![
        AgentMcpServer::Name("slack".into()),
        AgentMcpServer::Inline(local),
    ]);
    let payload = agent.to_wire();
    let obj = payload.as_object().unwrap();
    assert!(obj.contains_key("mcpServers"));
    assert!(!obj.contains_key("mcp_servers"));
    assert_eq!(payload["mcpServers"][0], json!("slack"));
    assert_eq!(
        payload["mcpServers"][1]["local"]["command"],
        json!("python")
    );
}

#[test]
fn agent_disallowed_tools_and_max_turns_camelcase() {
    let mut agent = AgentDefinition::new("test", "p");
    agent.disallowed_tools = Some(vec!["Bash".into(), "Write".into()]);
    agent.max_turns = Some(10);
    let payload = agent.to_wire();
    let obj = payload.as_object().unwrap();
    assert_eq!(payload["disallowedTools"], json!(["Bash", "Write"]));
    assert!(!obj.contains_key("disallowed_tools"));
    assert_eq!(payload["maxTurns"], json!(10));
    assert!(!obj.contains_key("max_turns"));
}

#[test]
fn agent_initial_prompt_camelcase() {
    let mut agent = AgentDefinition::new("test", "p");
    agent.initial_prompt = Some("/review-pr 123".into());
    let payload = agent.to_wire();
    let obj = payload.as_object().unwrap();
    assert_eq!(payload["initialPrompt"], json!("/review-pr 123"));
    assert!(!obj.contains_key("initial_prompt"));
}

#[test]
fn agent_model_accepts_full_model_id() {
    let mut agent = AgentDefinition::new("test", "p");
    agent.model = Some("claude-opus-4-5".into());
    assert_eq!(agent.to_wire()["model"], json!("claude-opus-4-5"));
}

#[test]
fn agent_background_serializes() {
    let mut agent = AgentDefinition::new("test", "p");
    agent.background = Some(true);
    assert_eq!(agent.to_wire()["background"], json!(true));
}

#[test]
fn agent_effort_named_and_xhigh_and_integer() {
    use claude_agent_sdk_rs::types::AgentEffort;

    let mut agent = AgentDefinition::new("test", "p");
    agent.effort = Some(AgentEffort::Level(EffortLevel::High));
    assert_eq!(agent.to_wire()["effort"], json!("high"));

    agent.effort = Some(AgentEffort::Level(EffortLevel::Xhigh));
    assert_eq!(agent.to_wire()["effort"], json!("xhigh"));

    agent.effort = Some(AgentEffort::Int(32000));
    assert_eq!(agent.to_wire()["effort"], json!(32000));
}

#[test]
fn agent_permission_mode_camelcase() {
    let mut agent = AgentDefinition::new("test", "p");
    agent.permission_mode = Some(PermissionMode::BypassPermissions);
    let payload = agent.to_wire();
    let obj = payload.as_object().unwrap();
    assert_eq!(payload["permissionMode"], json!("bypassPermissions"));
    assert!(!obj.contains_key("permission_mode"));
}

#[test]
fn agent_new_fields_omitted_when_none() {
    let agent = AgentDefinition::new("test", "p");
    let obj = agent.to_wire();
    let obj = obj.as_object().unwrap();
    assert!(!obj.contains_key("background"));
    assert!(!obj.contains_key("effort"));
    assert!(!obj.contains_key("permissionMode"));
}
