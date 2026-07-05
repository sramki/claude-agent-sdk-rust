//! JSONL parsing helpers: substring field extraction (no full parse), the
//! first-prompt heuristic, lite head/tail reads, `SessionInfo` derivation, and
//! ISO-timestamp parsing.
//!
//! Ported from the extraction + lite-read helpers in the Python
//! `claude_agent_sdk/_internal/sessions.py`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::types::SessionInfo;

/// Size of the head/tail buffer for lite metadata reads.
pub(crate) const LITE_READ_BUF_SIZE: usize = 65536;

/// Result of reading a session file's head, tail, mtime, and size.
pub(crate) struct LiteSessionFile {
    pub mtime: i64,
    pub size: u64,
    pub head: String,
    pub tail: String,
}

// ---------------------------------------------------------------------------
// Substring search
// ---------------------------------------------------------------------------

/// Finds `needle` in `haystack` at or after byte index `from`.
fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

// ---------------------------------------------------------------------------
// JSON string field extraction — no full parse, works on truncated lines
// ---------------------------------------------------------------------------

/// Unescape a JSON string value extracted as raw text. Mirrors
/// `_unescape_json_string`.
fn unescape_json_string(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    match serde_json::from_str::<String>(&format!("\"{raw}\"")) {
        Ok(s) => s,
        Err(_) => raw.to_string(),
    }
}

/// Scans from `value_start` for the closing unescaped quote; returns the
/// unescaped value and the index of the closing quote.
fn scan_string_value(bytes: &[u8], text: &str, value_start: usize) -> (Option<String>, usize) {
    let mut i = value_start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'"' => {
                return (Some(unescape_json_string(&text[value_start..i])), i);
            }
            _ => i += 1,
        }
    }
    (None, i)
}

/// Extracts a simple JSON string field value without full parsing. Looks for
/// `"key":"value"` or `"key": "value"`. Returns the first match, or `None`.
/// Mirrors `_extract_json_string_field`.
pub(crate) fn extract_json_string_field(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for pattern in [format!("\"{key}\":\""), format!("\"{key}\": \"")] {
        if let Some(idx) = find_from(bytes, pattern.as_bytes(), 0) {
            let value_start = idx + pattern.len();
            if let (Some(value), _) = scan_string_value(bytes, text, value_start) {
                return Some(value);
            }
        }
    }
    None
}

/// Like [`extract_json_string_field`] but finds the LAST occurrence. Mirrors
/// `_extract_last_json_string_field`.
pub(crate) fn extract_last_json_string_field(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut last: Option<String> = None;
    for pattern in [format!("\"{key}\":\""), format!("\"{key}\": \"")] {
        let mut search_from = 0;
        while let Some(idx) = find_from(bytes, pattern.as_bytes(), search_from) {
            let value_start = idx + pattern.len();
            let (value, end) = scan_string_value(bytes, text, value_start);
            if let Some(v) = value {
                last = Some(v);
            }
            search_from = end + 1;
        }
    }
    last
}

/// Maps `Some("")` to `None`, mirroring Python's `x or None` falsy handling.
fn nonempty(o: Option<String>) -> Option<String> {
    o.filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// First prompt extraction from head chunk
// ---------------------------------------------------------------------------

/// Extracts the `<command-name>...</command-name>` inner text (non-greedy).
fn command_name(s: &str) -> Option<String> {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    let start = s.find(OPEN)? + OPEN.len();
    let rel_end = s[start..].find(CLOSE)?;
    Some(s[start..start + rel_end].to_string())
}

/// Matches the auto-generated / system message patterns skipped when hunting
/// for the first meaningful prompt. Mirrors `_SKIP_FIRST_PROMPT_PATTERN`.
fn matches_skip_first_prompt(s: &str) -> bool {
    for prefix in ["<local-command-stdout>", "<session-start-hook>", "<tick>", "<goal>"] {
        if s.starts_with(prefix) {
            return true;
        }
    }
    const REQ: &str = "[Request interrupted by user";
    if let Some(rest) = s.strip_prefix(REQ) {
        // \[Request interrupted by user[^\]]*\] — a closing ']' must follow.
        if rest.contains(']') {
            return true;
        }
    }
    let t = s.trim();
    if t.starts_with("<ide_opened_file>") && t.ends_with("</ide_opened_file>") {
        return true;
    }
    if t.starts_with("<ide_selection>") && t.ends_with("</ide_selection>") {
        return true;
    }
    false
}

/// Extracts the first meaningful user prompt from a JSONL head chunk. Mirrors
/// `_extract_first_prompt_from_head`. Truncates to 200 chars + ellipsis.
pub(crate) fn extract_first_prompt_from_head(head: &str) -> String {
    let mut command_fallback = String::new();

    for line in head.split('\n') {
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\": \"user\"") {
            continue;
        }
        if line.contains("\"tool_result\"") {
            continue;
        }
        if line.contains("\"isMeta\":true") || line.contains("\"isMeta\": true") {
            continue;
        }
        if line.contains("\"isCompactSummary\":true") || line.contains("\"isCompactSummary\": true")
        {
            continue;
        }

        let entry: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !entry.is_object() || entry.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let message = match entry.get("message") {
            Some(m) if m.is_object() => m,
            _ => continue,
        };

        let mut texts: Vec<String> = Vec::new();
        match message.get("content") {
            Some(Value::String(s)) => texts.push(s.clone()),
            Some(Value::Array(arr)) => {
                for block in arr {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            texts.push(t.to_string());
                        }
                    }
                }
            }
            _ => {}
        }

        for raw in texts {
            let result = raw.replace('\n', " ");
            let result = result.trim();
            if result.is_empty() {
                continue;
            }
            if let Some(cmd) = command_name(result) {
                if command_fallback.is_empty() {
                    command_fallback = cmd;
                }
                continue;
            }
            if matches_skip_first_prompt(result) {
                continue;
            }
            if result.chars().count() > 200 {
                let truncated: String = result.chars().take(200).collect();
                return format!("{}\u{2026}", truncated.trim_end());
            }
            return result.to_string();
        }
    }

    command_fallback
}

