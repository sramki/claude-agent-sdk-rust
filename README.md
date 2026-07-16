# claude-agent-sdk-rs

An idiomatic Rust port of Anthropic's
[Claude Agent SDK](https://github.com/anthropics/claude-agent-sdk-python)
for Python (pinned to **v0.2.110**). Read local **Claude Code** session history
*and* drive the live `claude` runtime over the stream-json protocol.

`Result`-based, serde-typed, `tokio`-async; callbacks are `Arc`-wrapped closures.

## Install

The crate name is **`claude-agent-sdk-rs`**, imported as **`claude_agent_sdk_rs`**.

**Not yet on crates.io** — add it as a git dependency. Pin a `rev` (or `tag`) so
builds are reproducible while the API is still settling:

```toml
[dependencies]
claude-agent-sdk-rs = { git = "https://github.com/sramki/claude-agent-sdk-rust", rev = "7012b07" }
```

Track the latest `main` instead (unpinned — moves as the repo does):

```toml
claude-agent-sdk-rs = { git = "https://github.com/sramki/claude-agent-sdk-rust", branch = "main" }
```

Once published, this becomes:

```toml
claude-agent-sdk-rs = "0.1"
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
- **Typed multimodal input** (extension) — `input::user_message` +
  `UserContentBlock::{text, image_base64, image_url, document_base64, document_url}`
  build validated text / image / PDF content blocks (MIME allowlist + size cap)
  for `Prompt::Messages`.

**Session reader** (filesystem, no CLI)

- **`list_sessions`** / **`get_session_info`** — metadata from `stat` + head/tail
  reads (no full parse), newest-first, paging, optional git-worktree scanning.
- **`get_session_messages`** — the reconstructed user/assistant conversation
  (walks the `parentUuid` DAG to the most-recent leaf, collapsing to one branch).
- **`get_session_entries`** — *lossless* raw read (extension, beyond upstream
  parity): every transcript line, verbatim, in file order — no envelope
  projection, no chain selection (all branches / forks / pre-compaction history),
  no re-serialization. Byte-for-byte round-trippable. `get_session_entries_from_store`
  is the store-backed counterpart (field-lossless).
- **`get_session_entries_typed`** — same lossless view as typed `TranscriptEntry`
  values: common envelope fields (`parent_uuid`, `timestamp`, `cwd`, `git_branch`,
  `tool_use_result`, …) typed, everything else kept in `extra`. Plus
  `content_blocks(&msg.message)` to parse a message payload into typed
  `ContentBlock`s. Store variant: `get_session_entries_typed_from_store`.
- **`list_subagents`** / **`get_subagent_messages`** — subagent transcripts.

**Session write ops**

- **`rename_session`** / **`tag_session`** / **`delete_session`** /
  **`fork_session`** (local and `*_via_store`).

**External `SessionStore`**

- Implement the `SessionStore` trait for any backend (Postgres/S3/Redis/…);
  `InMemorySessionStore` is a ready reference impl, and
  `testing::run_session_store_conformance` checks an adapter against the contract.
- **Live mirroring** — set `options.session_store` and the runtime streams the
  transcript into your store as the session runs.
- **Store-backed readers** — `list_sessions_from_store`, `get_session_*_from_store`.
- **Resume from a store** — `resume`/`continue_conversation` + a store loads the
  session back and resumes it, even with no local copy.
- **`import_session_to_store`** — replay a local session into a store.

**Cartridge** (extension — `cartridge` module, `docs/cartridge-spec.md`)

A Claude "adapter" surface for wiring session data into an external
streaming/merge engine: **pure data + functions, nothing stateful** (no reader,
stream, cursor, or watch — those stay the engine's).

- **Locate** — `list_projects`, `discover_transcripts(recursive)` (finds nested
  subagent/workflow transcripts, not just top-level), `projects_dir`.
- **Interpret** — hot-path byte-scanners over `&[u8]` (`entry_id`, `entry_kind`,
  total / never-panic) + `&Value` accessors (`envelope`, `to_typed`,
  `content_blocks`, `blob_refs`).
- **Dereference** — `resolve_blob` (paste-cache / file-history by native key,
  on-demand).
- **`UPSTREAM_VERSION`** — the pinned Claude Code schema the interpret fns assume.

The reader degrades gracefully: a missing directory, unreadable file, or
malformed line yields `Ok` (empty), not an error. Only invalid *input* (a bad
UUID, an empty title) is an `Err`.

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
use claude_agent_sdk_rs::{list_sessions, get_session_messages, get_session_entries, MessageType};

# fn run() -> claude_agent_sdk_rs::Result<()> {
let dir = Path::new("/path/to/project");
for info in list_sessions(Some(dir), Some(20), 0, true)? {
    println!("{}  {}", info.session_id, info.summary);
}
// Conversation view: one branch, user/assistant only, envelope projected away.
for msg in get_session_messages("550e8400-e29b-41d4-a716-446655440000", Some(dir), None, 0)? {
    let who = match msg.message_type { MessageType::User => "user", MessageType::Assistant => "assistant" };
    println!("[{who}] {}", msg.message); // `message` is the raw serde_json::Value
}

// Lossless view: every transcript line, verbatim — all branches, all fields,
// byte-for-byte. Parse each line yourself for full-fidelity access.
for line in get_session_entries("550e8400-e29b-41d4-a716-446655440000", Some(dir))? {
    if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) {
        let _ = entry.get("parentUuid"); // envelope fields get_session_messages drops
    }
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

## How it compares (surveyed 2026-07-08)

There is no official Anthropic Rust SDK (only Python and TypeScript are
official). Several independent community ports of the Claude Agent SDK exist on
crates.io. This crate is distinguished on two axes: **parity with the Python SDK
v0.2.110**, and **test coverage**.

The Python SDK ships two halves — a live runtime *and* a session-history layer
(reading `~/.claude` transcripts, an external `SessionStore`, **live transcript
mirroring**, import, store-backed resume). Most Rust ports implement only the
runtime half.

**This crate ports the complete public API surface of Python SDK v0.2.110 —
including the session-history, `SessionStore`, and live-mirroring layer that the
other surveyed ports omit.** Parity is verified — every one of v0.2.110's 126
public names maps, and the upstream test suites are ported test-for-test, not
asserted informally. Two internal items remain
unported — OTEL trace propagation and username→uid resolution for
`Options.user` — and neither appears in the public API (see `CLAUDE.md`). Of the
community ports surveyed, this is the only one that covers the session-history /
`SessionStore` / mirroring half at all.

| Crate (crates.io) | 10 hook events | in-proc MCP | session reader + `SessionStore` + mirror | `#[test]` fns |
|---|:--:|:--:|:--:|--:|
| **this crate** | ✓ | ✓ | **✓** | **456** |
| `claude-code-agent-sdk` | ✓ | ✓ | ✗ | 223 |
| `claude-agent-sdk-rs` | ✗ (6) | ✓ | ✗ | 258 |
| `claude-sdk` | ✗ | ✗ | ✗ | 138 |
| `claude-agent-sdk` | ✗ (7) | ✓ | ✗ | 70 |
| `claude-agent-sdk-rust` | ✓ | ✗ | ✗ | 25 |

Figures are from each crate's published source / repository on the survey date;
other crates may add features over time. Beyond full upstream parity, this crate
also ships several non-upstream extensions: lossless raw/typed transcript reads
(`get_session_entries` / `get_session_entries_typed`), typed multimodal input
(image + PDF, validated), and the `cartridge` adapter surface for external
streaming engines. Extensions are marked as such and never touch the parity API.

## Scope

The full SDK surface is ported: the reader, the live runtime (`query`/`Client`
over the stream-json control protocol, hooks, permission callbacks, in-process
MCP servers), session write ops, and the complete external-`SessionStore`
integration (live mirroring, store-backed reads, resume/import, conformance
harness). See `CLAUDE.md` for the small remaining fidelity notes.

## Attribution & license

This is an independent, unofficial Rust port of Anthropic's Claude Agent SDK
(Python), including a Rust port of that project's tests. The original is
MIT-licensed (Copyright © 2025 Anthropic, PBC); this port preserves that license.
See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Not affiliated with or endorsed
by Anthropic.
