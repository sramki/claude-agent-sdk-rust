# claude-agent-sdk-rust — project context

A **standalone, open-source Rust port of the official Claude Agent SDK** (Python `claude-agent-sdk` /
TS `@anthropic-ai/claude-agent-sdk`). It reads local Claude Code session history and (roadmap) drives
the `claude` runtime. **Zero coupling to any other project** — this is a general community crate.

> This repo is intentionally standalone and decoupled from any downstream application that may
> consume it. Nothing here should reference a specific downstream project.

## Mission

A **faithful, maintained** Rust port of the Agent SDK — not a one-time snapshot. Two properties are
load-bearing:
1. **Faithful** — behavior matches the upstream SDK (which itself wraps the `claude` CLI's `.jsonl`
   format + stream-json protocol). Port the upstream *tests* too; they are the contract.
2. **In sync** — pinned to a declared upstream version; re-ported on upstream releases.

## Current state (2026-07-06)

Faithful to **`claude-agent-sdk` Python v0.2.110**. Idiomatic Rust (`Result`, serde enums, tokio async,
`Arc`-wrapped callbacks). **177 tests** (97 unit + 80 integration/runtime/mutations + 2 doctests),
`cargo clippy -D warnings` clean. **MIT**; on GitHub at `sramki/claude-agent-sdk-rust` (branch
`feat/full-parity`).

**DONE:**
- **Session reader** (`sessions`) — refactored to `Result` (invalid input → `Err`; missing → `Ok`).
- **Error type** (`error`) — `thiserror` enum mirroring `_errors.py`.
- **Core types** (`types/`) — content blocks, messages (incl. typed system-message kinds, rate-limit,
  result, stream events), permission types (`to_wire`/`from_wire`), hooks, MCP configs+status,
  sandbox, config sub-types, session-store types + `SessionStore` trait, `ClaudeAgentOptions`.
- **Runtime** (`runtime/`) — message parser, subprocess-CLI transport (tokio), control-protocol
  `Query` (hooks + permission + SDK-MCP callback dispatch, initialize handshake, all control ops),
  public `query()` + streaming `Client`. Mock-transport integration tests.
- **MCP SDK servers** (`mcp`) — `create_sdk_mcp_server`, `tool`, `SdkMcpTool`, `ToolAnnotations`.
- **Session mutations** (`mutations`) — local `rename`/`tag`/`delete`/`fork` + `project_key_for_directory`.
- **SessionStore core** (`store`) — `InMemorySessionStore`, `fold_session_summary`,
  `summary_entry_to_sdk_info`, `file_path_to_session_key`.

**Remaining (deepest store↔runtime integration; the upstream `defer until demanded` set):**
- Store-backed async listing variants (`list_sessions_from_store`, `get_session_*_from_store`, …).
- `import_session_to_store`; session-resume materialization (`materialize_resume_session`,
  `apply_materialized_options`); `validate_session_store_options`.
- Transcript-mirror batcher + runtime wiring (the `Query` read loop currently drops
  `transcript_mirror` frames; `--session-mirror` flag is emitted).
- `*_via_store` mutation variants.

## Reference

- Upstream: `anthropics/claude-agent-sdk-python` (readable; the reference impl) + `@anthropic-ai/claude-agent-sdk` (TS, minified).
- Docs: https://code.claude.com/docs/en/agent-sdk/sessions · https://code.claude.com/docs/en/agent-sdk/python
- Local Python source snapshot used for the reader port: `_internal/sessions.py` from v0.2.110.

## Conventions

- Faithful port in **idiomatic Rust** (`Result`-based, serde enums, tokio async). Cite upstream in code
  comments where logic is non-obvious. Minimal deps.
- The public API is `Result`-based: invalid *input* (bad UUID, empty title) → `Err`; a missing session
  or file degrades gracefully to `Ok(None)`/`Ok(vec![])` (the reader's documented best-effort contract).
- Every change: build + test + clippy green, then commit with a conventional message. Bump the declared
  upstream-version marker (README/NOTICE) whenever re-syncing.
