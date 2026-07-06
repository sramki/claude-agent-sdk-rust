//! Message types produced by the runtime.
//!
//! Ported from the message dataclasses in the Python `types.py`. These are the
//! SDK's *output* surface — the [`crate::MessageParser`] builds them from raw
//! CLI JSON (see `_internal/message_parser.py`); they are not deserialized from
//! a single wire tag. The specialized `system` subtypes that upstream models as
//! subclasses of `SystemMessage` are represented here via
//! [`SystemMessage::kind`], while `subtype`/`data` stay populated — matching the
//! upstream invariant that base fields remain set.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::content::ContentBlock;
use super::store::SessionKey;

/// Content of a [`UserMessage`] — a plain string or content blocks. Mirrors
/// Python's `str | list[ContentBlock]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// A plain string prompt.
    Text(String),
    /// Structured content blocks.
    Blocks(Vec<ContentBlock>),
}

/// A user message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    /// The message content.
    pub content: UserContent,
    /// Message uuid, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// Parent tool-use id, if this message is a tool result sidechain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
    /// Raw tool-use result payload, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Map<String, Value>>,
}

/// Error classifications an assistant message may carry. Mirrors
/// `AssistantMessageError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageError {
    /// Authentication failed.
    AuthenticationFailed,
    /// Billing error.
    BillingError,
    /// Rate limited.
    RateLimit,
    /// Invalid request.
    InvalidRequest,
    /// Server error.
    ServerError,
    /// Unknown error — also the fallback for any error string the SDK does not
    /// recognize, so a present error is never silently dropped.
    #[serde(other)]
    Unknown,
}

/// An assistant message with content blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// The assistant's content blocks.
    pub content: Vec<ContentBlock>,
    /// Model that produced the message.
    pub model: String,
    /// Parent tool-use id, if produced within a sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
    /// Error classification, if the turn errored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AssistantMessageError>,
    /// Raw usage stats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Map<String, Value>>,
    /// API message id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Stop reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Message uuid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// A tool use deferred by a `PreToolUse` hook returning `"defer"`. Mirrors
/// `DeferredToolUse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeferredToolUse {
    /// Tool-use id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool input.
    pub input: Map<String, Value>,
}

/// Usage statistics reported in task progress/notification messages. Mirrors
/// `TaskUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUsage {
    /// Total tokens used.
    pub total_tokens: u64,
    /// Number of tool uses.
    pub tool_uses: u64,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

/// Terminal/typed status for a `task_notification` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskNotificationStatus {
    /// Completed successfully.
    Completed,
    /// Failed.
    Failed,
    /// Stopped (the CLI's mapped form of a killed task).
    Stopped,
}

/// Status reported inside a `task_updated` patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskUpdatedStatus {
    /// Pending (non-terminal).
    Pending,
    /// Running (non-terminal).
    Running,
    /// Paused (non-terminal).
    Paused,
    /// Completed (terminal).
    Completed,
    /// Failed (terminal).
    Failed,
    /// Killed (terminal).
    Killed,
}

/// Task statuses (string form) that mean the task has finished. Mirrors
/// `TERMINAL_TASK_STATUSES`, spanning both `task_notification` (`stopped`) and
/// `task_updated` (`killed`) vocabularies.
pub const TERMINAL_TASK_STATUSES: [&str; 4] = ["completed", "failed", "stopped", "killed"];

/// System message emitted when a task starts. Mirrors `TaskStartedMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStartedMessage {
    /// Task id.
    pub task_id: String,
    /// Task description.
    pub description: String,
    /// Message uuid.
    pub uuid: String,
    /// Session id.
    pub session_id: String,
    /// Tool-use id that spawned the task, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Task type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
}

/// System message emitted while a task is in progress. Mirrors
/// `TaskProgressMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProgressMessage {
    /// Task id.
    pub task_id: String,
    /// Task description.
    pub description: String,
    /// Usage stats.
    pub usage: TaskUsage,
    /// Message uuid.
    pub uuid: String,
    /// Session id.
    pub session_id: String,
    /// Tool-use id, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Last tool name run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_name: Option<String>,
}

/// System message emitted when a task completes, fails, or is stopped. Mirrors
/// `TaskNotificationMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskNotificationMessage {
    /// Task id.
    pub task_id: String,
    /// Terminal status.
    pub status: TaskNotificationStatus,
    /// Output file path.
    pub output_file: String,
    /// Summary text.
    pub summary: String,
    /// Message uuid.
    pub uuid: String,
    /// Session id.
    pub session_id: String,
    /// Tool-use id, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Usage stats, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TaskUsage>,
}

/// System message emitted when a background task's state changes. Mirrors
/// `TaskUpdatedMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskUpdatedMessage {
    /// Task id.
    pub task_id: String,
    /// The changed fields.
    pub patch: Map<String, Value>,
    /// New status, if present in the patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskUpdatedStatus>,
    /// Session id, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Message uuid, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// System message emitted when a `SessionStore::append` call fails. Mirrors
/// `MirrorErrorMessage`. Non-fatal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorErrorMessage {
    /// The session key whose mirror failed, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<SessionKey>,
    /// The error string.
    pub error: String,
}

