//! [`ClaudeAgentOptions`] — the query configuration surface.
//!
//! Ported from the `ClaudeAgentOptions` dataclass in the Python `types.py`.
//! Fields are public and constructed via struct-update syntax over
//! [`Default`], mirroring the dataclass's keyword defaults. Callback fields hold
//! `Arc`-wrapped closures, so the struct is `Clone` but not `Debug`/`PartialEq`
//! (a manual [`Debug`] impl elides the callbacks).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};

use super::config::{
    AgentDefinition, EffortLevel, SdkBeta, SdkPluginConfig, SettingSource, Skills, SystemPrompt,
    TaskBudget, ThinkingConfig, ToolsConfig,
};
use super::hook::{HookEvent, HookMatcher};
use super::mcp::McpServerConfig;
use super::permission::{CanUseTool, PermissionMode};
use super::sandbox::SandboxSettings;
use super::store::{SessionStore, SessionStoreFlushMode};

/// Callback for stderr output from the subprocess. Mirrors `Callable[[str],
/// None]`.
pub type StderrCallback = Arc<dyn Fn(String) + Send + Sync>;

/// MCP server configuration source. Mirrors `dict[str, McpServerConfig] | str |
/// Path` — either an inline map of named servers or a path to an MCP config
/// JSON file.
#[derive(Debug, Clone, Default)]
pub enum McpServers {
    /// Inline named server configs.
    Map(HashMap<String, McpServerConfig>),
    /// Path to an MCP config JSON file.
    Path(PathBuf),
    /// No MCP servers (default).
    #[default]
    None,
}

/// Query options for the SDK. Mirrors `ClaudeAgentOptions`.
#[derive(Clone)]
pub struct ClaudeAgentOptions {
    /// Base set of available built-in tools.
    pub tools: Option<ToolsConfig>,
    /// Tool names auto-allowed without prompting.
    pub allowed_tools: Vec<String>,
    /// System prompt configuration.
    pub system_prompt: Option<SystemPrompt>,
    /// MCP server configurations.
    pub mcp_servers: McpServers,
    /// Only use MCP servers passed via [`mcp_servers`](Self::mcp_servers).
    pub strict_mcp_config: bool,
    /// Permission mode for the session.
    pub permission_mode: Option<PermissionMode>,
    /// Continue the most recent conversation in the cwd.
    pub continue_conversation: bool,
    /// Session id to resume.
    pub resume: Option<String>,
    /// Use a specific session id (must be a valid UUID).
    pub session_id: Option<String>,
    /// Maximum number of conversation turns.
    pub max_turns: Option<i64>,
    /// Maximum budget in USD.
    pub max_budget_usd: Option<f64>,
    /// Disallowed tool names.
    pub disallowed_tools: Vec<String>,
    /// Model to use.
    pub model: Option<String>,
    /// Fallback model.
    pub fallback_model: Option<String>,
    /// Enabled beta features.
    pub betas: Vec<SdkBeta>,
    /// MCP tool name to route permission prompts through.
    pub permission_prompt_tool_name: Option<String>,
    /// Working directory for the session.
    pub cwd: Option<PathBuf>,
    /// Path to the Claude Code CLI executable.
    pub cli_path: Option<PathBuf>,
    /// Path to an additional settings JSON file.
    pub settings: Option<String>,
    /// Additional accessible directories.
    pub add_dirs: Vec<PathBuf>,
    /// Environment variables for the subprocess.
    pub env: HashMap<String, String>,
    /// Additional CLI arguments (name → optional value; `None` = boolean flag).
    pub extra_args: HashMap<String, Option<String>>,
    /// Maximum bytes to buffer when reading subprocess stdout.
    pub max_buffer_size: Option<usize>,
    /// Callback for subprocess stderr output.
    pub stderr: Option<StderrCallback>,
    /// Custom permission handler for tool calls that would otherwise prompt.
    pub can_use_tool: Option<CanUseTool>,
    /// Hook callbacks for events during execution.
    pub hooks: Option<HashMap<HookEvent, Vec<HookMatcher>>>,
    /// Optional user identifier.
    pub user: Option<String>,
    /// Include partial/streaming message events.
    pub include_partial_messages: bool,
    /// Include hook lifecycle events in the message stream.
    pub include_hook_events: bool,
    /// Fork resumed sessions to a new session id.
    pub fork_session: bool,
    /// Programmatically-defined subagents.
    pub agents: Option<HashMap<String, AgentDefinition>>,
    /// Which filesystem settings sources to load.
    pub setting_sources: Option<Vec<SettingSource>>,
    /// Skills to enable for the main session.
    pub skills: Option<Skills>,
    /// Sandbox settings.
    pub sandbox: Option<SandboxSettings>,
    /// Plugins to load.
    pub plugins: Vec<SdkPluginConfig>,
    /// Deprecated: maximum thinking tokens. Prefer [`thinking`](Self::thinking).
    pub max_thinking_tokens: Option<i64>,
    /// Thinking/reasoning configuration.
    pub thinking: Option<ThinkingConfig>,
    /// Effort level.
    pub effort: Option<EffortLevel>,
    /// Output format for structured responses.
    pub output_format: Option<Map<String, Value>>,
    /// Enable file checkpointing.
    pub enable_file_checkpointing: bool,
    /// Mirror session transcripts to an external store.
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// When to flush mirrored transcript entries.
    pub session_store_flush: SessionStoreFlushMode,
    /// Timeout for each store `load`/`list_subkeys` call during resume, in ms.
    pub load_timeout_ms: i64,
    /// API-side task budget in tokens.
    pub task_budget: Option<TaskBudget>,
}

