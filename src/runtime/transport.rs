//! Subprocess transport over the Claude Code CLI.
//!
//! Faithful port of `_internal/transport/subprocess_cli.py` (and the abstract
//! `Transport` in `transport/__init__.py`). The pure argument builder
//! ([`build_command`]) and line parser ([`parse_stdout_line`]) are unit-tested;
//! the async I/O uses tokio. Stdout is read on a background task that frames
//! NDJSON lines and forwards parsed values over a channel, so writing to stdin
//! and reading messages proceed concurrently.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{Error, Result};
use crate::types::{
    ClaudeAgentOptions, McpServers, Skills, StderrCallback, SystemPrompt, ThinkingConfig,
    ToolsConfig,
};

const DEFAULT_MAX_BUFFER_SIZE: usize = 1024 * 1024;
const MINIMUM_CLAUDE_CODE_VERSION: (u32, u32, u32) = (2, 0, 0);

/// The version reported to the CLI via `CLAUDE_AGENT_SDK_VERSION`.
const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Tracks live CLI child PIDs so an `atexit` sweep can SIGTERM them if the host
/// process exits without `close()`. Mirrors the upstream `_ACTIVE_CHILDREN` +
/// `atexit` reaper (tokio's `kill_on_drop` covers normal `Drop`, but not
/// `process::exit` / `panic = "abort"`, which this backstops on unix).
#[cfg(unix)]
mod reaper {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static ACTIVE: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

