# claude-agent-sdk-rs

An idiomatic Rust port of Anthropic's
[Claude Agent SDK](https://github.com/anthropics/claude-agent-sdk-python)
for Python (pinned to **v0.2.110**). Read local **Claude Code** session history
*and* drive the live `claude` runtime over the stream-json protocol.

`Result`-based, serde-typed, `tokio`-async; callbacks are `Arc`-wrapped closures.

## Install

The crate is published as **`claude-agent-sdk-rs`** and imported as
**`claude_agent_sdk_rs`**:

```toml
[dependencies]
claude-agent-sdk-rs = "0.1"
```

Or straight from git (no crates.io needed):

```toml
claude-agent-sdk-rs = { git = "https://github.com/sramki/claude-agent-sdk-rust" }
```

```rust
use claude_agent_sdk_rs::query;
```

MSRV: Rust 1.83.

## What it does

**Live runtime** (`tokio`)

- **`query`** — one-shot / unidirectional streaming: spawn `claude --output-format
  stream-json`, send a prompt, get a typed `MessageStream`.
- **`Client`** — bidirectional interactive conversations: `connect`, `query`,
  read a continuous `messages()` stream, plus `interrupt`, `set_permission_mode`,
  `set_model`, `rewind_files`, MCP reconnect/toggle, `stop_task`,
  `get_mcp_status`, `get_context_usage`.
- **Hooks & permission callbacks** — `can_use_tool` and lifecycle hooks over the
  bidirectional control protocol.
- **In-process MCP tools** — `create_sdk_mcp_server` + `tool` run tools in your
  process (no IPC).

**Session reader** (filesystem, no CLI)

- **`list_sessions`** / **`get_session_info`** — metadata from `stat` + head/tail
  reads (no full parse), newest-first, paging, optional git-worktree scanning.
- **`get_session_messages`** — the reconstructed user/assistant conversation
  (walks the `parentUuid` DAG to the most-recent leaf, collapsing to one branch).
- **`list_subagents`** / **`get_subagent_messages`** — subagent transcripts.

**Session write ops**

- **`rename_session`** / **`tag_session`** / **`delete_session`** /
  **`fork_session`**, and the `InMemorySessionStore` reference `SessionStore`.

The reader degrades gracefully: a missing directory, unreadable file, or
malformed line yields `Ok` (empty), not an error. Only invalid *input* (a bad
UUID, an empty title) is an `Err`.

> Not yet ported (upstream's *defer until demanded* set): the store-backed async
> listing variants, `SessionStore`-mirrored resume/import, and the transcript
> mirror batcher. See `CLAUDE.md`.

## Usage

Drive a live query (requires the `claude` CLI on `PATH`):

```rust
use claude_agent_sdk_rs::{query, ClaudeAgentOptions, Message};

# async fn run() -> claude_agent_sdk_rs::Result<()> {
let mut stream = query("What is 2 + 2?", ClaudeAgentOptions::default()).await?;
while let Some(msg) = stream.next().await {
    if let Message::Result(r) = msg? {
        println!("{}", r.result.unwrap_or_default());
    }
}
# Ok(()) }
```

Read local session history (filesystem only, no CLI):

```rust
use std::path::Path;
use claude_agent_sdk_rs::{list_sessions, get_session_messages, MessageType};

# fn run() -> claude_agent_sdk_rs::Result<()> {
let dir = Path::new("/path/to/project");
for info in list_sessions(Some(dir), Some(20), 0, true)? {
    println!("{}  {}", info.session_id, info.summary);
}
for msg in get_session_messages("550e8400-e29b-41d4-a716-446655440000", Some(dir), None, 0)? {
    let who = match msg.message_type { MessageType::User => "user", MessageType::Assistant => "assistant" };
    println!("[{who}] {}", msg.message); // `message` is the raw serde_json::Value
}
# Ok(()) }
```

Run the bundled reader example against your own history:

```console
$ cargo run --example list_sessions              # across all projects
$ cargo run --example list_sessions -- /my/proj  # one project directory
```

The reader functions are `Result`-based; `limit = Some(0)` means "no limit"
(matching the upstream `limit > 0` check).

## Scope / non-goals

Ported: the reader, the live runtime (`query`/`Client` over the stream-json
control protocol, hooks, permission callbacks, in-process MCP servers), and local
session write ops + the `InMemorySessionStore`. The deeper `SessionStore`↔runtime
integration (store-backed listing, mirrored resume/import, the transcript-mirror
batcher) is **not** yet ported — see `CLAUDE.md`.

## Attribution & license

This is an independent, unofficial Rust port of Anthropic's Claude Agent SDK
(Python), including a Rust port of that project's tests. The original is
MIT-licensed (Copyright © 2025 Anthropic, PBC); this port preserves that license.
See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Not affiliated with or endorsed
by Anthropic.