// ---------------------------------------------------------------------------
// File I/O — read head and tail of a file
// ---------------------------------------------------------------------------

/// Reads up to `buf.len()` bytes, looping to fill across short reads. Returns
/// the number of bytes read (may be less than `buf.len()` at EOF).
fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Opens a session file, stats it, and reads head + tail. Returns `None` on any
/// error or if the file is empty. Mirrors `_read_session_lite`.
pub(crate) fn read_session_lite(path: &Path) -> Option<LiteSessionFile> {
    let mut f = File::open(path).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let size = meta.len();
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(UNIX_EPOCH).ok()?;
    let mtime = (dur.as_secs() as i64) * 1000 + i64::from(dur.subsec_millis());

    let mut head_buf = vec![0u8; LITE_READ_BUF_SIZE];
    let head_n = read_up_to(&mut f, &mut head_buf).ok()?;
    if head_n == 0 {
        return None;
    }
    head_buf.truncate(head_n);
    let head = String::from_utf8_lossy(&head_buf).into_owned();

    let tail_offset = size.saturating_sub(LITE_READ_BUF_SIZE as u64);
    let tail = if tail_offset == 0 {
        head.clone()
    } else {
        f.seek(SeekFrom::Start(tail_offset)).ok()?;
        let mut tail_buf = vec![0u8; LITE_READ_BUF_SIZE];
        let tail_n = read_up_to(&mut f, &mut tail_buf).unwrap_or(0);
        tail_buf.truncate(tail_n);
        String::from_utf8_lossy(&tail_buf).into_owned()
    };

    Some(LiteSessionFile {
        mtime,
        size,
        head,
        tail,
    })
}