    fn registry() -> &'static Mutex<HashSet<u32>> {
        ACTIVE.get_or_init(|| {
            // Register the reaper the first time we track a child.
            unsafe { libc::atexit(reap_all) };
            Mutex::new(HashSet::new())
        })
    }

    pub fn register(pid: u32) {
        registry().lock().unwrap().insert(pid);
    }

    pub fn unregister(pid: u32) {
        if let Some(m) = ACTIVE.get() {
            if let Ok(mut set) = m.lock() {
                set.remove(&pid);
            }
        }
    }

    extern "C" fn reap_all() {
        if let Some(m) = ACTIVE.get() {
            if let Ok(set) = m.lock() {
                for &pid in set.iter() {
                    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod reaper {
    pub fn register(_pid: u32) {}
    pub fn unregister(_pid: u32) {}
}

/// A stream of raw CLI messages.
pub type MessageStream = Pin<Box<dyn Stream<Item = Result<Value>> + Send>>;

/// Transport abstraction. Mirrors the Python `Transport` ABC.
#[async_trait]
pub trait Transport: Send {
    /// Starts the underlying transport.
    async fn connect(&mut self) -> Result<()>;
    /// Writes raw data (typically one JSON line) to the transport.
    async fn write(&mut self, data: &str) -> Result<()>;
    /// Ends the input stream (closes stdin).
    async fn end_input(&mut self) -> Result<()>;
    /// Takes the message stream. Valid once after [`connect`](Self::connect).
    fn read_messages(&mut self) -> MessageStream;
    /// Closes the transport and cleans up.
    async fn close(&mut self) -> Result<()>;
    /// Whether the transport is ready for communication.
    fn is_ready(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Parses one complete NDJSON stdout line. Returns `Ok(None)` for blank or
/// non-JSON lines (e.g. `[SandboxDebug] ...`), and an error for a line that
/// looks like JSON but does not parse. Mirrors `_parse_stdout_line`.
pub(crate) fn parse_stdout_line(line: &str) -> Result<Option<Value>> {
    let line = line.trim();
    if line.is_empty() || !line.starts_with('{') {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(line) {
        Ok(v) => Ok(Some(v)),
        Err(e) => Err(Error::json_decode(line, e)),
    }
}

/// Computes the effective `allowed_tools` and `setting_sources` after applying
/// skill defaults. Mirrors `_apply_skills_defaults`.
fn apply_skills_defaults(options: &ClaudeAgentOptions) -> (Vec<String>, Option<Vec<String>>) {
    let mut allowed_tools = options.allowed_tools.clone();
    let mut setting_sources: Option<Vec<String>> = options.setting_sources.as_ref().map(|ss| {
        ss.iter()
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    });

    match &options.skills {
        None => return (allowed_tools, setting_sources),
        Some(Skills::All) => {
            if !allowed_tools.iter().any(|t| t == "Skill") {
                allowed_tools.push("Skill".to_string());
            }
        }
        Some(Skills::List(names)) => {
            for name in names {
                let pattern = format!("Skill({name})");
                if !allowed_tools.contains(&pattern) {
                    allowed_tools.push(pattern);
                }
            }
        }
    }

    if setting_sources.is_none() {
        setting_sources = Some(vec!["user".to_string(), "project".to_string()]);
    }
    (allowed_tools, setting_sources)
}

/// Builds the settings value, merging sandbox settings if provided. Mirrors
/// `_build_settings_value`.
fn build_settings_value(options: &ClaudeAgentOptions) -> Option<String> {
    let has_settings = options.settings.is_some();
    let has_sandbox = options.sandbox.is_some();
    if !has_settings && !has_sandbox {
        return None;
    }
    if has_settings && !has_sandbox {
        return options.settings.clone();
    }

    let mut settings_obj: Map<String, Value> = Map::new();
    if let Some(settings) = &options.settings {
        let s = settings.trim();
        let parsed = if s.starts_with('{') && s.ends_with('}') {
            serde_json::from_str::<Map<String, Value>>(s).ok()
        } else {
            std::fs::read_to_string(s)
                .ok()
                .and_then(|c| serde_json::from_str::<Map<String, Value>>(&c).ok())
        };
        if let Some(p) = parsed {
            settings_obj = p;
        }
    }
    if let Some(sandbox) = &options.sandbox {
        settings_obj.insert("sandbox".into(), serde_json::to_value(sandbox).unwrap());
    }
    Some(Value::Object(settings_obj).to_string())
}

fn wire_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|x| x.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Builds the full CLI command (excluding the executable's own resolution).
/// Faithful port of `_build_command`.
pub(crate) fn build_command(cli_path: &str, options: &ClaudeAgentOptions) -> Vec<String> {
    let mut cmd: Vec<String> = vec![
        cli_path.to_string(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];

    // System prompt.
    match &options.system_prompt {
        None => {
            cmd.push("--system-prompt".into());
            cmd.push(String::new());
        }
        Some(SystemPrompt::Text(s)) => {
            cmd.push("--system-prompt".into());
            cmd.push(s.clone());
        }
        Some(SystemPrompt::File(path)) => {
            cmd.push("--system-prompt-file".into());
            cmd.push(path.clone());
        }
        Some(SystemPrompt::Preset(p)) => {
            if let Some(append) = &p.append {
                cmd.push("--append-system-prompt".into());
                cmd.push(append.clone());
            }
        }
    }

    // Tools.
    if let Some(tools) = &options.tools {
        match tools {
            ToolsConfig::List(list) => {
                cmd.push("--tools".into());
                cmd.push(list.join(","));
            }
            ToolsConfig::Preset => {
                cmd.push("--tools".into());
                cmd.push("default".into());
            }
        }
    }

    let (allowed_tools, setting_sources) = apply_skills_defaults(options);
    if !allowed_tools.is_empty() {
        cmd.push("--allowedTools".into());
        cmd.push(allowed_tools.join(","));
    }

    if let Some(n) = options.max_turns {
        if n != 0 {
            cmd.push("--max-turns".into());
            cmd.push(n.to_string());
        }
    }
    if let Some(b) = options.max_budget_usd {
        cmd.push("--max-budget-usd".into());
        cmd.push(b.to_string());
    }
    if !options.disallowed_tools.is_empty() {
        cmd.push("--disallowedTools".into());
        cmd.push(options.disallowed_tools.join(","));
    }
    if let Some(tb) = &options.task_budget {
        cmd.push("--task-budget".into());
        cmd.push(tb.total.to_string());
    }
    if let Some(model) = &options.model {
        cmd.push("--model".into());
        cmd.push(model.clone());
    }
    if let Some(fm) = &options.fallback_model {
        cmd.push("--fallback-model".into());
        cmd.push(fm.clone());
    }
    if !options.betas.is_empty() {
        let joined = options
            .betas
            .iter()
            .map(wire_str)
            .collect::<Vec<_>>()
            .join(",");
        cmd.push("--betas".into());
        cmd.push(joined);
    }
    if let Some(name) = &options.permission_prompt_tool_name {
        cmd.push("--permission-prompt-tool".into());
        cmd.push(name.clone());
    }
    if let Some(pm) = &options.permission_mode {
        cmd.push("--permission-mode".into());
        cmd.push(wire_str(pm));
    }
    if options.continue_conversation {
        cmd.push("--continue".into());
    }
    if let Some(resume) = &options.resume {
        cmd.push("--resume".into());
        cmd.push(resume.clone());
    }
    if let Some(sid) = &options.session_id {
        cmd.push("--session-id".into());
        cmd.push(sid.clone());
    }
    if let Some(settings_value) = build_settings_value(options) {
        cmd.push("--settings".into());
        cmd.push(settings_value);
    }
    for dir in &options.add_dirs {
        cmd.push("--add-dir".into());
        cmd.push(dir.to_string_lossy().into_owned());
    }

    // MCP servers.
    match &options.mcp_servers {
        McpServers::Map(map) if !map.is_empty() => {
            let servers: Map<String, Value> = map
                .iter()
                .map(|(name, config)| (name.clone(), config.to_wire()))
                .collect();
            cmd.push("--mcp-config".into());
            cmd.push(json!({ "mcpServers": servers }).to_string());
        }
        McpServers::Path(p) => {
            cmd.push("--mcp-config".into());
            cmd.push(p.to_string_lossy().into_owned());
        }
        _ => {}
    }

    if options.include_partial_messages {
        cmd.push("--include-partial-messages".into());
    }
    if options.include_hook_events {
        cmd.push("--include-hook-events".into());
    }
    if options.strict_mcp_config {
        cmd.push("--strict-mcp-config".into());
    }
    if options.fork_session {
        cmd.push("--fork-session".into());
    }
    if options.session_store.is_some() {
        cmd.push("--session-mirror".into());
    }
    if let Some(ss) = &setting_sources {
        cmd.push(format!("--setting-sources={}", ss.join(",")));
    }
    for plugin in &options.plugins {
        // Only local plugins are supported (matches upstream).
        cmd.push("--plugin-dir".into());
        cmd.push(plugin.path.clone());
    }
    for (flag, value) in &options.extra_args {
        match value {
            None => cmd.push(format!("--{flag}")),
            Some(v) => {
                cmd.push(format!("--{flag}"));
                cmd.push(v.clone());
            }
        }
    }

    // Thinking config takes precedence over deprecated max_thinking_tokens.
    if let Some(thinking) = &options.thinking {
        match thinking {
            ThinkingConfig::Adaptive { display } => {
                cmd.push("--thinking".into());
                cmd.push("adaptive".into());
                if let Some(d) = display {
                    cmd.push("--thinking-display".into());
                    cmd.push(wire_str(d));
                }
            }
            ThinkingConfig::Enabled {
                budget_tokens,
                display,
            } => {
                cmd.push("--max-thinking-tokens".into());
                cmd.push(budget_tokens.to_string());
                if let Some(d) = display {
                    cmd.push("--thinking-display".into());
                    cmd.push(wire_str(d));
                }
            }
            ThinkingConfig::Disabled => {
                cmd.push("--thinking".into());
                cmd.push("disabled".into());
            }
        }
    } else if let Some(mtt) = options.max_thinking_tokens {
        cmd.push("--max-thinking-tokens".into());
        cmd.push(mtt.to_string());
    }

    if let Some(effort) = &options.effort {
        cmd.push("--effort".into());
        cmd.push(wire_str(effort));
    }

    if let Some(of) = &options.output_format {
        if of.get("type").and_then(Value::as_str) == Some("json_schema") {
            if let Some(schema) = of.get("schema") {
                cmd.push("--json-schema".into());
                cmd.push(schema.to_string());
            }
        }
    }

    cmd.push("--input-format".into());
    cmd.push("stream-json".into());

    cmd
}

/// Candidate CLI file names, most-specific first. On Windows a standard
/// `npm install -g` produces a `claude.cmd` batch shim on `PATH`; we try the
/// executable extensions the way Python's `shutil.which("claude")` does via
/// `PATHEXT`. The shim itself is thin — see [`prefer_real_exe`] for why a
/// resolved `.cmd`/`.bat` match is upgraded to the real binary it wraps.
fn cli_candidate_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["claude.cmd", "claude.exe", "claude.bat", "claude"]
    } else {
        &["claude"]
    }
}

/// On Windows, an npm-installed `claude.cmd`/`claude.bat` is a thin shim
/// (`"%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe" %*`) that
/// execs a real compiled binary at a fixed, predictable location relative to
/// itself. Passing a JSON-bearing argument (`--mcp-config`, `--json-schema`)
/// through a batch file corrupts it in transit — `cmd.exe`'s re-quoting for
/// `.cmd`/`.bat` targets mangles braces/quotes, confirmed via live
/// reproduction against both flags — a bug class that does not exist for a
/// genuine PE executable target, which Rust's `std::process::Command` spawns
/// via the standard, correct argv-quoting instead. Upgrading to the real
/// `.exe` here sidesteps the ENTIRE bug class at the source rather than
/// special-casing each affected flag (this repo already special-cased
/// `--mcp-config` via a temp file before this fix — `--json-schema` has no
/// such file-path fallback, so that approach doesn't generalize).
///
/// Falls back to the resolved batch shim itself when the real binary isn't
/// found at the expected relative path (a different install layout, or a
/// future packaging change) — no functional regression, just no upgrade.
#[cfg(windows)]
fn prefer_real_exe(resolved: &Path) -> PathBuf {
    let is_batch = resolved
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false);
    if !is_batch {
        return resolved.to_path_buf();
    }
    let Some(dir) = resolved.parent() else {
        return resolved.to_path_buf();
    };
    let real_exe = dir
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("bin")
        .join("claude.exe");
    if real_exe.is_file() {
        real_exe
    } else {
        resolved.to_path_buf()
    }
}

#[cfg(not(windows))]
fn prefer_real_exe(resolved: &Path) -> PathBuf {
    resolved.to_path_buf()
}

/// Finds the Claude Code CLI binary. Mirrors `_find_cli` (minus the bundled
/// binary, which the Rust crate does not ship).
pub(crate) fn find_cli() -> Result<String> {
    let names = cli_candidate_names();

    // Search PATH (each dir × each candidate name).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in names {
                let candidate = dir.join(name);
                if is_executable_file(&candidate) {
                    return Ok(prefer_real_exe(&candidate).to_string_lossy().into_owned());
                }
            }
        }
    }

    // Fallback well-known bin directories (each × each candidate name).
    if let Some(home) = home_dir() {
        let dirs = [
            home.join(".npm-global/bin"),
            PathBuf::from("/usr/local/bin"),
            home.join(".local/bin"),
            home.join("node_modules/.bin"),
            home.join(".yarn/bin"),
            home.join(".claude/local"),
        ];
        for dir in dirs {
            for name in names {
                let path = dir.join(name);
                if path.is_file() {
                    return Ok(prefer_real_exe(&path).to_string_lossy().into_owned());
                }
            }
        }
    }

    Err(Error::cli_not_found(None::<String>))
}

fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
}

fn parse_version_triple(s: &str) -> Option<(u32, u32, u32)> {
    let head: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let parts: Vec<&str> = head.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

// ---------------------------------------------------------------------------
// Subprocess transport
// ---------------------------------------------------------------------------

/// Subprocess transport using the Claude Code CLI.
pub struct SubprocessCliTransport {
    options: ClaudeAgentOptions,
    cli_path: Option<String>,
    cwd: Option<String>,
    ready: bool,
    max_buffer_size: usize,
    stdin: Option<ChildStdin>,
    msg_rx: Option<mpsc::Receiver<Result<Value>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    reader_handle: Option<JoinHandle<()>>,
    stderr_handle: Option<JoinHandle<()>>,
    // Holds the `--mcp-config` temp file (Windows only — see `connect`) alive
    // for the transport's lifetime; dropped (and deleted) on transport drop.
    mcp_config_tempfile: Option<tempfile::NamedTempFile>,
}

impl SubprocessCliTransport {
    /// Creates a transport for the given options. Call [`connect`] to start.
    ///
    /// [`connect`]: Transport::connect
    pub fn new(options: ClaudeAgentOptions) -> Self {
        let cli_path = options
            .cli_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let cwd = options
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let max_buffer_size = options.max_buffer_size.unwrap_or(DEFAULT_MAX_BUFFER_SIZE);
        SubprocessCliTransport {
            options,
            cli_path,
            cwd,
            ready: false,
            max_buffer_size,
            stdin: None,
            msg_rx: None,
            shutdown_tx: None,
            reader_handle: None,
            stderr_handle: None,
            mcp_config_tempfile: None,
        }
    }

    /// Rewrites an inline JSON `--mcp-config` argument to a temp-file path on
    /// Windows.
    ///
    /// The CLI is invoked through a `claude.cmd` batch shim there (see
    /// [`find_cli`]), and Rust's batch-file argument quoting can't reliably
    /// round-trip a JSON blob through `cmd.exe`'s tokenizer — braces and
    /// quotes get corrupted, and the corruption can bleed into the *next*
    /// argument, observed live as `--mcp-config`'s JSON value and the
    /// following `--input-format stream-json` merging into one garbled
    /// argument. Writing the same JSON to a file and passing the path
    /// instead sidesteps `cmd.exe` quoting entirely — the file's lifetime is
    /// tied to `self` via `mcp_config_tempfile` so it outlives the spawned
    /// child. No-op on non-Windows and when `--mcp-config`'s value is
    /// already a path (the `McpServers::Path` case never emits inline JSON).
    fn route_mcp_config_through_tempfile_on_windows(&mut self, cmd: &mut [String]) -> Result<()> {
        if !cfg!(windows) {
            return Ok(());
        }
        let Some(idx) = cmd.iter().position(|a| a == "--mcp-config") else {
            return Ok(());
        };
        let Some(value) = cmd.get(idx + 1) else {
            return Ok(());
        };
        if !value.trim_start().starts_with('{') {
            return Ok(());
        }
        let mut file = tempfile::Builder::new()
            .prefix("claude-mcp-config-")
            .suffix(".json")
            .tempfile()
            .map_err(|e| {
                Error::connection(format!("Failed to create MCP config temp file: {e}"))
            })?;
        use std::io::Write;
        file.write_all(value.as_bytes())
            .map_err(|e| Error::connection(format!("Failed to write MCP config temp file: {e}")))?;
        cmd[idx + 1] = file.path().to_string_lossy().into_owned();
        self.mcp_config_tempfile = Some(file);
        Ok(())
    }

    async fn check_version(&self, cli_path: &str) {
        let fut = Command::new(cli_path).arg("-v").output();
        if let Ok(Ok(output)) = tokio::time::timeout(std::time::Duration::from_secs(2), fut).await {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(v) = parse_version_triple(text.trim()) {
                if v < MINIMUM_CLAUDE_CODE_VERSION {
                    eprintln!(
                        "warning: Claude Code version {}.{}.{} at {} is below the minimum {}.{}.{} supported by the Agent SDK; some features may not work.",
                        v.0, v.1, v.2, cli_path,
                        MINIMUM_CLAUDE_CODE_VERSION.0,
                        MINIMUM_CLAUDE_CODE_VERSION.1,
                        MINIMUM_CLAUDE_CODE_VERSION.2,
                    );
                }
            }
        }
    }

    fn build_env(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars()
            .filter(|(k, _)| k != "CLAUDECODE")
            .collect();
        env.insert("CLAUDE_CODE_ENTRYPOINT".into(), "sdk-rust".into());
        for (k, v) in &self.options.env {
            env.insert(k.clone(), v.clone());
        }
        env.insert("CLAUDE_AGENT_SDK_VERSION".into(), SDK_VERSION.into());
        if self.options.enable_file_checkpointing {
            env.insert(
                "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING".into(),
                "true".into(),
            );
        }
        if let Some(cwd) = &self.cwd {
            env.insert("PWD".into(), cwd.clone());
        }
        env
    }
}

#[async_trait]
impl Transport for SubprocessCliTransport {
    async fn connect(&mut self) -> Result<()> {
        if self.reader_handle.is_some() {
            return Ok(());
        }
        if self.cli_path.is_none() {
            self.cli_path = Some(find_cli()?);
        }
        let cli_path = self.cli_path.clone().unwrap();

        if std::env::var_os("CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK").is_none() {
            self.check_version(&cli_path).await;
        }

        // Fail clearly on a missing working directory.
        if let Some(cwd) = &self.cwd {
            if !Path::new(cwd).exists() {
                return Err(Error::connection(format!(
                    "Working directory does not exist: {cwd}"
                )));
            }
        }

        let mut cmd = build_command(&cli_path, &self.options);
        self.route_mcp_config_through_tempfile_on_windows(&mut cmd)?;
        let pipe_stderr = self.options.stderr.is_some();

        let mut command = Command::new(&cmd[0]);
        command
            .args(&cmd[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(if pipe_stderr {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::inherit()
            })
            .env_clear()
            .envs(self.build_env())
            .kill_on_drop(true);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        // Run the subprocess as a specific OS user (unix, numeric uid only —
        // username resolution would need a passwd lookup). Mirrors the upstream
        // `user=` subprocess argument.
        #[cfg(unix)]
        if let Some(uid) = self
            .options
            .user
            .as_deref()
            .and_then(|u| u.parse::<u32>().ok())
        {
            command.uid(uid);
        }

        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::cli_not_found(Some(cli_path.clone()))
            } else {
                Error::connection(format!("Failed to start Claude Code: {e}"))
            }
        })?;

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::connection("Failed to capture CLI stdout"))?;
        let stderr = child.stderr.take();

        // Background stdout reader → channel.
        let (tx, rx) = mpsc::channel::<Result<Value>>(1024);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let max_buffer = self.max_buffer_size;
        let reader_handle = tokio::spawn(read_loop(child, stdout, shutdown_rx, max_buffer, tx));

        // Background stderr reader → callback.
        let stderr_handle = match (stderr, self.options.stderr.clone()) {
            (Some(stderr), Some(cb)) => Some(tokio::spawn(stderr_loop(stderr, cb))),
            _ => None,
        };

        self.stdin = stdin;
        self.msg_rx = Some(rx);
        self.shutdown_tx = Some(shutdown_tx);
        self.reader_handle = Some(reader_handle);
        self.stderr_handle = stderr_handle;
        self.ready = true;
        Ok(())
    }

    async fn write(&mut self, data: &str) -> Result<()> {
        if !self.ready {
            return Err(Error::connection(
                "ProcessTransport is not ready for writing",
            ));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::connection("ProcessTransport is not ready for writing"))?;
        if let Err(e) = stdin.write_all(data.as_bytes()).await {
            self.ready = false;
            return Err(Error::connection(format!(
                "Failed to write to process stdin: {e}"
            )));
        }
        if let Err(e) = stdin.flush().await {
            self.ready = false;
            return Err(Error::connection(format!(
                "Failed to write to process stdin: {e}"
            )));
        }
        Ok(())
    }

    async fn end_input(&mut self) -> Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.shutdown().await;
        }
        Ok(())
    }

    fn read_messages(&mut self) -> MessageStream {
        match self.msg_rx.take() {
            Some(rx) => Box::pin(ReceiverStream::new(rx)),
            None => Box::pin(tokio_stream::empty()),
        }
    }

    async fn close(&mut self) -> Result<()> {
        self.ready = false;
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.shutdown().await;
        }
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.reader_handle.take() {
            // Bounded by the read loop's graceful-shutdown escalation
            // (grace + SIGTERM + SIGKILL, ~15s worst case) plus slack.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(20), handle).await;
        }
        if let Some(handle) = self.stderr_handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        self.msg_rx = None;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

