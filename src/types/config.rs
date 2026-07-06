//! Configuration sub-types used by [`ClaudeAgentOptions`](super::ClaudeAgentOptions).
//!
//! Ported from the many small config types in the Python `types.py`:
//! `SettingSource`, `EffortLevel`, `SdkBeta`, the system-prompt / tools presets,
//! `ThinkingConfig`, `TaskBudget`, `SdkPluginConfig`, and `AgentDefinition`.
//! Wire field names are camelCase where the CLI expects it.

use serde::{Deserialize, Serialize};

/// Which filesystem settings sources to load. Mirrors `SettingSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingSource {
    /// Global user settings.
    User,
    /// Project settings.
    Project,
    /// Local settings.
    Local,
}

/// Effort level guiding thinking depth. Mirrors `EffortLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    /// Minimal thinking, fastest.
    Low,
    /// Moderate thinking.
    Medium,
    /// Deep reasoning (default).
    High,
    /// Extended reasoning depth (Opus only).
    Xhigh,
    /// Maximum effort.
    Max,
}

/// Beta feature flags. Mirrors `SdkBeta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SdkBeta {
    /// 1M token context window (Sonnet 4/4.5 only).
    #[serde(rename = "context-1m-2025-08-07")]
    Context1m20250807,
}

/// System prompt preset. Mirrors `SystemPromptPreset`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPromptPreset {
    /// Preset name (always `claude_code`).
    pub preset: String,
    /// Text appended to the preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<String>,
    /// Strip per-user dynamic sections for cross-user cacheability.
    #[serde(
        rename = "exclude_dynamic_sections",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub exclude_dynamic_sections: Option<bool>,
}

/// System prompt configuration. Mirrors `str | SystemPromptPreset |
/// SystemPromptFile | None` (the `None` case is the enclosing `Option`).
#[derive(Debug, Clone, PartialEq)]
pub enum SystemPrompt {
    /// A custom system prompt string.
    Text(String),
    /// Claude Code's default preset (optionally with appended instructions).
    Preset(SystemPromptPreset),
    /// Load the system prompt from a file path.
    File(String),
}

/// Base tool set configuration. Mirrors `list[str] | ToolsPreset | None`.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolsConfig {
    /// Specific built-in tool names (empty disables all built-ins).
    List(Vec<String>),
    /// All default Claude Code tools (`{"type": "preset", "preset":
    /// "claude_code"}`).
    Preset,
}

/// API-side task budget in tokens. Mirrors `TaskBudget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBudget {
    /// Total token budget.
    pub total: i64,
}

/// Whether thinking text is summarized or omitted. Mirrors `ThinkingDisplay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    /// Return summarized thinking text.
    Summarized,
    /// Omit thinking text (signature only).
    Omitted,
}

/// Thinking/reasoning configuration. Mirrors `ThinkingConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThinkingConfig {
    /// Adaptive — Claude decides when and how much to think.
    Adaptive {
        /// Optional display mode.
        display: Option<ThinkingDisplay>,
    },
    /// Enabled with a fixed token budget.
    Enabled {
        /// Thinking token budget.
        budget_tokens: i64,
        /// Optional display mode.
        display: Option<ThinkingDisplay>,
    },
    /// Disabled — no extended thinking.
    Disabled,
}

impl ThinkingConfig {
    /// Serializes to the wire dict form.
    pub fn to_wire(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            ThinkingConfig::Adaptive { display } => {
                let mut v = json!({"type": "adaptive"});
                if let Some(d) = display {
                    v["display"] = serde_json::to_value(d).unwrap();
                }
                v
            }
            ThinkingConfig::Enabled {
                budget_tokens,
                display,
            } => {
                let mut v = json!({"type": "enabled", "budget_tokens": budget_tokens});
                if let Some(d) = display {
                    v["display"] = serde_json::to_value(d).unwrap();
                }
                v
            }
            ThinkingConfig::Disabled => json!({"type": "disabled"}),
        }
    }
}

/// SDK plugin configuration. Mirrors `SdkPluginConfig` (only local plugins).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SdkPluginConfig {
    /// Plugin type (always `local`).
    #[serde(rename = "type")]
    pub plugin_type: String,
    /// Path to the plugin.
    pub path: String,
}

impl SdkPluginConfig {
    /// Constructs a local plugin config from a path.
    pub fn local(path: impl Into<String>) -> Self {
        SdkPluginConfig {
            plugin_type: "local".into(),
            path: path.into(),
        }
    }
}

/// Skills to enable. Mirrors `list[str] | Literal["all"] | None` (the `None`
/// case is the enclosing `Option`).
#[derive(Debug, Clone, PartialEq)]
pub enum Skills {
    /// Enable every discovered skill.
    All,
    /// Enable only the listed skills.
    List(Vec<String>),
}

/// Effort setting for an agent — either a named level or a raw integer. Mirrors
/// `EffortLevel | int`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentEffort {
    /// A named effort level.
    Level(EffortLevel),
    /// A raw integer effort value.
    Int(i64),
}