/// Hook lifecycle event, emitted when `include_hook_events` is enabled. Mirrors
/// `HookEventMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEventMessage {
    /// Hook event name (e.g. `"PreToolUse"`).
    pub hook_event_name: String,
    /// Session id, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Message uuid, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// The recognized, typed forms of a `system` message. Upstream models these as
/// subclasses of `SystemMessage`; here they are an enum carried on
/// [`SystemMessage::kind`] while `subtype`/`data` stay populated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SystemMessageKind {
    /// A `task_started` system message.
    TaskStarted(TaskStartedMessage),
    /// A `task_progress` system message.
    TaskProgress(TaskProgressMessage),
    /// A `task_notification` system message.
    TaskNotification(TaskNotificationMessage),
    /// A `task_updated` system message.
    TaskUpdated(TaskUpdatedMessage),
    /// A `mirror_error` system message.
    MirrorError(MirrorErrorMessage),
    /// A hook lifecycle event (`hook_started` / `hook_response`).
    HookEvent(HookEventMessage),
}

/// A system message with metadata. Mirrors `SystemMessage` (and, via [`kind`],
/// its typed subclasses).
///
/// [`kind`]: SystemMessage::kind
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemMessage {
    /// The subtype discriminator (e.g. `"init"`, `"task_started"`).
    pub subtype: String,
    /// The raw payload.
    pub data: Map<String, Value>,
    /// The typed view, when the subtype is recognized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SystemMessageKind>,
}

/// A result message with cost and usage information. Mirrors `ResultMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultMessage {
    /// Result subtype.
    pub subtype: String,
    /// Wall-clock duration in ms.
    pub duration_ms: i64,
    /// API duration in ms.
    pub duration_api_ms: i64,
    /// Whether the turn errored.
    pub is_error: bool,
    /// Number of turns.
    pub num_turns: i64,
    /// Session id.
    pub session_id: String,
    /// Stop reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Total cost in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    /// Raw usage stats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Map<String, Value>>,
    /// Final result text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Structured output, if `output_format` was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<Value>,
    /// Per-model usage breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_usage: Option<Map<String, Value>>,
    /// Permission denials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_denials: Option<Vec<Value>>,
    /// Deferred tool use, if the run stopped on a `defer` decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_tool_use: Option<DeferredToolUse>,
    /// Error strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
    /// HTTP status code of the failing API call, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<i64>,
    /// Message uuid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

/// A stream event for partial message updates during streaming. Mirrors
/// `StreamEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Message uuid.
    pub uuid: String,
    /// Session id.
    pub session_id: String,
    /// The raw Anthropic API stream event.
    pub event: Map<String, Value>,
    /// Parent tool-use id, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_use_id: Option<String>,
}

/// Rate limit status. Mirrors `RateLimitStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitStatus {
    /// Allowed.
    Allowed,
    /// Approaching the limit.
    AllowedWarning,
    /// Limit hit.
    Rejected,
}

/// Which rate limit window applies. Mirrors `RateLimitType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitType {
    /// Five-hour window.
    FiveHour,
    /// Seven-day window.
    SevenDay,
    /// Seven-day Opus window.
    SevenDayOpus,
    /// Seven-day Sonnet window.
    SevenDaySonnet,
    /// Overage / pay-as-you-go.
    Overage,
}

/// Rate limit status emitted by the CLI when state changes. Mirrors
/// `RateLimitInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitInfo {
    /// Current status.
    pub status: RateLimitStatus,
    /// Unix timestamp when the window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// Which window applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_type: Option<RateLimitType>,
    /// Fraction of the limit consumed (0.0–1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization: Option<f64>,
    /// Overage status, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_status: Option<RateLimitStatus>,
    /// Unix timestamp when the overage window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_resets_at: Option<i64>,
    /// Why overage is unavailable, if rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_disabled_reason: Option<String>,
    /// Full raw dict from the CLI.
    #[serde(default)]
    pub raw: Map<String, Value>,
}

/// Rate limit event emitted when rate limit info changes. Mirrors
/// `RateLimitEvent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitEvent {
    /// The rate limit info.
    pub rate_limit_info: RateLimitInfo,
    /// Message uuid.
    pub uuid: String,
    /// Session id.
    pub session_id: String,
}

/// A message emitted by the runtime. Mirrors the Python `Message` union
/// (`UserMessage | AssistantMessage | SystemMessage | ResultMessage |
/// StreamEvent | RateLimitEvent`).
///
/// Variants differ in size (a `ResultMessage` carries several optional maps);
/// this is a user-facing output enum matched by value, so the variants are
/// intentionally unboxed for ergonomics.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A user message.
    User(UserMessage),
    /// An assistant message.
    Assistant(AssistantMessage),
    /// A system message (possibly a typed [`SystemMessageKind`]).
    System(SystemMessage),
    /// A result message.
    Result(ResultMessage),
    /// A partial-streaming event.
    StreamEvent(StreamEvent),
    /// A rate limit event.
    RateLimit(RateLimitEvent),
}
