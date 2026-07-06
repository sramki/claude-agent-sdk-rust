//! End-to-end tests against the **real** `claude` CLI.
//!
//! These validate the transport + control protocol against the actual binary —
//! something the mock-transport tests can't. They are **skipped gracefully**
//! (the test passes, printing a notice) when `claude` is not found on `PATH`,
//! or when `CLAUDE_SDK_SKIP_LIVE_TESTS` is set — so `cargo test` never fails
//! merely because the binary (or credentials) are unavailable. When present,
//! they issue a tiny text-only query (built-in tools disabled) and assert a
//! non-error result.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use claude_agent_sdk_rs::{
    create_sdk_mcp_server, query, tool, ClaudeAgentOptions, Client, McpServers, Message,
    PermissionMode, ToolsConfig,
};

/// Locates `claude` on `PATH` (matching the SDK's own discovery), or `None`.
fn find_claude() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "claude.exe" } else { "claude" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(exe))
            .find(|p| p.is_file())
    })
}

/// Returns `true` (and prints why) if the live tests should be skipped.
fn should_skip(test: &str) -> bool {
    if std::env::var_os("CLAUDE_SDK_SKIP_LIVE_TESTS").is_some() {
        eprintln!("skipping {test}: CLAUDE_SDK_SKIP_LIVE_TESTS is set");
        return true;
    }
    if find_claude().is_none() {
        eprintln!("skipping {test}: `claude` not found on PATH");
        return true;
    }
    false
}

/// Options for a cheap, deterministic, tool-free text reply.
fn text_only_options() -> ClaudeAgentOptions {
    ClaudeAgentOptions {
        // Empty list disables all built-in tools: a pure text turn, no tool
        // execution, no permission prompts.
        tools: Some(ToolsConfig::List(vec![])),
        max_turns: Some(1),
        ..Default::default()
    }
}

#[tokio::test]
async fn live_query_one_shot() {
    if should_skip("live_query_one_shot") {
        return;
    }

    let fut = async {
        let mut stream = query("Reply with exactly the word: pong", text_only_options())
            .await
            .expect("query() should start against the real CLI");

        let mut saw_assistant = false;
        let mut result = None;
        while let Some(item) = stream.next().await {
            match item.expect("no stream error") {
                Message::Assistant(_) => saw_assistant = true,
                Message::Result(r) => {
                    result = Some(r);
                    break;
                }
                _ => {}
            }
        }
        (saw_assistant, result)
    };

    let (saw_assistant, result) = tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .expect("live query timed out");

    assert!(saw_assistant, "expected at least one assistant message");
    let result = result.expect("expected a result message");
    assert!(!result.is_error, "result was an error: {result:?}");
}

#[tokio::test]
async fn live_client_interactive() {
    if should_skip("live_client_interactive") {
        return;
    }

    let fut = async {
        let mut client = Client::new(text_only_options());
        client.connect(None).await.expect("connect to real CLI");

        // Server info was captured during the initialize handshake.
        assert!(
            client.get_server_info().is_some(),
            "expected initialize response from the CLI"
        );

        let mut stream = client.messages();
        client
            .query("Reply with exactly the word: pong", "default")
            .await
            .expect("send query");

        let mut result = None;
        while let Some(item) = stream.next().await {
            if let Message::Result(r) = item.expect("no stream error") {
                result = Some(r);
                break;
            }
        }
        drop(stream);
        client.disconnect().await.expect("disconnect");
        result
    };

    let result = tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .expect("live client timed out");

    let result = result.expect("expected a result message");
    assert!(!result.is_error, "result was an error: {result:?}");
}

/// Drains a client message stream until a `ResultMessage`, returning it.
async fn drain_to_result(stream: &mut claude_agent_sdk_rs::MessageStream) -> claude_agent_sdk_rs::types::ResultMessage {
    while let Some(item) = stream.next().await {
        if let Message::Result(r) = item.expect("no stream error") {
            return r;
        }
    }
    panic!("stream ended before a result message");
}

#[tokio::test]
async fn live_control_methods_against_real_cli() {
    if should_skip("live_control_methods_against_real_cli") {
        return;
    }

    let fut = async {
        let mut client = Client::new(text_only_options());
        client.connect(None).await.expect("connect");

        let mut stream = client.messages();
        client.query("Say hi in one word.", "default").await.expect("query");
        let result = drain_to_result(&mut stream).await;
        assert!(!result.is_error, "result errored: {result:?}");

        // These validate our typed structs against the REAL CLI wire format.
        let usage = client.get_context_usage().await.expect("get_context_usage");
        assert!(!usage.model.is_empty(), "context usage should name a model");
        assert!(usage.max_tokens > 0, "context usage should report a max");

        let status = client.get_mcp_status().await.expect("get_mcp_status");
        // No servers configured -> empty list, but the call + deserialize must work.
        let _ = status.mcp_servers.len();

        client
            .set_permission_mode(PermissionMode::AcceptEdits)
            .await
            .expect("set_permission_mode");
        client.set_model(None).await.expect("set_model");

        drop(stream);
        client.disconnect().await.expect("disconnect");
    };

    tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .expect("live control methods timed out");
}

#[tokio::test]
async fn live_sdk_mcp_tool_is_called_by_model() {
    if should_skip("live_sdk_mcp_tool_is_called_by_model") {
        return;
    }

    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let greet = tool(
        "greet",
        "Return a friendly greeting for the given name.",
        json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
        move |args| {
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("world");
                Ok(json!({"content": [{"type": "text", "text": format!("Hello, {name}!")}]}))
            }
        },
    );
    let mut servers = HashMap::new();
    servers.insert("greeter".to_string(), create_sdk_mcp_server("greeter", "1.0.0", vec![greet]));
    let options = ClaudeAgentOptions {
        mcp_servers: McpServers::Map(servers),
        // Auto-approve the MCP tool so no permission prompt is needed.
        allowed_tools: vec!["mcp__greeter__greet".to_string()],
        max_turns: Some(4),
        ..Default::default()
    };

    let fut = async {
        let mut stream = query(
            "Call the mcp__greeter__greet tool with name set to \"World\", then tell me what it returned.",
            options,
        )
        .await
        .expect("query starts");
        let mut result = None;
        while let Some(item) = stream.next().await {
            if let Message::Result(r) = item.expect("no stream error") {
                result = Some(r);
                break;
            }
        }
        result
    };

    let result = tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .expect("live mcp tool call timed out")
        .expect("expected a result message");
    assert!(!result.is_error, "result errored: {result:?}");
    assert!(
        called.load(Ordering::SeqCst),
        "the model did not call the in-process greet tool"
    );
}