/// An MCP server reference in an [`AgentDefinition`] — a server name or an
/// inline config object. Mirrors `str | dict[str, Any]`.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMcpServer {
    /// Reference an existing server by name.
    Name(String),
    /// An inline `{name: config}` object.
    Inline(serde_json::Map<String, serde_json::Value>),
}

/// A programmatically-defined subagent. Mirrors `AgentDefinition`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDefinition {
    /// Description shown to the model.
    pub description: String,
    /// The agent's system prompt.
    pub prompt: String,
    /// Allowed tools.
    pub tools: Option<Vec<String>>,
    /// Disallowed tools.
    pub disallowed_tools: Option<Vec<String>>,
    /// Model alias or full model id.
    pub model: Option<String>,
    /// Skills to enable.
    pub skills: Option<Vec<String>>,
    /// Memory source.
    pub memory: Option<SettingSource>,
    /// MCP servers (by name or inline).
    pub mcp_servers: Option<Vec<AgentMcpServer>>,
    /// Initial prompt.
    pub initial_prompt: Option<String>,
    /// Max turns.
    pub max_turns: Option<i64>,
    /// Run in the background.
    pub background: Option<bool>,
    /// Effort setting.
    pub effort: Option<AgentEffort>,
    /// Permission mode.
    pub permission_mode: Option<super::PermissionMode>,
}

impl AgentDefinition {
    /// Creates a minimal definition with just a description and prompt.
    pub fn new(description: impl Into<String>, prompt: impl Into<String>) -> Self {
        AgentDefinition {
            description: description.into(),
            prompt: prompt.into(),
            tools: None,
            disallowed_tools: None,
            model: None,
            skills: None,
            memory: None,
            mcp_servers: None,
            initial_prompt: None,
            max_turns: None,
            background: None,
            effort: None,
            permission_mode: None,
        }
    }

    /// Serializes to the wire dict sent in the `initialize` control request.
    /// Only set fields are included; keys use the CLI's camelCase names.
    pub fn to_wire(&self) -> serde_json::Value {
        use serde_json::{json, Map, Value};
        let mut o = Map::new();
        o.insert("description".into(), json!(self.description));
        o.insert("prompt".into(), json!(self.prompt));
        if let Some(t) = &self.tools {
            o.insert("tools".into(), json!(t));
        }
        if let Some(t) = &self.disallowed_tools {
            o.insert("disallowedTools".into(), json!(t));
        }
        if let Some(m) = &self.model {
            o.insert("model".into(), json!(m));
        }
        if let Some(s) = &self.skills {
            o.insert("skills".into(), json!(s));
        }
        if let Some(m) = &self.memory {
            o.insert("memory".into(), serde_json::to_value(m).unwrap());
        }
        if let Some(servers) = &self.mcp_servers {
            let arr: Vec<Value> = servers
                .iter()
                .map(|s| match s {
                    AgentMcpServer::Name(n) => json!(n),
                    AgentMcpServer::Inline(m) => Value::Object(m.clone()),
                })
                .collect();
            o.insert("mcpServers".into(), Value::Array(arr));
        }
        if let Some(p) = &self.initial_prompt {
            o.insert("initialPrompt".into(), json!(p));
        }
        if let Some(n) = self.max_turns {
            o.insert("maxTurns".into(), json!(n));
        }
        if let Some(b) = self.background {
            o.insert("background".into(), json!(b));
        }
        if let Some(e) = &self.effort {
            let ev = match e {
                AgentEffort::Level(l) => serde_json::to_value(l).unwrap(),
                AgentEffort::Int(i) => json!(i),
            };
            o.insert("effort".into(), ev);
        }
        if let Some(pm) = &self.permission_mode {
            o.insert("permissionMode".into(), serde_json::to_value(pm).unwrap());
        }
        Value::Object(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn effort_level_lowercase() {
        assert_eq!(serde_json::to_value(EffortLevel::Xhigh).unwrap(), json!("xhigh"));
    }

    #[test]
    fn beta_flag_wire() {
        assert_eq!(
            serde_json::to_value(SdkBeta::Context1m20250807).unwrap(),
            json!("context-1m-2025-08-07")
        );
    }

    #[test]
    fn thinking_enabled_to_wire() {
        let t = ThinkingConfig::Enabled {
            budget_tokens: 4096,
            display: Some(ThinkingDisplay::Summarized),
        };
        assert_eq!(
            t.to_wire(),
            json!({"type": "enabled", "budget_tokens": 4096, "display": "summarized"})
        );
    }

    #[test]
    fn agent_definition_to_wire_camelcase() {
        let mut a = AgentDefinition::new("A helper", "Be helpful");
        a.disallowed_tools = Some(vec!["Bash".into()]);
        a.max_turns = Some(3);
        a.permission_mode = Some(super::super::PermissionMode::Plan);
        let w = a.to_wire();
        assert_eq!(w["disallowedTools"], json!(["Bash"]));
        assert_eq!(w["maxTurns"], json!(3));
        assert_eq!(w["permissionMode"], json!("plan"));
    }
}
