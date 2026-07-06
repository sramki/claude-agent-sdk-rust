//! Permission types and the tool-permission callback.
//!
//! Ported from the permission section of the Python `types.py`
//! (`PermissionMode`, `PermissionUpdate`, `PermissionResult*`,
//! `ToolPermissionContext`, `CanUseTool`). [`PermissionUpdate::to_wire`] /
//! [`PermissionUpdate::from_wire`] mirror the upstream `to_dict`/`from_dict`
//! control-protocol conversions (camelCase field names).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::BoxFuture;
use crate::error::Result;

/// Permission mode for a session. Mirrors `PermissionMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionMode {
    /// Standard behavior; prompts for dangerous operations.
    #[serde(rename = "default")]
    Default,
    /// Auto-accept file edits.
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    /// Planning mode; no tool execution.
    #[serde(rename = "plan")]
    Plan,
    /// Bypass all permission checks.
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
    /// Don't prompt; deny if not pre-approved.
    #[serde(rename = "dontAsk")]
    DontAsk,
    /// Auto mode.
    #[serde(rename = "auto")]
    Auto,
}

/// Destination a permission update writes to. Mirrors
/// `PermissionUpdateDestination`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionUpdateDestination {
    /// User settings.
    #[serde(rename = "userSettings")]
    UserSettings,
    /// Project settings.
    #[serde(rename = "projectSettings")]
    ProjectSettings,
    /// Local settings.
    #[serde(rename = "localSettings")]
    LocalSettings,
    /// Session only.
    #[serde(rename = "session")]
    Session,
}

/// Permission behavior for a rule. Mirrors `PermissionBehavior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionBehavior {
    /// Allow.
    Allow,
    /// Deny.
    Deny,
    /// Ask.
    Ask,
}

/// A permission rule value. Mirrors `PermissionRuleValue`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRuleValue {
    /// Tool name the rule targets.
    pub tool_name: String,
    /// Rule content specifier, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
}

/// The variant of a [`PermissionUpdate`]. Mirrors the `type` literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionUpdateType {
    /// Add rules.
    #[serde(rename = "addRules")]
    AddRules,
    /// Replace rules.
    #[serde(rename = "replaceRules")]
    ReplaceRules,
    /// Remove rules.
    #[serde(rename = "removeRules")]
    RemoveRules,
    /// Set the permission mode.
    #[serde(rename = "setMode")]
    SetMode,
    /// Add directories.
    #[serde(rename = "addDirectories")]
    AddDirectories,
    /// Remove directories.
    #[serde(rename = "removeDirectories")]
    RemoveDirectories,
}

impl PermissionUpdateType {
    fn as_wire(self) -> &'static str {
        match self {
            PermissionUpdateType::AddRules => "addRules",
            PermissionUpdateType::ReplaceRules => "replaceRules",
            PermissionUpdateType::RemoveRules => "removeRules",
            PermissionUpdateType::SetMode => "setMode",
            PermissionUpdateType::AddDirectories => "addDirectories",
            PermissionUpdateType::RemoveDirectories => "removeDirectories",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "addRules" => PermissionUpdateType::AddRules,
            "replaceRules" => PermissionUpdateType::ReplaceRules,
            "removeRules" => PermissionUpdateType::RemoveRules,
            "setMode" => PermissionUpdateType::SetMode,
            "addDirectories" => PermissionUpdateType::AddDirectories,
            "removeDirectories" => PermissionUpdateType::RemoveDirectories,
            _ => return None,
        })
    }
}

/// A permission update configuration. Mirrors `PermissionUpdate`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PermissionUpdate {
    /// The update variant.
    #[serde(rename = "type")]
    pub update_type: Option<PermissionUpdateType>,
    /// Rules, for rules-based variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<PermissionRuleValue>>,
    /// Behavior, for rules-based variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<PermissionBehavior>,
    /// Mode, for `setMode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    /// Directories, for directory variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directories: Option<Vec<String>>,
    /// Destination for the update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PermissionUpdateDestination>,
}

impl PermissionUpdate {
    /// Serializes to the control-protocol wire dict. Mirrors `to_dict`:
    /// camelCase `rules` (`toolName`/`ruleContent`), and only the fields
    /// relevant to the variant are included.
    pub fn to_wire(&self) -> Value {
        let mut result = Map::new();
        if let Some(t) = self.update_type {
            result.insert("type".into(), json!(t.as_wire()));
        }
        if let Some(dest) = &self.destination {
            result.insert(
                "destination".into(),
                serde_json::to_value(dest).unwrap_or(Value::Null),
            );
        }
        match self.update_type {
            Some(
                PermissionUpdateType::AddRules
                | PermissionUpdateType::ReplaceRules
                | PermissionUpdateType::RemoveRules,
            ) => {
                if let Some(rules) = &self.rules {
                    let wire_rules: Vec<Value> = rules
                        .iter()
                        .map(|r| {
                            json!({
                                "toolName": r.tool_name,
                                "ruleContent": r.rule_content,
                            })
                        })
                        .collect();
                    result.insert("rules".into(), Value::Array(wire_rules));
                }
                if let Some(b) = &self.behavior {
                    result.insert("behavior".into(), serde_json::to_value(b).unwrap());
                }
            }
            Some(PermissionUpdateType::SetMode) => {
                if let Some(m) = &self.mode {
                    result.insert("mode".into(), serde_json::to_value(m).unwrap());
                }
            }
            Some(
                PermissionUpdateType::AddDirectories | PermissionUpdateType::RemoveDirectories,
            ) => {
                if let Some(dirs) = &self.directories {
                    result.insert("directories".into(), json!(dirs));
                }
            }
            None => {}
        }
        Value::Object(result)
    }

