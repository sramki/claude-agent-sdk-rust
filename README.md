# claude-agent-sdk (Rust)

Read local **Claude Code** session history from `~/.claude/projects/**/*.jsonl`.

This crate is a faithful Rust port of the **session-reading** functionality of
Anthropic's [`claude-agent-sdk`](https://github.com/anthropics/claude-agent-sdk-python)
for Python — specifically the filesystem logic in
`src/claude_agent_sdk/_internal/sessions.py`. It reads and reconstructs
transcripts that Claude Code has already written to disk. It does **not** wrap
the `claude` CLI, stream live turns, or talk to any network service.

Zero external services, minimal dependencies (`serde`, `serde_json`,
`unicode-normalization`), fully synchronous.

## What it does

- **`list_sessions`** — enumerate sessions with metadata pulled from `stat` +
  head/tail reads (no full parse), newest-first, with `limit`/`offset` paging
  and optional git-worktree scanning.
- **`get_session_info`** — metadata for a single session by UUID.
- **`get_session_messages`** — the reconstructed user/assistant conversation in
  chronological order. The transcript is a `parentUuid` DAG; this walks it to
  the most-recent leaf and collapses it to a single branch (preferring the main
  chain over sidechains, keeping compaction summaries, dropping meta/team
  messages).
- **`list_subagents`** / **`get_subagent_messages`** — subagent transcripts
  under `<session>/subagents/` (including nested `workflows/<runId>/`).

Config home is `$CLAUDE_CONFIG_DIR` if set, otherwise `~/.claude`. Every
function degrades gracefully: a missing directory, unreadable file, invalid
UUID, or malformed line yields an empty result rather than an error.

## Usage

```rust
use std::path::Path;
use claude_agent_sdk::{list_sessions, get_session_messages, MessageType};

// 20 newest sessions for a project (also scanning its git worktrees).
let dir = Path::new("/path/to/project");
for info in list_sessions(Some(dir), Some(20), 0, true) {
    println!("{}  {}", info.session_id, info.summary);
}

// The full conversation of one session.
for msg in get_session_messages("550e8400-e29b-41d4-a716-446655440000", Some(dir), None, 0) {
    let who = match msg.message_type {
        MessageType::User => "user",
        MessageType::Assistant => "assistant",
    };
    println!("[{who}] {}", msg.message); // `message` is the raw serde_json::Value
}
```

Run the bundled example against your own history:

```console
$ cargo run --example list_sessions              # across all projects
$ cargo run --example list_sessions -- /my/proj  # one project directory
```

## Public API

| Function | Returns |
|---|---|
| `list_sessions(dir: Option<&Path>, limit: Option<usize>, offset: usize, include_worktrees: bool)` | `Vec<SessionInfo>` |
| `get_session_info(session_id: &str, dir: Option<&Path>)` | `Option<SessionInfo>` |
| `get_session_messages(session_id: &str, dir: Option<&Path>, limit: Option<usize>, offset: usize)` | `Vec<SessionMessage>` |
| `list_subagents(session_id: &str, dir: Option<&Path>)` | `Vec<String>` |
| `get_subagent_messages(session_id: &str, agent_id: &str, dir: Option<&Path>, limit: Option<usize>, offset: usize)` | `Vec<SessionMessage>` |

`limit = Some(0)` means "no limit" (matching the upstream `limit > 0` check).

## Scope / non-goals

The live-streaming client (`query()`, the `claude` subprocess transport) and the
async `SessionStore` backend are intentionally **not** ported. This crate is the
filesystem read path only.

## Attribution & license

This is an independent, unofficial Rust port of the session-reading logic in
Anthropic's `claude-agent-sdk` (Python), including a Rust port of that project's
`tests/test_sessions.py`. The original is MIT-licensed
(Copyright © 2025 Anthropic, PBC); this port preserves that license. See
[`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Not affiliated with or endorsed by
Anthropic.
