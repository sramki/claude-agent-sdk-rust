//! Sandbox configuration types.
//!
//! Ported from the sandbox section of the Python `types.py`
//! (`SandboxNetworkConfig`, `SandboxIgnoreViolations`, `SandboxSettings`). All
//! fields are optional (`total=False` upstream) and use wire-format camelCase.

use serde::{Deserialize, Serialize};

/// Network configuration for the sandbox. Mirrors `SandboxNetworkConfig`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxNetworkConfig {
    /// Domains sandboxed processes may access.
    #[serde(rename = "allowedDomains", default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    /// Domains always blocked.
    #[serde(rename = "deniedDomains", default, skip_serializing_if = "Option::is_none")]
    pub denied_domains: Option<Vec<String>>,
    /// Only respect managed-settings allowed domains.
    #[serde(
        rename = "allowManagedDomainsOnly",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_managed_domains_only: Option<bool>,
    /// Unix socket paths accessible in the sandbox.
    #[serde(rename = "allowUnixSockets", default, skip_serializing_if = "Option::is_none")]
    pub allow_unix_sockets: Option<Vec<String>>,
    /// Allow all Unix sockets (less secure).
    #[serde(
        rename = "allowAllUnixSockets",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_all_unix_sockets: Option<bool>,
    /// Allow binding to localhost ports (macOS only).
    #[serde(rename = "allowLocalBinding", default, skip_serializing_if = "Option::is_none")]
    pub allow_local_binding: Option<bool>,
    /// XPC/Mach service names to allow (macOS only).
    #[serde(rename = "allowMachLookup", default, skip_serializing_if = "Option::is_none")]
    pub allow_mach_lookup: Option<Vec<String>>,
    /// HTTP proxy port.
    #[serde(rename = "httpProxyPort", default, skip_serializing_if = "Option::is_none")]
    pub http_proxy_port: Option<i64>,
    /// SOCKS5 proxy port.
    #[serde(rename = "socksProxyPort", default, skip_serializing_if = "Option::is_none")]
    pub socks_proxy_port: Option<i64>,
}

/// Violations to ignore in the sandbox. Mirrors `SandboxIgnoreViolations`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxIgnoreViolations {
    /// File paths whose violations are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Vec<String>>,
    /// Network hosts whose violations are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Vec<String>>,
}

/// Sandbox settings for command execution isolation. Mirrors `SandboxSettings`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SandboxSettings {
    /// Enable bash sandboxing (macOS/Linux only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Auto-approve bash commands when sandboxed.
    #[serde(
        rename = "autoAllowBashIfSandboxed",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_allow_bash_if_sandboxed: Option<bool>,
    /// Commands that run outside the sandbox.
    #[serde(rename = "excludedCommands", default, skip_serializing_if = "Option::is_none")]
    pub excluded_commands: Option<Vec<String>>,
    /// Allow commands to bypass the sandbox.
    #[serde(
        rename = "allowUnsandboxedCommands",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub allow_unsandboxed_commands: Option<bool>,
    /// Network configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<SandboxNetworkConfig>,
    /// Violations to ignore.
    #[serde(rename = "ignoreViolations", default, skip_serializing_if = "Option::is_none")]
    pub ignore_violations: Option<SandboxIgnoreViolations>,
    /// Enable a weaker sandbox for unprivileged Docker (Linux only).
    #[serde(
        rename = "enableWeakerNestedSandbox",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub enable_weaker_nested_sandbox: Option<bool>,
}