    /// Parses from the control-protocol wire dict. Mirrors `from_dict` (the
    /// inverse of [`to_wire`](Self::to_wire)). Returns `None` if `type` is
    /// missing or unrecognized.
    pub fn from_wire(data: &Value) -> Option<Self> {
        let update_type = PermissionUpdateType::from_wire(data.get("type")?.as_str()?)?;
        let rules = data.get("rules").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some(PermissionRuleValue {
                        tool_name: r.get("toolName")?.as_str()?.to_string(),
                        rule_content: r
                            .get("ruleContent")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        });
        Some(PermissionUpdate {
            update_type: Some(update_type),
            rules,
            behavior: data
                .get("behavior")
                .and_then(|b| serde_json::from_value(b.clone()).ok()),
            mode: data
                .get("mode")
                .and_then(|m| serde_json::from_value(m.clone()).ok()),
            directories: data.get("directories").and_then(|d| {
                d.as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            }),
            destination: data
                .get("destination")
                .and_then(|d| serde_json::from_value(d.clone()).ok()),
        })
    }
}

/// Context information for a tool-permission callback. Mirrors
/// `ToolPermissionContext`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolPermissionContext {
    /// Permission suggestions from the CLI.
    pub suggestions: Vec<PermissionUpdate>,
    /// Unique id for this tool call. Wire protocol guarantees a non-empty
    /// string when delivered to a callback.
    pub tool_use_id: Option<String>,
    /// Sub-agent id, if running inside a sub-agent.
    pub agent_id: Option<String>,
    /// The file path that triggered the request, if applicable.
    pub blocked_path: Option<String>,
    /// Why the request was triggered.
    pub decision_reason: Option<String>,
    /// Full permission prompt sentence.
    pub title: Option<String>,
    /// Short noun phrase for the tool action.
    pub display_name: Option<String>,
    /// Human-readable subtitle.
    pub description: Option<String>,
}

/// Allow result. Mirrors `PermissionResultAllow`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionResultAllow {
    /// Optionally rewrite the tool input before it runs.
    pub updated_input: Option<Map<String, Value>>,
    /// Permission updates to apply alongside the allow.
    pub updated_permissions: Option<Vec<PermissionUpdate>>,
}

/// Deny result. Mirrors `PermissionResultDeny`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionResultDeny {
    /// Message shown to the model.
    pub message: String,
    /// Whether to interrupt the run.
    pub interrupt: bool,
}

/// The result of a tool-permission callback. Mirrors `PermissionResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionResult {
    /// Allow the tool call.
    Allow(PermissionResultAllow),
    /// Deny the tool call.
    Deny(PermissionResultDeny),
}

/// Custom permission handler for tool calls that would otherwise prompt.
/// Mirrors the `CanUseTool` callable: `(tool_name, input, context) ->
/// PermissionResult`.
pub type CanUseTool = Arc<
    dyn Fn(String, Map<String, Value>, ToolPermissionContext) -> BoxFuture<'static, Result<PermissionResult>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_mode_wire_names() {
        assert_eq!(
            serde_json::to_value(PermissionMode::AcceptEdits).unwrap(),
            json!("acceptEdits")
        );
        assert_eq!(
            serde_json::to_value(PermissionMode::BypassPermissions).unwrap(),
            json!("bypassPermissions")
        );
    }

    #[test]
    fn permission_update_add_rules_to_wire() {
        let u = PermissionUpdate {
            update_type: Some(PermissionUpdateType::AddRules),
            rules: Some(vec![PermissionRuleValue {
                tool_name: "Bash".into(),
                rule_content: Some("ls:*".into()),
            }]),
            behavior: Some(PermissionBehavior::Allow),
            destination: Some(PermissionUpdateDestination::Session),
            ..Default::default()
        };
        let wire = u.to_wire();
        assert_eq!(
            wire,
            json!({
                "type": "addRules",
                "destination": "session",
                "rules": [{"toolName": "Bash", "ruleContent": "ls:*"}],
                "behavior": "allow",
            })
        );
    }

    #[test]
    fn permission_update_set_mode_to_wire() {
        let u = PermissionUpdate {
            update_type: Some(PermissionUpdateType::SetMode),
            mode: Some(PermissionMode::Plan),
            ..Default::default()
        };
        assert_eq!(u.to_wire(), json!({"type": "setMode", "mode": "plan"}));
    }

    #[test]
    fn permission_update_roundtrip_wire() {
        let u = PermissionUpdate {
            update_type: Some(PermissionUpdateType::AddRules),
            rules: Some(vec![PermissionRuleValue {
                tool_name: "Read".into(),
                rule_content: None,
            }]),
            behavior: Some(PermissionBehavior::Deny),
            destination: Some(PermissionUpdateDestination::ProjectSettings),
            ..Default::default()
        };
        let back = PermissionUpdate::from_wire(&u.to_wire()).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn permission_update_directories_to_wire() {
        let u = PermissionUpdate {
            update_type: Some(PermissionUpdateType::AddDirectories),
            directories: Some(vec!["/a".into(), "/b".into()]),
            ..Default::default()
        };
        assert_eq!(
            u.to_wire(),
            json!({"type": "addDirectories", "directories": ["/a", "/b"]})
        );
    }
}