fn buffer_exceeded_error(len: usize, max: usize) -> Error {
    // Synthesize a decode error to carry the buffer-overflow diagnostic,
    // matching the upstream CLIJSONDecodeError for oversized messages.
    let source = serde_json::from_str::<Value>("").unwrap_err();
    Error::json_decode(
        format!("JSON message exceeded maximum buffer size of {max} bytes (got {len})"),
        source,
    )
}

/// Frames NDJSON lines from a reader and forwards parsed values to `tx`.
///
/// Uses `read_until('\n')` so a *truncated* final line (EOF mid-write, no
/// trailing newline) is dropped rather than surfaced as a decode error, while a
/// complete (newline-terminated) corrupt line still surfaces one — matching the
/// upstream framer/flush behavior. Returns `true` if it stopped because
/// `shutdown` was signaled. Generic over the reader so it is unit-testable
/// without a real subprocess.
async fn pump_stdout<R>(
    stdout: R,
    shutdown: &mut oneshot::Receiver<()>,
    max_buffer: usize,
    tx: &mpsc::Sender<Result<Value>>,
) -> bool
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(stdout);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        tokio::select! {
            biased;
            _ = &mut *shutdown => return true,
            res = reader.read_until(b'\n', &mut buf) => match res {
                Ok(0) => return false, // EOF, nothing pending
                Ok(_) => {
                    let had_newline = buf.last() == Some(&b'\n');
                    if buf.len() > max_buffer {
                        let _ = tx.send(Err(buffer_exceeded_error(buf.len(), max_buffer))).await;
                        return false;
                    }
                    let line = String::from_utf8_lossy(&buf);
                    match parse_stdout_line(&line) {
                        Ok(Some(v)) => {
                            if tx.send(Ok(v)).await.is_err() {
                                return false; // receiver dropped
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            // Complete corrupt line -> real decode error;
                            // truncated final line -> drop silently.
                            if had_newline {
                                let _ = tx.send(Err(e)).await;
                            }
                            return false;
                        }
                    }
                    if !had_newline {
                        return false; // EOF reached on a partial final line
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(Error::Io(e))).await;
                    return false;
                }
            }
        }
    }
}