impl Default for ClaudeAgentOptions {
    fn default() -> Self {
        ClaudeAgentOptions {
            tools: None,
            allowed_tools: Vec::new(),
            system_prompt: None,
            mcp_servers: McpServers::None,
            strict_mcp_config: false,
            permission_mode: None,
            continue_conversation: false,
            resume: None,
            session_id: None,
            max_turns: None,
            max_budget_usd: None,
            disallowed_tools: Vec::new(),
            model: None,
            fallback_model: None,
            betas: Vec::new(),
            permission_prompt_tool_name: None,
            cwd: None,
            cli_path: None,
            settings: None,
            add_dirs: Vec::new(),
            env: HashMap::new(),
            extra_args: HashMap::new(),
            max_buffer_size: None,
            stderr: None,
            can_use_tool: None,
            hooks: None,
            user: None,
            include_partial_messages: false,
            include_hook_events: false,
            fork_session: false,
            agents: None,
            setting_sources: None,
            skills: None,
            sandbox: None,
            plugins: Vec::new(),
            max_thinking_tokens: None,
            thinking: None,
            effort: None,
            output_format: None,
            enable_file_checkpointing: false,
            session_store: None,
            session_store_flush: SessionStoreFlushMode::Batched,
            load_timeout_ms: 60_000,
            task_budget: None,
        }
    }
}

impl std::fmt::Debug for ClaudeAgentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeAgentOptions")
            .field("tools", &self.tools)
            .field("allowed_tools", &self.allowed_tools)
            .field("system_prompt", &self.system_prompt)
            .field("mcp_servers", &self.mcp_servers)
            .field("permission_mode", &self.permission_mode)
            .field("model", &self.model)
            .field("resume", &self.resume)
            .field("session_id", &self.session_id)
            .field("max_turns", &self.max_turns)
            .field("cwd", &self.cwd)
            .field("can_use_tool", &self.can_use_tool.as_ref().map(|_| "<callback>"))
            .field("hooks", &self.hooks.as_ref().map(|h| h.len()))
            .field("stderr", &self.stderr.as_ref().map(|_| "<callback>"))
            .field(
                "session_store",
                &self.session_store.as_ref().map(|_| "<store>"),
            )
            .finish_non_exhaustive()
    }
}

/// Convenience alias for [`ClaudeAgentOptions`].
pub type Options = ClaudeAgentOptions;
