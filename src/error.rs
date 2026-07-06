//! Error types for the SDK.
//!
//! Idiomatic Rust counterpart of the Python `_errors.py` hierarchy. Python
//! exposes a class tree — `ClaudeSDKError` (base) → `CLIConnectionError` →
//! `CLINotFoundError`, plus `ProcessError`, `CLIJSONDecodeError`, and
//! `MessageParseError`. Here that becomes a single [`Error`] enum whose variants
//! carry the same fields, which is the conventional Rust shape for a library
//! error. The variant names mirror the upstream class names so the mapping is
//! obvious.

use std::fmt;

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors surfaced by the SDK.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The provided session id is not a valid UUID.
    ///
    /// The session reader treats a *missing* session as a normal `Ok(None)` /
    /// `Ok(vec![])`, but a *malformed* id is a caller error and surfaces here.
    #[error("invalid session id: {0}")]
    InvalidSessionId(String),

    /// An empty agent id was passed to a subagent lookup.
    #[error("invalid agent id: must not be empty")]
    InvalidAgentId,

    /// Unable to connect to Claude Code. Mirrors `CLIConnectionError`.
    #[error("{0}")]
    Connection(String),

    /// Claude Code was not found or is not installed. Mirrors
    /// `CLINotFoundError` (a subtype of connection error upstream).
    #[error("{}", format_cli_not_found(.message, .cli_path))]
    CliNotFound {
        /// Human-readable message (defaults to `"Claude Code not found"`).
        message: String,
        /// The path that was probed, if known.
        cli_path: Option<String>,
    },

    /// The CLI process failed. Mirrors `ProcessError`.
    #[error("{}", format_process_error(.message, *.exit_code, .stderr))]
    Process {
        /// Base message.
        message: String,
        /// Process exit code, if the process exited.
        exit_code: Option<i32>,
        /// Captured standard error, if any.
        stderr: Option<String>,
    },

    /// Unable to decode a JSON line from CLI output. Mirrors
    /// `CLIJSONDecodeError`.
    #[error("Failed to decode JSON: {}...", truncate(.line, 100))]
    JsonDecode {
        /// The offending line.
        line: String,
        /// The underlying decode error.
        #[source]
        source: serde_json::Error,
    },

    /// Unable to parse a message from CLI output. Mirrors `MessageParseError`.
    #[error("{message}")]
    MessageParse {
        /// Description of what failed to parse.
        message: String,
        /// The raw data that could not be parsed, if available.
        data: Option<serde_json::Value>,
    },

    /// An underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Constructs a [`Error::CliNotFound`] with the default message.
    pub fn cli_not_found(cli_path: Option<impl Into<String>>) -> Self {
        Error::CliNotFound {
            message: "Claude Code not found".to_string(),
            cli_path: cli_path.map(Into::into),
        }
    }

    /// Constructs a [`Error::Connection`].
    pub fn connection(message: impl Into<String>) -> Self {
        Error::Connection(message.into())
    }

    /// Constructs a [`Error::Process`].
    pub fn process(
        message: impl Into<String>,
        exit_code: Option<i32>,
        stderr: Option<String>,
    ) -> Self {
        Error::Process {
            message: message.into(),
            exit_code,
            stderr,
        }
    }

    /// Constructs a [`Error::JsonDecode`] from the offending line and error.
    pub fn json_decode(line: impl Into<String>, source: serde_json::Error) -> Self {
        Error::JsonDecode {
            line: line.into(),
            source,
        }
    }

    /// Constructs a [`Error::MessageParse`].
    pub fn message_parse(message: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Error::MessageParse {
            message: message.into(),
            data,
        }
    }
}

fn truncate(s: &str, n: usize) -> TruncatedDisplay<'_> {
    TruncatedDisplay { s, n }
}

/// Displays at most `n` chars of `s` (by char, matching Python's `line[:100]`).
struct TruncatedDisplay<'a> {
    s: &'a str,
    n: usize,
}

impl fmt::Display for TruncatedDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for ch in self.s.chars().take(self.n) {
            f.write_str(ch.encode_utf8(&mut [0u8; 4]))?;
        }
        Ok(())
    }
}

fn format_cli_not_found(message: &str, cli_path: &Option<String>) -> String {
    match cli_path {
        Some(p) => format!("{message}: {p}"),
        None => message.to_string(),
    }
}

fn format_process_error(message: &str, exit_code: Option<i32>, stderr: &Option<String>) -> String {
    let mut msg = message.to_string();
    if let Some(code) = exit_code {
        msg = format!("{msg} (exit code: {code})");
    }
    if let Some(err) = stderr {
        msg = format!("{msg}\nError output: {err}");
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_not_found_with_path() {
        let e = Error::cli_not_found(Some("/usr/bin/claude"));
        assert_eq!(e.to_string(), "Claude Code not found: /usr/bin/claude");
    }

    #[test]
    fn cli_not_found_without_path() {
        let e = Error::cli_not_found(None::<String>);
        assert_eq!(e.to_string(), "Claude Code not found");
    }

    #[test]
    fn process_error_formats_code_and_stderr() {
        let e = Error::process("Command failed", Some(2), Some("boom".to_string()));
        assert_eq!(
            e.to_string(),
            "Command failed (exit code: 2)\nError output: boom"
        );
    }

    #[test]
    fn process_error_message_only() {
        let e = Error::process("Command failed", None, None);
        assert_eq!(e.to_string(), "Command failed");
    }

    #[test]
    fn json_decode_truncates_to_100_chars() {
        let line = "x".repeat(250);
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let e = Error::json_decode(line, source);
        let msg = e.to_string();
        assert!(msg.starts_with("Failed to decode JSON: "));
        assert!(msg.ends_with("..."));
        // 100 x's between prefix and the "..." suffix.
        assert_eq!(msg.matches('x').count(), 100);
    }

    #[test]
    fn invalid_session_id_display() {
        let e = Error::InvalidSessionId("nope".to_string());
        assert_eq!(e.to_string(), "invalid session id: nope");
    }
}