/// Background task: frame + parse stdout, then reap the process and surface a
/// non-zero exit as a `ProcessError` (or run the graceful shutdown escalation
/// when `close()` signaled).
async fn read_loop(
    mut child: Child,
    stdout: ChildStdout,
    mut shutdown: oneshot::Receiver<()>,
    max_buffer: usize,
    tx: mpsc::Sender<Result<Value>>,
) {
    let pid = child.id();
    if let Some(p) = pid {
        reaper::register(p);
    }

    let shutting_down = pump_stdout(stdout, &mut shutdown, max_buffer, &tx).await;

    if shutting_down {
        // stdin was closed by close() before signaling; give the CLI a grace
        // period to flush its session file, then escalate. Mirrors upstream's
        // shielded terminate/kill sequence (close(), #625).
        graceful_terminate(&mut child, pid).await;
    } else if let Ok(status) = child.wait().await {
        if !status.success() {
            let code = status.code();
            let _ = tx
                .send(Err(Error::process(
                    format!("Command failed with exit code {}", code.unwrap_or(-1)),
                    code,
                    Some("Check stderr output for details".into()),
                )))
                .await;
        }
    }

    if let Some(p) = pid {
        reaper::unregister(p);
    }
}

/// Waits for a graceful exit, escalating to SIGTERM then SIGKILL (each bounded).
/// Mirrors `SubprocessCLITransport.close()`'s terminate/kill escalation.
async fn graceful_terminate(child: &mut Child, pid: Option<u32>) {
    use std::time::Duration;
    let grace = Duration::from_secs(5);

    if let Ok(Ok(_)) = tokio::time::timeout(grace, child.wait()).await {
        return;
    }
    // SIGTERM (on non-unix, fall straight through to SIGKILL via start_kill).
    #[cfg(unix)]
    if let Some(p) = pid {
        unsafe { libc::kill(p as libc::pid_t, libc::SIGTERM) };
    }
    #[cfg(not(unix))]
    let _ = (pid, child.start_kill());
    if let Ok(Ok(_)) = tokio::time::timeout(grace, child.wait()).await {
        return;
    }
    // SIGKILL.
    let _ = child.start_kill();
    let _ = tokio::time::timeout(grace, child.wait()).await;
}

