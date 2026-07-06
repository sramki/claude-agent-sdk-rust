//! Path resolution: config home, project-directory sanitization, the djb2
//! `simple_hash`, UUID validation, and git-worktree discovery.
//!
//! Ported from the path-handling helpers in the Python
//! `_internal/sessions.py`.

use std::path::{Path, PathBuf};
use std::process::Command;

use unicode_normalization::UnicodeNormalization;

/// Maximum length for a single sanitized path component before it is truncated
/// and given a hash suffix. Most filesystems cap a component at 255 bytes; 200
/// leaves room for the hash suffix and separator.
pub(crate) const MAX_SANITIZED_LENGTH: usize = 200;

/// NFC-normalize a string (matches Python `unicodedata.normalize("NFC", ...)`).
pub(crate) fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Best-effort home directory (`$HOME` on Unix, `%USERPROFILE%` on Windows).
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

/// Returns the Claude config directory, respecting `CLAUDE_CONFIG_DIR`.
///
/// Mirrors `_get_claude_config_home_dir`.
pub(crate) fn claude_config_home_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(nfc(&dir.to_string_lossy()));
        }
    }
    let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
    PathBuf::from(nfc(&home.join(".claude").to_string_lossy()))
}

/// Returns the `<config_home>/projects` directory. Mirrors `_get_projects_dir`.
pub(crate) fn projects_dir() -> PathBuf {
    claude_config_home_dir().join("projects")
}

/// 32-bit integer djb2 hash to base36, matching the CLI's directory naming.
///
/// Faithful port of `_simple_hash`: `h = (h << 5) - h + char`, coerced to a
/// 32-bit signed integer each step (emulating JS `h |= 0`), then `abs()`, then
/// base-36.
pub(crate) fn simple_hash(s: &str) -> String {
    let mut h: i64 = 0;
    for ch in s.chars() {
        let c = ch as i64;
        h = (h << 5) - h + c;
        h &= 0xFFFF_FFFF;
        if h >= 0x8000_0000 {
            h -= 0x1_0000_0000;
        }
    }
    let mut n = h.abs();
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    // Safe: DIGITS are all ASCII.
    String::from_utf8(out).unwrap()
}

/// Makes a string safe for use as a directory name. Mirrors `_sanitize_path`:
/// every char not in `[A-Za-z0-9]` becomes `-`; results longer than
/// [`MAX_SANITIZED_LENGTH`] are truncated and given a `-<hash>` suffix.
pub(crate) fn sanitize_path(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return sanitized;
    }
    // `sanitized` is pure ASCII (alnum or '-'), so byte slicing at
    // MAX_SANITIZED_LENGTH lands on a char boundary.
    let prefix = &sanitized[..MAX_SANITIZED_LENGTH];
    format!("{}-{}", prefix, simple_hash(name))
}

/// Returns `true` if `s` is a valid UUID (`8-4-4-4-12` hex, case-insensitive).
/// Mirrors `_validate_uuid`.
pub(crate) fn validate_uuid(s: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != GROUPS.len() {
        return false;
    }
    parts
        .iter()
        .zip(GROUPS.iter())
        .all(|(p, &len)| p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Resolves a directory to its canonical form (realpath + NFC).
///
/// Mirrors `_canonicalize_path`. Unlike Python's `os.path.realpath`, Rust's
/// [`std::fs::canonicalize`] requires the path to exist; when it does not, we
/// fall back to the NFC-normalized input (matching Python's `except OSError`).
pub(crate) fn canonicalize_path(d: &Path) -> String {
    match std::fs::canonicalize(d) {
        Ok(p) => nfc(&p.to_string_lossy()),
        Err(_) => nfc(&d.to_string_lossy()),
    }
}

/// `<projects>/<sanitize(project_path)>`. Mirrors `_get_project_dir`.
pub(crate) fn project_dir(project_path: &str) -> PathBuf {
    projects_dir().join(sanitize_path(project_path))
}

/// Finds the project directory for a path, tolerating Bun-vs-Node hash
/// mismatches on long (>200 char) paths via prefix scanning.
///
/// Mirrors `_find_project_dir`.
pub(crate) fn find_project_dir(project_path: &str) -> Option<PathBuf> {
    let exact = project_dir(project_path);
    if exact.is_dir() {
        return Some(exact);
    }

    let sanitized = sanitize_path(project_path);
    if sanitized.len() <= MAX_SANITIZED_LENGTH {
        return None;
    }

    let prefix = format!("{}-", &sanitized[..MAX_SANITIZED_LENGTH]);
    let entries = std::fs::read_dir(projects_dir()).ok()?;
    for entry in entries.flatten() {
        if entry.path().is_dir() && entry.file_name().to_string_lossy().starts_with(&prefix) {
            return Some(entry.path());
        }
    }
    None
}

/// Returns absolute worktree paths for the git repo containing `cwd`, or an
/// empty list if git is unavailable or `cwd` is not in a repo.
///
/// Mirrors `_get_worktree_paths`. Note: unlike the Python version there is no
/// 5-second subprocess timeout (Rust's std has no built-in wait-with-timeout);
/// a hung `git` would block. Acceptable for local read tooling.
pub(crate) fn worktree_paths(cwd: &str) -> Vec<String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output();

    let output = match output {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => o,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(nfc))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_uuid_valid() {
        assert!(validate_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(validate_uuid("550E8400-E29B-41D4-A716-446655440000"));
    }

    #[test]
    fn validate_uuid_invalid() {
        assert!(!validate_uuid("not-a-uuid"));
        assert!(!validate_uuid(""));
        assert!(!validate_uuid("550e8400-e29b-41d4-a716"));
    }

    #[test]
    fn sanitize_path_basic() {
        assert_eq!(sanitize_path("/Users/foo/my-project"), "-Users-foo-my-project");
        assert_eq!(sanitize_path("plugin:name:server"), "plugin-name-server");
    }

    #[test]
    fn sanitize_path_long() {
        let long_path = "/x".repeat(150); // 300 chars
        let result = sanitize_path(&long_path);
        assert!(result.len() > MAX_SANITIZED_LENGTH);
        assert!(result.starts_with("-x-x"));
        assert!(result[MAX_SANITIZED_LENGTH..].contains('-'));
    }

    #[test]
    fn simple_hash_deterministic() {
        assert_eq!(simple_hash("hello"), simple_hash("hello"));
        assert_ne!(simple_hash("hello"), simple_hash("world"));
    }

    #[test]
    fn simple_hash_zero() {
        assert_eq!(simple_hash(""), "0");
    }

    #[test]
    fn simple_hash_matches_js_djb2_vectors() {
        // Reference values from the JS/Python `simpleHash` implementation.
        // h = ((h<<5) - h + c) coerced to int32 each step, abs, base36.
        // "a" => 97 => base36 "2p"
        assert_eq!(simple_hash("a"), "2p");
        // "hello": folding h = h*31 + c gives 99162322 => base36 "1n1e4y"
        assert_eq!(simple_hash("hello"), "1n1e4y");
    }
}