// ---------------------------------------------------------------------------
// ISO timestamp parsing → epoch milliseconds
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(mut y: i64, m: i64, d: i64) -> i64 {
    if m <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parses an ISO-8601 timestamp to epoch milliseconds. Mirrors the
/// `datetime.fromisoformat(...).timestamp() * 1000` path (with `Z` handling).
/// Returns `None` on any format deviation. A missing timezone is treated as
/// UTC (Python would use local time for a naive datetime; real transcripts
/// always carry `Z`).
pub(crate) fn parse_iso_to_epoch_ms(ts: &str) -> Option<i64> {
    let normalized = if let Some(stripped) = ts.strip_suffix('Z') {
        format!("{stripped}+00:00")
    } else {
        ts.to_string()
    };

    let (date, time_part) = normalized.split_once('T')?;

    let date_fields: Vec<&str> = date.split('-').collect();
    if date_fields.len() != 3 {
        return None;
    }
    let year: i64 = date_fields[0].parse().ok()?;
    let month: i64 = date_fields[1].parse().ok()?;
    let day: i64 = date_fields[2].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Split off the timezone offset (a '+' or '-' after the time).
    let (time_only, offset_sec) = match time_part.find(['+', '-']) {
        Some(i) => (&time_part[..i], parse_offset_seconds(&time_part[i..])?),
        None => (time_part, 0),
    };

    let time_fields: Vec<&str> = time_only.split(':').collect();
    if time_fields.len() != 3 {
        return None;
    }
    let hour: i64 = time_fields[0].parse().ok()?;
    let minute: i64 = time_fields[1].parse().ok()?;
    let (sec_str, frac_ms) = match time_fields[2].split_once('.') {
        Some((s, frac)) => (s, parse_fraction_ms(frac)?),
        None => (time_fields[2], 0),
    };
    let second: i64 = sec_str.parse().ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let local_seconds =
        days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second;
    let epoch_seconds = local_seconds - offset_sec;
    Some(epoch_seconds * 1000 + frac_ms)
}

/// Parses a `±HH:MM` (or `±HHMM`) offset into signed seconds.
fn parse_offset_seconds(tz: &str) -> Option<i64> {
    let (sign, rest) = match tz.as_bytes().first()? {
        b'+' => (1, &tz[1..]),
        b'-' => (-1, &tz[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 2 {
        return None;
    }
    let hh: i64 = digits[..2].parse().ok()?;
    let mm: i64 = if digits.len() >= 4 {
        digits[2..4].parse().ok()?
    } else {
        0
    };
    Some(sign * (hh * 3600 + mm * 60))
}

/// Parses fractional seconds (digits after the `.`) into milliseconds.
fn parse_fraction_ms(frac: &str) -> Option<i64> {
    if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut ms_digits: String = frac.chars().take(3).collect();
    while ms_digits.len() < 3 {
        ms_digits.push('0');
    }
    ms_digits.parse().ok()
}

// ---------------------------------------------------------------------------
// SessionInfo derivation from a lite read
// ---------------------------------------------------------------------------

/// Parses [`SessionInfo`] fields from a lite read. Returns `None` for sidechain
/// sessions or metadata-only sessions with no extractable summary. Mirrors
/// `_parse_session_info_from_lite`.
pub(crate) fn parse_session_info_from_lite(
    session_id: &str,
    lite: &LiteSessionFile,
    project_path: Option<&str>,
) -> Option<SessionInfo> {
    let head = &lite.head;
    let tail = &lite.tail;

    let first_line = match head.find('\n') {
        Some(i) => &head[..i],
        None => head,
    };
    if first_line.contains("\"isSidechain\":true") || first_line.contains("\"isSidechain\": true") {
        return None;
    }

    let custom_title = nonempty(extract_last_json_string_field(tail, "customTitle"))
        .or_else(|| nonempty(extract_last_json_string_field(head, "customTitle")))
        .or_else(|| nonempty(extract_last_json_string_field(tail, "aiTitle")))
        .or_else(|| nonempty(extract_last_json_string_field(head, "aiTitle")));

    let first_prompt = nonempty(Some(extract_first_prompt_from_head(head)));

    let summary = custom_title
        .clone()
        .or_else(|| nonempty(extract_last_json_string_field(tail, "lastPrompt")))
        .or_else(|| nonempty(extract_last_json_string_field(tail, "summary")))
        .or_else(|| first_prompt.clone());

    let summary = match summary {
        Some(s) if !s.is_empty() => s,
        _ => return None,
    };

    let git_branch = nonempty(extract_last_json_string_field(tail, "gitBranch"))
        .or_else(|| nonempty(extract_json_string_field(head, "gitBranch")));

    let session_cwd = nonempty(extract_json_string_field(head, "cwd"))
        .or_else(|| project_path.filter(|p| !p.is_empty()).map(str::to_string));

    let tag_line = tail
        .split('\n')
        .rev()
        .find(|line| line.starts_with("{\"type\":\"tag\""));
    let tag = tag_line.and_then(|line| nonempty(extract_last_json_string_field(line, "tag")));

    let created_at = nonempty(extract_json_string_field(head, "timestamp"))
        .and_then(|ts| parse_iso_to_epoch_ms(&ts));

    Some(SessionInfo {
        session_id: session_id.to_string(),
        summary,
        last_modified: lite.mtime,
        file_size: Some(lite.size),
        custom_title,
        first_prompt,
        git_branch,
        cwd: session_cwd,
        tag,
        created_at,
    })
}

// ---------------------------------------------------------------------------
// In-memory lite view (test helper)
// ---------------------------------------------------------------------------

/// Builds a lite view from an in-memory JSONL string, matching
/// [`read_session_lite`]'s byte semantics. Mirrors `_jsonl_to_lite`; used by
/// unit tests that exercise the parse path without touching disk.
#[cfg(test)]
pub(crate) fn jsonl_to_lite(jsonl: &str, mtime: i64) -> LiteSessionFile {
    let buf = jsonl.as_bytes();
    let size = buf.len() as u64;
    let head = String::from_utf8_lossy(&buf[..buf.len().min(LITE_READ_BUF_SIZE)]).into_owned();
    let tail = if buf.len() > LITE_READ_BUF_SIZE {
        String::from_utf8_lossy(&buf[buf.len() - LITE_READ_BUF_SIZE..]).into_owned()
    } else {
        head.clone()
    };
    LiteSessionFile {
        mtime,
        size,
        head,
        tail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_string_field_simple() {
        let text = r#"{"foo":"bar","baz":"qux"}"#;
        assert_eq!(extract_json_string_field(text, "foo").as_deref(), Some("bar"));
        assert_eq!(extract_json_string_field(text, "baz").as_deref(), Some("qux"));
        assert_eq!(extract_json_string_field(text, "missing"), None);
    }

    #[test]
    fn extract_json_string_field_with_space() {
        let text = r#"{"foo": "bar"}"#;
        assert_eq!(extract_json_string_field(text, "foo").as_deref(), Some("bar"));
    }

    #[test]
    fn extract_json_string_field_escaped() {
        let text = r#"{"foo":"bar\"baz"}"#;
        assert_eq!(
            extract_json_string_field(text, "foo").as_deref(),
            Some("bar\"baz")
        );
    }

    #[test]
    fn extract_last_json_string_field_last_wins() {
        let text = "{\"summary\":\"first\"}\n{\"summary\":\"second\"}\n{\"summary\":\"third\"}";
        assert_eq!(
            extract_last_json_string_field(text, "summary").as_deref(),
            Some("third")
        );
    }

    #[test]
    fn first_prompt_simple() {
        let head = r#"{"type":"user","message":{"content":"Hello!"}}"#.to_string() + "\n";
        assert_eq!(extract_first_prompt_from_head(&head), "Hello!");
    }

    #[test]
    fn first_prompt_skips_meta() {
        let head = "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"meta\"}}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"real prompt\"}}\n";
        assert_eq!(extract_first_prompt_from_head(head), "real prompt");
    }

    #[test]
    fn first_prompt_skips_tool_result() {
        let head = "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"content\":\"x\"}]}}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"actual prompt\"}}\n";
        assert_eq!(extract_first_prompt_from_head(head), "actual prompt");
    }

    #[test]
    fn first_prompt_content_blocks() {
        let head =
            r#"{"type":"user","message":{"content":[{"type":"text","text":"block prompt"}]}}"#
                .to_string()
                + "\n";
        assert_eq!(extract_first_prompt_from_head(&head), "block prompt");
    }

    #[test]
    fn first_prompt_truncates() {
        let long_prompt = "x".repeat(300);
        let head = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{long_prompt}\"}}}}\n");
        let result = extract_first_prompt_from_head(&head);
        assert!(result.chars().count() <= 201);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn first_prompt_command_fallback() {
        let head = r#"{"type":"user","message":{"content":"<command-name>/help</command-name>stuff"}}"#
            .to_string()
            + "\n";
        assert_eq!(extract_first_prompt_from_head(&head), "/help");
    }

    #[test]
    fn first_prompt_empty() {
        assert_eq!(extract_first_prompt_from_head(""), "");
        assert_eq!(
            extract_first_prompt_from_head("{\"type\":\"assistant\"}\n"),
            ""
        );
    }

    #[test]
    fn iso_timestamp_with_z() {
        assert_eq!(
            parse_iso_to_epoch_ms("2026-01-15T10:30:00.000Z"),
            Some(1768473000000)
        );
    }

    #[test]
    fn iso_timestamp_with_offset() {
        assert_eq!(
            parse_iso_to_epoch_ms("2026-01-15T10:30:00+00:00"),
            Some(1768473000000)
        );
    }

    #[test]
    fn iso_timestamp_invalid() {
        assert_eq!(parse_iso_to_epoch_ms("not-a-valid-iso-date"), None);
    }

    #[test]
    fn iso_timestamp_fraction() {
        // .5 seconds => 500 ms
        assert_eq!(
            parse_iso_to_epoch_ms("2026-01-15T10:30:00.5Z"),
            Some(1768473000500)
        );
    }

    #[test]
    fn parse_lite_helper_direct() {
        let jsonl = "{\"type\":\"user\",\"message\":{\"content\":\"test prompt\"},\"cwd\":\"/workspace\"}\n\
             {\"type\":\"tag\",\"tag\":\"experiment\",\"sessionId\":\"s\"}\n";
        let lite = jsonl_to_lite(jsonl, 123);
        let info = parse_session_info_from_lite("sid", &lite, Some("/fallback")).unwrap();
        assert_eq!(info.session_id, "sid");
        assert_eq!(info.summary, "test prompt");
        assert_eq!(info.tag.as_deref(), Some("experiment"));
        assert_eq!(info.cwd.as_deref(), Some("/workspace"));
    }

    #[test]
    fn parse_lite_created_at_invalid_is_none() {
        let jsonl =
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"timestamp\":\"not-a-valid-iso-date\"}\n";
        let lite = jsonl_to_lite(jsonl, 0);
        let info = parse_session_info_from_lite("sid", &lite, None).unwrap();
        assert_eq!(info.created_at, None);
    }
}
