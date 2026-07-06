//! Parity tests ported from upstream `claude-agent-sdk-python`:
//!   - `tests/test_errors.py`             (error Display / field parity)
//!   - `tests/test_rate_limit_event_repro.py` (rate_limit_event parsing)
//!
//! Faithful to `claude-agent-sdk` Python v0.2.110.
//!
//! Python-only mechanics (exception subclassing / `isinstance` hierarchy) are
//! not ported: the Rust SDK collapses the class tree into a single `Error`
//! enum, so we assert Display strings and fields instead of type identity.
//!
//! `parse_message` here returns `Result<Option<Message>>`; upstream's Python
//! `parse_message` returns `Message | None`. `Ok(None)` maps to Python `None`.

use claude_agent_sdk_rs::{
    parse_message, ContentBlock, Error, Message, RateLimitStatus, RateLimitType,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Ported from test_errors.py
// ---------------------------------------------------------------------------

// Upstream `test_base_error` (`ClaudeSDKError("...")`) has no direct analog:
// the Rust SDK has no bare "base" variant carrying an arbitrary string. The
// closest string-carrying connection variant is exercised by
// `connection_error` below, which mirrors `CLIConnectionError`.

/// Ported from `test_cli_not_found_error`. Upstream asserts the message is
/// contained in `str(error)`; the Rust Display additionally appends the probed
/// path (`"<message>: <path>"`) when known.
#[test]
fn cli_not_found_error() {
    // Without a path (upstream passes only a message).
    let e = Error::cli_not_found(None::<String>);
    assert!(e.to_string().contains("Claude Code not found"));
    assert_eq!(e.to_string(), "Claude Code not found");

    // With a path — Rust-specific formatting the upstream Display also uses.
    let e = Error::cli_not_found(Some("/usr/local/bin/claude"));
    assert_eq!(
        e.to_string(),
        "Claude Code not found: /usr/local/bin/claude"
    );
    match e {
        Error::CliNotFound { cli_path, .. } => {
            assert_eq!(cli_path.as_deref(), Some("/usr/local/bin/claude"));
        }
        other => panic!("expected CliNotFound, got {other:?}"),
    }
}

/// Ported from `test_connection_error`. Mirrors `CLIConnectionError`.
#[test]
fn connection_error() {
    let e = Error::connection("Failed to connect to CLI");
    assert!(e.to_string().contains("Failed to connect to CLI"));
    assert_eq!(e.to_string(), "Failed to connect to CLI");
}

/// Ported from `test_process_error`. Asserts the `exit_code`/`stderr` fields
/// and that all three fragments appear in Display.
#[test]
fn process_error_with_code_and_stderr() {
    let e = Error::process(
        "Process failed",
        Some(1),
        Some("Command not found".to_string()),
    );
    match &e {
        Error::Process {
            exit_code, stderr, ..
        } => {
            assert_eq!(*exit_code, Some(1));
            assert_eq!(stderr.as_deref(), Some("Command not found"));
        }
        other => panic!("expected Process, got {other:?}"),
    }
    let msg = e.to_string();
    assert!(msg.contains("Process failed"));
    assert!(msg.contains("exit code: 1"));
    assert!(msg.contains("Command not found"));
    // Exact upstream format string.
    assert_eq!(
        msg,
        "Process failed (exit code: 1)\nError output: Command not found"
    );
}

/// Complementary: a ProcessError with no optional fields is just the message.
#[test]
fn process_error_message_only() {
    let e = Error::process("Process failed", None, None);
    assert_eq!(e.to_string(), "Process failed");
}

/// Complementary: exit code without stderr.
#[test]
fn process_error_code_only() {
    let e = Error::process("Process failed", Some(2), None);
    assert_eq!(e.to_string(), "Process failed (exit code: 2)");
}

/// Ported from `test_json_decode_error`. Asserts the `line` field is preserved
/// and Display contains the "Failed to decode JSON" prefix.
#[test]
fn json_decode_error() {
    let line = "{invalid json}";
    let source = serde_json::from_str::<serde_json::Value>(line).unwrap_err();
    let e = Error::json_decode(line, source);
    match &e {
        Error::JsonDecode { line: l, .. } => assert_eq!(l, "{invalid json}"),
        other => panic!("expected JsonDecode, got {other:?}"),
    }
    let msg = e.to_string();
    assert!(msg.contains("Failed to decode JSON"));
    // Short lines still get the "..." suffix (upstream: `{line[:100]}...`).
    assert_eq!(msg, "Failed to decode JSON: {invalid json}...");
}

/// Distinct case: the JSON-decode Display truncates the offending line to the
/// first 100 characters (upstream `line[:100]`).
#[test]
fn json_decode_error_truncates_to_100_chars() {
    let line = "y".repeat(250);
    let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let e = Error::json_decode(line, source);
    let msg = e.to_string();
    assert!(msg.starts_with("Failed to decode JSON: "));
    assert!(msg.ends_with("..."));
    assert_eq!(msg.matches('y').count(), 100);
}

// ---------------------------------------------------------------------------
// Ported from test_rate_limit_event_repro.py
// ---------------------------------------------------------------------------

/// Ported from `test_rate_limit_event_parsed_as_typed_message`. `allowed_warning`
/// status with all optional modeled fields, plus an unmodeled field preserved
/// in `raw`. Note the wire uses camelCase inside `rate_limit_info`.
#[test]
fn rate_limit_event_parsed_as_typed_message() {
    let data = json!({
        "type": "rate_limit_event",
        "rate_limit_info": {
            "status": "allowed_warning",
            "resetsAt": 1_700_000_000i64,
            "rateLimitType": "five_hour",
            "utilization": 0.85,
            "isUsingOverage": false,
        },
        "uuid": "550e8400-e29b-41d4-a716-446655440000",
        "session_id": "test-session-id",
    });

    let Message::RateLimit(event) = parse_message(&data).unwrap().unwrap() else {
        panic!("expected RateLimit message");
    };
    assert_eq!(event.uuid, "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(event.session_id, "test-session-id");

    let info = event.rate_limit_info;
    assert_eq!(info.status, RateLimitStatus::AllowedWarning);
    assert_eq!(info.resets_at, Some(1_700_000_000));
    assert_eq!(info.rate_limit_type, Some(RateLimitType::FiveHour));
    assert_eq!(info.utilization, Some(0.85));
    // Unmodeled field preserved in raw.
    assert_eq!(info.raw.get("isUsingOverage"), Some(&json!(false)));
    // raw preserves the whole info object, including modeled fields.
    assert_eq!(info.raw.get("status"), Some(&json!("allowed_warning")));
    assert_eq!(info.raw.get("resetsAt"), Some(&json!(1_700_000_000i64)));
}

/// Ported from `test_rate_limit_event_rejected_parsed`. Hard rate limit with
/// overage info.
#[test]
fn rate_limit_event_rejected_parsed() {
    let data = json!({
        "type": "rate_limit_event",
        "rate_limit_info": {
            "status": "rejected",
            "resetsAt": 1_700_003_600i64,
            "rateLimitType": "seven_day",
            "isUsingOverage": false,
            "overageStatus": "rejected",
            "overageDisabledReason": "out_of_credits",
        },
        "uuid": "660e8400-e29b-41d4-a716-446655440001",
        "session_id": "test-session-id",
    });

    let Message::RateLimit(event) = parse_message(&data).unwrap().unwrap() else {
        panic!("expected RateLimit message");
    };
    let info = event.rate_limit_info;
    assert_eq!(info.status, RateLimitStatus::Rejected);
    assert_eq!(info.rate_limit_type, Some(RateLimitType::SevenDay));
    assert_eq!(info.overage_status, Some(RateLimitStatus::Rejected));
    assert_eq!(
        info.overage_disabled_reason.as_deref(),
        Some("out_of_credits")
    );
}

/// Ported from `test_rate_limit_event_minimal_fields`. Only `status` is
/// required; optional fields default to `None`.
#[test]
fn rate_limit_event_minimal_fields() {
    let data = json!({
        "type": "rate_limit_event",
        "rate_limit_info": { "status": "allowed" },
        "uuid": "770e8400-e29b-41d4-a716-446655440002",
        "session_id": "test-session-id",
    });

    let Message::RateLimit(event) = parse_message(&data).unwrap().unwrap() else {
        panic!("expected RateLimit message");
    };
    let info = event.rate_limit_info;
    assert_eq!(info.status, RateLimitStatus::Allowed);
    assert_eq!(info.resets_at, None);
    assert_eq!(info.rate_limit_type, None);
    assert_eq!(info.utilization, None);
    assert_eq!(info.overage_status, None);
    assert_eq!(info.overage_resets_at, None);
    assert_eq!(info.overage_disabled_reason, None);
}

/// Complementary coverage of the remaining `RateLimitType` variants
/// (`seven_day_opus`, `seven_day_sonnet`, `overage`) and `overageResetsAt`,
/// none of which the upstream cases exercise directly.
#[test]
fn rate_limit_event_extra_type_variants() {
    for (wire, expected) in [
        ("seven_day_opus", RateLimitType::SevenDayOpus),
        ("seven_day_sonnet", RateLimitType::SevenDaySonnet),
        ("overage", RateLimitType::Overage),
    ] {
        let data = json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "allowed",
                "rateLimitType": wire,
                "overageStatus": "allowed_warning",
                "overageResetsAt": 1_700_009_999i64,
            },
            "uuid": "990e8400-e29b-41d4-a716-446655440004",
            "session_id": "test-session-id",
        });

        let Message::RateLimit(event) = parse_message(&data).unwrap().unwrap() else {
            panic!("expected RateLimit message for {wire}");
        };
        let info = event.rate_limit_info;
        assert_eq!(info.rate_limit_type, Some(expected));
        assert_eq!(info.overage_status, Some(RateLimitStatus::AllowedWarning));
        assert_eq!(info.overage_resets_at, Some(1_700_009_999));
    }
}

/// Ported from `test_unknown_message_type_returns_none`. Unknown message types
/// return `Ok(None)` (Python: `None`) for forward compatibility.
#[test]
fn unknown_message_type_returns_none() {
    let data = json!({
        "type": "some_future_event_type",
        "uuid": "880e8400-e29b-41d4-a716-446655440003",
        "session_id": "test-session-id",
    });
    assert!(parse_message(&data).unwrap().is_none());
}

/// Ported from `test_known_message_types_still_parsed`. Known types still parse
/// normally after the rate_limit_event handler was added.
#[test]
fn known_message_types_still_parsed() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [{ "type": "text", "text": "hello" }],
            "model": "claude-sonnet-4-6-20250929",
        },
    });

    let Message::Assistant(msg) = parse_message(&data).unwrap().unwrap() else {
        panic!("expected Assistant message");
    };
    let ContentBlock::Text(block) = &msg.content[0] else {
        panic!("expected a text block");
    };
    assert_eq!(block.text, "hello");
}