/// Background task: frame stderr into lines and forward them to the callback.
async fn stderr_loop(stderr: tokio::process::ChildStderr, callback: StderrCallback) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Isolate the user's callback per line: a panic must not kill the loop
        // and silently drop every subsequent stderr line (upstream isolates the
        // callback the same way).
        let line = trimmed.to_string();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(line)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AgentDefinition, ClaudeAgentOptions, EffortLevel, McpServerConfig, McpServers,
        McpStdioServerConfig, PermissionMode, SandboxSettings, SdkBeta, SdkPluginConfig, Skills,
        SystemPrompt, SystemPromptPreset, TaskBudget, ThinkingConfig, ThinkingDisplay, ToolsConfig,
    };
    use std::collections::HashMap;

    // ── prefer_real_exe (Windows npm-shim → real-binary upgrade) ────────

    #[cfg(windows)]
    #[test]
    fn prefer_real_exe_upgrades_a_cmd_shim_to_the_real_exe_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        std::fs::write(&shim, "@ECHO off").unwrap();
        let real_exe_dir = dir
            .path()
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin");
        std::fs::create_dir_all(&real_exe_dir).unwrap();
        let real_exe = real_exe_dir.join("claude.exe");
        std::fs::write(&real_exe, "fake binary").unwrap();

        assert_eq!(prefer_real_exe(&shim), real_exe);
    }

    #[cfg(windows)]
    #[test]
    fn prefer_real_exe_falls_back_to_the_shim_when_no_real_exe_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        std::fs::write(&shim, "@ECHO off").unwrap();
        // No node_modules/@anthropic-ai/claude-code/bin/claude.exe created.

        assert_eq!(prefer_real_exe(&shim), shim);
    }

    #[cfg(windows)]
    #[test]
    fn prefer_real_exe_is_a_noop_for_a_non_batch_target() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("claude.exe");
        std::fs::write(&exe, "fake binary").unwrap();
        // A sibling node_modules/.../claude.exe existing here must NOT
        // redirect an already-real .exe target.
        let real_exe_dir = dir
            .path()
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin");
        std::fs::create_dir_all(&real_exe_dir).unwrap();
        std::fs::write(real_exe_dir.join("claude.exe"), "other binary").unwrap();

        assert_eq!(prefer_real_exe(&exe), exe);
    }

    fn base() -> ClaudeAgentOptions {
        ClaudeAgentOptions::default()
    }

    fn has_pair(cmd: &[String], flag: &str, val: &str) -> bool {
        cmd.windows(2).any(|w| w[0] == flag && w[1] == val)
    }

    #[test]
    fn command_has_stream_json_and_input_format() {
        let cmd = build_command("/bin/claude", &base());
        assert_eq!(cmd[0], "/bin/claude");
        assert!(cmd
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"]));
        assert!(cmd
            .windows(2)
            .any(|w| w == ["--input-format", "stream-json"]));
        assert!(cmd.contains(&"--verbose".to_string()));
    }

    #[test]
    fn default_system_prompt_is_empty_flag() {
        let cmd = build_command("/bin/claude", &base());
        let idx = cmd.iter().position(|a| a == "--system-prompt").unwrap();
        assert_eq!(cmd[idx + 1], "");
    }

    #[test]
    fn text_system_prompt() {
        let mut o = base();
        o.system_prompt = Some(SystemPrompt::Text("Be terse".into()));
        let cmd = build_command("/bin/claude", &o);
        let idx = cmd.iter().position(|a| a == "--system-prompt").unwrap();
        assert_eq!(cmd[idx + 1], "Be terse");
    }

    #[test]
    fn permission_mode_and_model_flags() {
        let mut o = base();
        o.permission_mode = Some(PermissionMode::AcceptEdits);
        o.model = Some("claude-opus-4-5".into());
        let cmd = build_command("/bin/claude", &o);
        assert!(cmd
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"]));
        assert!(cmd.windows(2).any(|w| w == ["--model", "claude-opus-4-5"]));
    }

    #[test]
    fn allowed_and_disallowed_tools() {
        let mut o = base();
        o.allowed_tools = vec!["Read".into(), "Write".into()];
        o.disallowed_tools = vec!["Bash".into()];
        let cmd = build_command("/bin/claude", &o);
        assert!(cmd
            .windows(2)
            .any(|w| w == ["--allowedTools", "Read,Write"]));
        assert!(cmd.windows(2).any(|w| w == ["--disallowedTools", "Bash"]));
    }

    #[test]
    fn skills_all_injects_skill_and_setting_sources() {
        let mut o = base();
        o.skills = Some(Skills::All);
        let cmd = build_command("/bin/claude", &o);
        assert!(cmd.windows(2).any(|w| w == ["--allowedTools", "Skill"]));
        assert!(cmd.iter().any(|a| a == "--setting-sources=user,project"));
    }

    #[test]
    fn thinking_enabled_uses_max_tokens() {
        let mut o = base();
        o.thinking = Some(ThinkingConfig::Enabled {
            budget_tokens: 2048,
            display: None,
        });
        let cmd = build_command("/bin/claude", &o);
        assert!(cmd
            .windows(2)
            .any(|w| w == ["--max-thinking-tokens", "2048"]));
    }

    #[test]
    fn extra_args_bool_and_valued() {
        let mut o = base();
        o.extra_args.insert("foo".into(), None);
        o.extra_args.insert("bar".into(), Some("baz".into()));
        let cmd = build_command("/bin/claude", &o);
        assert!(cmd.iter().any(|a| a == "--foo"));
        assert!(cmd.windows(2).any(|w| w == ["--bar", "baz"]));
    }

    #[test]
    fn parse_stdout_line_variants() {
        assert!(parse_stdout_line("").unwrap().is_none());
        assert!(parse_stdout_line("   ").unwrap().is_none());
        assert!(parse_stdout_line("[SandboxDebug] hi").unwrap().is_none());
        assert!(parse_stdout_line(r#"{"type":"x"}"#).unwrap().is_some());
        assert!(parse_stdout_line("{not json").is_err());
    }

    #[test]
    fn version_triple_parse() {
        assert_eq!(parse_version_triple("2.1.3 (extra)"), Some((2, 1, 3)));
        assert_eq!(parse_version_triple("nope"), None);
    }

    // --- Additional build_command cases (ported from test_transport.py) ---

    #[test]
    fn no_print_flag_ever() {
        assert!(!build_command("/c", &base()).iter().any(|a| a == "--print"));
    }

    #[test]
    fn include_hook_events_toggles() {
        let mut o = base();
        o.include_hook_events = true;
        assert!(build_command("/c", &o).contains(&"--include-hook-events".to_string()));
        assert!(!build_command("/c", &base()).contains(&"--include-hook-events".to_string()));
    }

    #[test]
    fn strict_mcp_config_toggles() {
        let mut o = base();
        o.strict_mcp_config = true;
        assert!(build_command("/c", &o).contains(&"--strict-mcp-config".to_string()));
        assert!(!build_command("/c", &base()).contains(&"--strict-mcp-config".to_string()));
    }

    #[test]
    fn effort_and_dont_ask_and_fallback() {
        let mut o = base();
        o.effort = Some(EffortLevel::Xhigh);
        o.permission_mode = Some(PermissionMode::DontAsk);
        o.model = Some("opus".into());
        o.fallback_model = Some("sonnet".into());
        let cmd = build_command("/c", &o);
        assert!(has_pair(&cmd, "--effort", "xhigh"));
        assert!(has_pair(&cmd, "--permission-mode", "dontAsk"));
        assert!(has_pair(&cmd, "--model", "opus"));
        assert!(has_pair(&cmd, "--fallback-model", "sonnet"));
    }

    #[test]
    fn system_prompt_preset_no_append_emits_nothing() {
        let mut o = base();
        o.system_prompt = Some(SystemPrompt::Preset(SystemPromptPreset {
            preset: "claude_code".into(),
            append: None,
            exclude_dynamic_sections: None,
        }));
        let cmd = build_command("/c", &o);
        assert!(!cmd.iter().any(|a| a == "--system-prompt"));
        assert!(!cmd.iter().any(|a| a == "--append-system-prompt"));
    }

    #[test]
    fn system_prompt_preset_with_append() {
        let mut o = base();
        o.system_prompt = Some(SystemPrompt::Preset(SystemPromptPreset {
            preset: "claude_code".into(),
            append: Some("Be concise.".into()),
            exclude_dynamic_sections: None,
        }));
        let cmd = build_command("/c", &o);
        assert!(has_pair(&cmd, "--append-system-prompt", "Be concise."));
    }

    #[test]
    fn system_prompt_file() {
        let mut o = base();
        o.system_prompt = Some(SystemPrompt::File("/path/to/prompt.md".into()));
        let cmd = build_command("/c", &o);
        assert!(has_pair(&cmd, "--system-prompt-file", "/path/to/prompt.md"));
        assert!(!cmd.iter().any(|a| a == "--system-prompt"));
    }

    #[test]
    fn task_budget_toggles() {
        let mut o = base();
        o.task_budget = Some(TaskBudget { total: 100_000 });
        assert!(has_pair(
            &build_command("/c", &o),
            "--task-budget",
            "100000"
        ));
        assert!(!build_command("/c", &base())
            .iter()
            .any(|a| a == "--task-budget"));
    }

    #[test]
    fn deprecated_max_thinking_tokens() {
        let mut o = base();
        o.max_thinking_tokens = Some(5000);
        assert!(has_pair(
            &build_command("/c", &o),
            "--max-thinking-tokens",
            "5000"
        ));
    }

    #[test]
    fn thinking_variants() {
        let mut adaptive = base();
        adaptive.thinking = Some(ThinkingConfig::Adaptive { display: None });
        let cmd = build_command("/c", &adaptive);
        assert!(has_pair(&cmd, "--thinking", "adaptive"));
        assert!(!cmd.iter().any(|a| a == "--max-thinking-tokens"));

        let mut disabled = base();
        disabled.thinking = Some(ThinkingConfig::Disabled);
        assert!(has_pair(
            &build_command("/c", &disabled),
            "--thinking",
            "disabled"
        ));

        // thinking takes precedence over the deprecated max_thinking_tokens.
        let mut both = base();
        both.thinking = Some(ThinkingConfig::Enabled {
            budget_tokens: 5000,
            display: None,
        });
        both.max_thinking_tokens = Some(999);
        let cmd = build_command("/c", &both);
        assert!(has_pair(&cmd, "--max-thinking-tokens", "5000"));
        assert!(!cmd.iter().any(|a| a == "999"));
    }

    #[test]
    fn thinking_display_forwarded_only_when_present() {
        let mut with = base();
        with.thinking = Some(ThinkingConfig::Adaptive {
            display: Some(ThinkingDisplay::Summarized),
        });
        assert!(has_pair(
            &build_command("/c", &with),
            "--thinking-display",
            "summarized"
        ));

        let mut without = base();
        without.thinking = Some(ThinkingConfig::Adaptive { display: None });
        assert!(!build_command("/c", &without)
            .iter()
            .any(|a| a == "--thinking-display"));
    }

    #[test]
    fn tools_list_empty_preset_and_absent() {
        let mut list = base();
        list.tools = Some(ToolsConfig::List(vec!["Bash".into(), "Read".into()]));
        assert!(has_pair(
            &build_command("/c", &list),
            "--tools",
            "Bash,Read"
        ));

        let mut empty = base();
        empty.tools = Some(ToolsConfig::List(vec![]));
        assert!(has_pair(&build_command("/c", &empty), "--tools", ""));

        let mut preset = base();
        preset.tools = Some(ToolsConfig::Preset);
        assert!(has_pair(
            &build_command("/c", &preset),
            "--tools",
            "default"
        ));

        assert!(!build_command("/c", &base()).iter().any(|a| a == "--tools"));
    }

    #[test]
    fn betas_and_add_dirs_and_plugins() {
        let mut o = base();
        o.betas = vec![SdkBeta::Context1m20250807];
        o.add_dirs = vec!["/a".into(), "/b".into()];
        o.plugins = vec![SdkPluginConfig::local("/plug")];
        let cmd = build_command("/c", &o);
        assert!(has_pair(&cmd, "--betas", "context-1m-2025-08-07"));
        assert!(has_pair(&cmd, "--add-dir", "/a"));
        assert!(has_pair(&cmd, "--add-dir", "/b"));
        assert!(has_pair(&cmd, "--plugin-dir", "/plug"));
    }

    #[test]
    fn partial_messages_fork_session_mirror_and_json_schema() {
        let mut o = base();
        o.include_partial_messages = true;
        o.fork_session = true;
        o.session_store = Some(std::sync::Arc::new(crate::store::InMemorySessionStore::new()));
        let cmd = build_command("/c", &o);
        assert!(cmd.contains(&"--include-partial-messages".to_string()));
        assert!(cmd.contains(&"--fork-session".to_string()));
        assert!(cmd.contains(&"--session-mirror".to_string())); // from session_store
    }

    #[test]
    fn mcp_servers_map_and_path() {
        let mut map = base();
        let mut servers = HashMap::new();
        servers.insert(
            "fs".to_string(),
            McpServerConfig::Stdio(McpStdioServerConfig {
                command: "node".into(),
                args: vec![],
                env: HashMap::new(),
            }),
        );
        map.mcp_servers = McpServers::Map(servers);
        let cmd = build_command("/c", &map);
        let idx = cmd.iter().position(|a| a == "--mcp-config").unwrap();
        assert!(cmd[idx + 1].contains("\"mcpServers\""));
        assert!(cmd[idx + 1].contains("\"fs\""));

        let mut path = base();
        path.mcp_servers = McpServers::Path("/cfg.json".into());
        assert!(has_pair(
            &build_command("/c", &path),
            "--mcp-config",
            "/cfg.json"
        ));
    }

    #[test]
    fn mcp_config_tempfile_routes_inline_json_on_windows_only() {
        let mut o = base();
        let mut servers = HashMap::new();
        servers.insert(
            "fs".to_string(),
            McpServerConfig::Stdio(McpStdioServerConfig {
                command: "node".into(),
                args: vec![],
                env: HashMap::new(),
            }),
        );
        o.mcp_servers = McpServers::Map(servers);
        let mut cmd = build_command("/c", &o);
        let mut transport = SubprocessCliTransport::new(o);
        transport
            .route_mcp_config_through_tempfile_on_windows(&mut cmd)
            .unwrap();
        let idx = cmd.iter().position(|a| a == "--mcp-config").unwrap();
        if cfg!(windows) {
            let path = &cmd[idx + 1];
            assert!(!path.trim_start().starts_with('{'));
            let contents = std::fs::read_to_string(path).unwrap();
            assert!(contents.contains("\"mcpServers\""));
            assert!(contents.contains("\"fs\""));
            assert!(transport.mcp_config_tempfile.is_some());
        } else {
            assert!(cmd[idx + 1].trim_start().starts_with('{'));
            assert!(transport.mcp_config_tempfile.is_none());
        }
    }

    #[test]
    fn mcp_config_tempfile_noop_for_path_variant() {
        let mut o = base();
        o.mcp_servers = McpServers::Path("/cfg.json".into());
        let mut cmd = build_command("/c", &o);
        let mut transport = SubprocessCliTransport::new(o);
        transport
            .route_mcp_config_through_tempfile_on_windows(&mut cmd)
            .unwrap();
        let idx = cmd.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(cmd[idx + 1], "/cfg.json");
        assert!(transport.mcp_config_tempfile.is_none());
    }

    #[test]
    fn sandbox_merges_into_settings_json() {
        let mut o = base();
        o.sandbox = Some(SandboxSettings {
            enabled: Some(true),
            ..Default::default()
        });
        let cmd = build_command("/c", &o);
        let idx = cmd.iter().position(|a| a == "--settings").unwrap();
        assert!(cmd[idx + 1].contains("\"sandbox\""));
        assert!(cmd[idx + 1].contains("\"enabled\":true"));
    }

    #[test]
    fn output_format_json_schema() {
        let mut o = base();
        o.output_format = Some(
            serde_json::json!({"type": "json_schema", "schema": {"type": "object"}})
                .as_object()
                .unwrap()
                .clone(),
        );
        let cmd = build_command("/c", &o);
        let idx = cmd.iter().position(|a| a == "--json-schema").unwrap();
        assert_eq!(cmd[idx + 1], "{\"type\":\"object\"}");
    }

    #[test]
    fn skills_named_list_and_no_duplicates() {
        let mut o = base();
        o.skills = Some(Skills::List(vec!["a".into(), "b".into()]));
        o.allowed_tools = vec!["Skill(a)".into(), "Read".into()];
        let cmd = build_command("/c", &o);
        let idx = cmd.iter().position(|a| a == "--allowedTools").unwrap();
        let allowed = &cmd[idx + 1];
        // Existing Skill(a) not duplicated; Skill(b) appended.
        assert_eq!(allowed.matches("Skill(a)").count(), 1);
        assert!(allowed.contains("Skill(b)"));
        assert!(allowed.contains("Read"));
    }

    #[test]
    fn skills_preserve_user_setting_sources() {
        use crate::types::SettingSource;
        let mut o = base();
        o.skills = Some(Skills::All);
        o.setting_sources = Some(vec![SettingSource::Project]);
        let cmd = build_command("/c", &o);
        assert!(cmd.iter().any(|a| a == "--setting-sources=project"));
    }

    #[test]
    fn agents_never_passed_as_cli_flag() {
        // Agents are sent via the initialize control request, not a CLI flag.
        let mut o = base();
        let mut agents = HashMap::new();
        agents.insert(
            "helper".to_string(),
            AgentDefinition::new("A helper", "Be helpful"),
        );
        o.agents = Some(agents);
        let cmd = build_command("/c", &o);
        assert!(!cmd.iter().any(|a| a == "--agents"));
    }

    // --- Stdout framing (pump_stdout), ported from test_subprocess_buffering.py ---

    async fn pump(data: &'static [u8], max: usize) -> Vec<Result<Value>> {
        let (tx, mut rx) = mpsc::channel(64);
        let (_keep, mut shutdown) = oneshot::channel::<()>();
        pump_stdout(data, &mut shutdown, max, &tx).await;
        drop(tx);
        let mut out = Vec::new();
        while let Some(item) = rx.recv().await {
            out.push(item);
        }
        out
    }

    fn oks(out: &[Result<Value>]) -> Vec<Value> {
        out.iter()
            .filter_map(|r| r.as_ref().ok().cloned())
            .collect()
    }

    #[tokio::test]
    async fn framing_multiple_messages_and_blank_lines() {
        let out = pump(b"{\"n\":1}\n\n{\"n\":2}\n", DEFAULT_MAX_BUFFER_SIZE).await;
        let vals = oks(&out);
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0]["n"], 1);
        assert_eq!(vals[1]["n"], 2);
    }

    #[tokio::test]
    async fn framing_crlf_and_non_json_skipped() {
        let out = pump(
            b"{\"a\":1}\r\n[SandboxDebug] noise\n{\"b\":2}\n",
            DEFAULT_MAX_BUFFER_SIZE,
        )
        .await;
        assert_eq!(oks(&out).len(), 2); // CRLF trimmed, debug line skipped
    }

    #[tokio::test]
    async fn framing_embedded_escaped_newline() {
        // A literal newline inside a JSON string is escaped on the wire.
        let out = pump(b"{\"text\":\"a\\nb\"}\n", DEFAULT_MAX_BUFFER_SIZE).await;
        let vals = oks(&out);
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0]["text"], "a\nb");
    }

    #[tokio::test]
    async fn framing_final_line_without_newline_yielded() {
        let out = pump(b"{\"n\":1}\n{\"n\":2}", DEFAULT_MAX_BUFFER_SIZE).await;
        assert_eq!(oks(&out).len(), 2);
    }

    #[tokio::test]
    async fn framing_truncated_final_line_dropped_not_raised() {
        // Valid line, then a cut-off final line with no newline: the tail is
        // dropped (no error surfaced).
        let out = pump(b"{\"n\":1}\n{\"n\":2", DEFAULT_MAX_BUFFER_SIZE).await;
        assert_eq!(oks(&out).len(), 1);
        assert!(out.iter().all(|r| r.is_ok()));
    }

    #[tokio::test]
    async fn framing_complete_corrupt_line_raises() {
        let out = pump(b"{\"n\":1}\n{bad json}\n", DEFAULT_MAX_BUFFER_SIZE).await;
        assert_eq!(oks(&out).len(), 1);
        assert!(out
            .iter()
            .any(|r| matches!(r, Err(Error::JsonDecode { .. }))));
    }

    #[tokio::test]
    async fn framing_oversized_line_raises() {
        // A complete line larger than the (tiny) buffer surfaces a decode error.
        let out = pump(b"{\"x\":\"aaaaaaaaaaaaaaaaaaaa\"}\n", 8).await;
        assert!(out
            .iter()
            .any(|r| matches!(r, Err(Error::JsonDecode { .. }))));
    }
}
