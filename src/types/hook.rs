//! Hook types and the hook callback.
//!
//! Ported from the hook section of the Python `types.py`. [`HookInput`] is a
//! discriminated union keyed on `hook_event_name` (mirroring the per-event
//! `*HookInput` TypedDicts); [`HookJSONOutput`] serializes back to the wire with
//! the upstream keyword fixups (`continue_` → `"continue"`, `async_` →
//! `"async"`). Event-specific output payloads are carried as raw JSON on
//! [`SyncHookOutput::hook_specific_output`] with constructor helpers for the
//! common cases.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::BoxFuture;
use crate::error::Result;

/// A hook event name. Mirrors `HookEvent`. Variant identifiers match the wire
/// values exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Before a tool runs.
    PreToolUse,
    /// After a tool runs.
    PostToolUse,
    /// After a tool fails.
    PostToolUseFailure,
    /// When the user submits a prompt.
    UserPromptSubmit,
    /// When the main loop stops.
    Stop,
    /// When a sub-agent stops.
    SubagentStop,
    /// Before a compaction.
    PreCompact,
    /// On a notification.
    Notification,
    /// When a sub-agent starts.
    SubagentStart,
    /// On a permission request.
    PermissionRequest,
}

/// Strongly-typed input delivered to a hook callback. Mirrors the `HookInput`
/// union. The common fields (`session_id`, `transcript_path`, `cwd`,
/// `permission_mode`) plus any event-specific fields not modeled explicitly are
/// preserved in [`extra`](HookInput::extra).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "hook_event_name")]
pub enum HookInput {
    /// `PreToolUse` input.
    PreToolUse {
        /// Tool name.
        tool_name: String,
        /// Tool input.
        tool_input: Map<String, Value>,
        /// Tool-use id.
        tool_use_id: String,
        /// Other fields (session_id, cwd, agent_id, ...).
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `PostToolUse` input.
    PostToolUse {
        /// Tool name.
        tool_name: String,
        /// Tool input.
        tool_input: Map<String, Value>,
        /// Tool response.
        tool_response: Value,
        /// Tool-use id.
        tool_use_id: String,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `PostToolUseFailure` input.
    PostToolUseFailure {
        /// Tool name.
        tool_name: String,
        /// Tool input.
        tool_input: Map<String, Value>,
        /// Tool-use id.
        tool_use_id: String,
        /// Error text.
        error: String,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `UserPromptSubmit` input.
    UserPromptSubmit {
        /// The submitted prompt.
        prompt: String,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `Stop` input.
    Stop {
        /// Whether a stop hook is already active.
        stop_hook_active: bool,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `SubagentStop` input.
    SubagentStop {
        /// Whether a stop hook is already active.
        stop_hook_active: bool,
        /// Sub-agent id.
        agent_id: String,
        /// Sub-agent transcript path.
        agent_transcript_path: String,
        /// Sub-agent type.
        agent_type: String,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `PreCompact` input.
    PreCompact {
        /// Trigger (`manual` or `auto`).
        trigger: String,
        /// Custom instructions, if any.
        custom_instructions: Option<String>,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `Notification` input.
    Notification {
        /// The message.
        message: String,
        /// Notification type.
        notification_type: String,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `SubagentStart` input.
    SubagentStart {
        /// Sub-agent id.
        agent_id: String,
        /// Sub-agent type.
        agent_type: String,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `PermissionRequest` input.
    PermissionRequest {
        /// Tool name.
        tool_name: String,
        /// Tool input.
        tool_input: Map<String, Value>,
        /// Other fields.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
}

/// Context passed to a hook callback. Mirrors `HookContext` (the `signal`
/// abort-support field is reserved for future use).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookContext {
    // Reserved for future abort-signal support.
}

/// Event-specific output payload for a hook. Serialized under
/// `hookSpecificOutput`; the caller may also construct one directly as raw
/// JSON via [`SyncHookOutput::hook_specific_output`].
pub type HookSpecificOutput = Value;

/// Synchronous hook output. Mirrors `SyncHookJSONOutput`. Field names are fixed
/// up to the wire form on [`to_wire`](Self::to_wire).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncHookOutput {
    /// Whether Claude should proceed after the hook (wire: `continue`).
    pub continue_: Option<bool>,
    /// Hide stdout from transcript mode.
    pub suppress_output: Option<bool>,
    /// Message shown when `continue_` is `false`.
    pub stop_reason: Option<String>,
    /// Set to `"block"` for blocking behavior.
    pub decision: Option<String>,
    /// Warning message displayed to the user.
    pub system_message: Option<String>,
    /// Feedback message for Claude.
    pub reason: Option<String>,
    /// Event-specific output (e.g. `{"hookEventName": "PreToolUse",
    /// "permissionDecision": "deny", ...}`).
    pub hook_specific_output: Option<HookSpecificOutput>,
}

impl SyncHookOutput {
    /// Serializes to the wire dict, applying the keyword fixups.
    pub fn to_wire(&self) -> Value {
        let mut o = Map::new();
        if let Some(c) = self.continue_ {
            o.insert("continue".into(), json!(c));
        }
        if let Some(s) = self.suppress_output {
            o.insert("suppressOutput".into(), json!(s));
        }
        if let Some(s) = &self.stop_reason {
            o.insert("stopReason".into(), json!(s));
        }
        if let Some(d) = &self.decision {
            o.insert("decision".into(), json!(d));
        }
        if let Some(m) = &self.system_message {
            o.insert("systemMessage".into(), json!(m));
        }
        if let Some(r) = &self.reason {
            o.insert("reason".into(), json!(r));
        }
        if let Some(h) = &self.hook_specific_output {
            o.insert("hookSpecificOutput".into(), h.clone());
        }
        Value::Object(o)
    }
}

/// Asynchronous hook output that defers execution. Mirrors
/// `AsyncHookJSONOutput`.
#[derive(Debug, Clone, PartialEq)]
pub struct AsyncHookOutput {
    /// Optional timeout in milliseconds.
    pub async_timeout: Option<i64>,
}

impl AsyncHookOutput {
    /// Serializes to the wire dict (`{"async": true, ...}`).
    pub fn to_wire(&self) -> Value {
        let mut o = Map::new();
        o.insert("async".into(), json!(true));
        if let Some(t) = self.async_timeout {
            o.insert("asyncTimeout".into(), json!(t));
        }
        Value::Object(o)
    }
}

/// The output of a hook callback. Mirrors `HookJSONOutput`.
#[derive(Debug, Clone, PartialEq)]
pub enum HookJSONOutput {
    /// Synchronous output.
    Sync(SyncHookOutput),
    /// Async (deferred) output.
    Async(AsyncHookOutput),
}

impl HookJSONOutput {
    /// Serializes to the wire dict.
    pub fn to_wire(&self) -> Value {
        match self {
            HookJSONOutput::Sync(s) => s.to_wire(),
            HookJSONOutput::Async(a) => a.to_wire(),
        }
    }
}

impl Default for HookJSONOutput {
    fn default() -> Self {
        HookJSONOutput::Sync(SyncHookOutput::default())
    }
}

/// A hook callback: `(input, tool_use_id, context) -> HookJSONOutput`. Mirrors
/// `HookCallback`.
pub type HookCallback = Arc<
    dyn Fn(HookInput, Option<String>, HookContext) -> BoxFuture<'static, Result<HookJSONOutput>>
        + Send
        + Sync,
>;

/// A hook matcher configuration. Mirrors `HookMatcher`.
#[derive(Clone)]
pub struct HookMatcher {
    /// Matcher string (e.g. a tool name like `"Bash"` or `"Write|Edit"`).
    pub matcher: Option<String>,
    /// The callbacks to run.
    pub hooks: Vec<HookCallback>,
    /// Timeout in seconds for all hooks in this matcher.
    pub timeout: Option<f64>,
}

impl std::fmt::Debug for HookMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookMatcher")
            .field("matcher", &self.matcher)
            .field("hooks", &format_args!("<{} callbacks>", self.hooks.len()))
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl HookMatcher {
    /// Creates a matcher with the given matcher string and callbacks.
    pub fn new(matcher: Option<String>, hooks: Vec<HookCallback>) -> Self {
        HookMatcher {
            matcher,
            hooks,
            timeout: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_input_pre_tool_use_deserializes() {
        let v = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "transcript_path": "/t",
            "cwd": "/c",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
            "tool_use_id": "tu1",
            "agent_id": "a1"
        });
        let input: HookInput = serde_json::from_value(v).unwrap();
        match input {
            HookInput::PreToolUse {
                tool_name,
                tool_use_id,
                extra,
                ..
            } => {
                assert_eq!(tool_name, "Bash");
                assert_eq!(tool_use_id, "tu1");
                assert_eq!(extra.get("session_id").unwrap(), "s1");
                assert_eq!(extra.get("agent_id").unwrap(), "a1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn sync_output_to_wire_fixes_keywords() {
        let out = SyncHookOutput {
            continue_: Some(false),
            stop_reason: Some("stop".into()),
            ..Default::default()
        };
        assert_eq!(
            out.to_wire(),
            json!({"continue": false, "stopReason": "stop"})
        );
    }

    #[test]
    fn async_output_to_wire() {
        let out = AsyncHookOutput {
            async_timeout: Some(5000),
        };
        assert_eq!(out.to_wire(), json!({"async": true, "asyncTimeout": 5000}));
    }
}
