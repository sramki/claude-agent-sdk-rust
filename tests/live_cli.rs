//! End-to-end tests against the **real** `claude` CLI.
//!
//! These validate the transport + control protocol against the actual binary —
//! something the mock-transport tests can't. They are **skipped gracefully**
//! (the test passes, printing a notice) when `claude` is not found on `PATH`,
//! or when `CLAUDE_SDK_SKIP_LIVE_TESTS` is set — so `cargo test` never fails
//! merely because the binary (or credentials) are unavailable. When present,
//! they issue a tiny text-only query (built-in tools disabled) and assert a
//! non-error result.

use std::path::PathBuf;
use std::time::Duration;

use claude_agent_sdk::{query, ClaudeAgentOptions, Client, Message, ToolsConfig};

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
