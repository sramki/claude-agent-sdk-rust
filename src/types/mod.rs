//! Core SDK types.
//!
//! A faithful, idiomatic Rust port of the Python `claude_agent_sdk/types.py`.
//! Organized into submodules by concern; everything is re-exported from this
//! module root so callers can `use claude_agent_sdk::types::*` (or the crate
//! root re-exports).

use std::future::Future;
use std::pin::Pin;

/// A boxed, `Send` future — the return shape of the SDK's async callbacks
/// (`CanUseTool`, `HookCallback`). Mirrors Python's `Awaitable[...]`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub mod config;
pub mod content;
pub mod hook;
pub mod mcp;
pub mod message;
pub mod options;
pub mod permission;
pub mod sandbox;
pub mod session_info;
pub mod store;

// Session reader types (used across the crate's reader modules).
pub use session_info::{MessageType, SessionInfo, SessionMessage};

// Content blocks.
pub use content::{
    ContentBlock, ServerToolName, ServerToolResultBlock, ServerToolUseBlock, TextBlock,
    ThinkingBlock, ToolResultBlock, ToolResultContent, ToolUseBlock,
};

// Messages.
pub use message::{
    AssistantMessage, AssistantMessageError, DeferredToolUse, HookEventMessage, Message,
    MirrorErrorMessage, RateLimitEvent, RateLimitInfo, RateLimitStatus, RateLimitType,
    ResultMessage, StreamEvent, SystemMessage, SystemMessageKind, TaskNotificationMessage,
    TaskNotificationStatus, TaskProgressMessage, TaskStartedMessage, TaskUpdatedMessage,
    TaskUpdatedStatus, TaskUsage, UserContent, UserMessage, TERMINAL_TASK_STATUSES,
};

// Permission.
pub use permission::{
    CanUseTool, PermissionBehavior, PermissionMode, PermissionResult, PermissionResultAllow,
    PermissionResultDeny, PermissionRuleValue, PermissionUpdate, PermissionUpdateDestination,
    PermissionUpdateType, ToolPermissionContext,
};

// Hooks.
pub use hook::{
    AsyncHookOutput, HookCallback, HookContext, HookEvent, HookInput, HookJSONOutput,
    HookMatcher, HookSpecificOutput, SyncHookOutput,
};

// MCP.
pub use mcp::{
    ContextUsageCategory, ContextUsageResponse, McpHttpServerConfig, McpServerConfig,
    McpServerConnectionStatus, McpServerInfo, McpServerInstance, McpServerStatus, McpSseServerConfig,
    McpStatusResponse, McpStdioServerConfig, McpToolAnnotations, McpToolInfo,
};

// Config sub-types.
pub use config::{
    AgentDefinition, AgentEffort, AgentMcpServer, EffortLevel, SdkBeta, SdkPluginConfig,
    SettingSource, Skills, SystemPrompt, SystemPromptPreset, TaskBudget, ThinkingConfig,
    ThinkingDisplay, ToolsConfig,
};

// Sandbox.
pub use sandbox::{SandboxIgnoreViolations, SandboxNetworkConfig, SandboxSettings};

// Session store.
pub use store::{
    SessionKey, SessionListSubkeysKey, SessionStore, SessionStoreEntry, SessionStoreFlushMode,
    SessionStoreListEntry, SessionSummaryEntry,
};

// Options.
pub use options::{ClaudeAgentOptions, McpServers, Options, StderrCallback};
