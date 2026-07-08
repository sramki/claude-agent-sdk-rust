# claude-agent-sdk-rs — project context

A **standalone, open-source Rust port of Anthropic's official Claude Agent SDK** (Python and
TypeScript). Published on crates.io as `claude-agent-sdk-rs`, imported as `claude_agent_sdk_rs`. It
reads local Claude Code session history and drives the `claude` runtime. **Zero coupling to any other
project** — this is a general community crate.

> This repo is intentionally standalone and decoupled from any downstream application that may
> consume it. Nothing here should reference a specific downstream project.

## Mission

A **faithful, maintained** Rust port of the Agent SDK — not a one-time snapshot. Two properties are
load-bearing:
1. **Faithful** — behavior matches the upstream SDK (which itself wraps the `claude` CLI's `.jsonl`
   format + stream-json protocol). Port the upstream *tests* too; they are the contract.
2. **In sync** — pinned to a declared upstream version; re-ported on upstream releases.

## Current state (2026-07-06)

Faithful to **Anthropic's Python SDK v0.2.110**. Idiomatic Rust (`Result`, serde enums, tokio async,
`Arc`-wrapped callbacks). **~388 tests** (unit + ported-upstream parity suites + mock-transport runtime
+ live-CLI + doctests), `cargo clippy -D warnings` clean. Published as `claude-agent-sdk-rs`, imported
as `claude_agent_sdk_rs`. **MIT**; GitHub `sramki/claude-agent-sdk-rust`.

Fidelity fixes done (former "bucket B"): transport `close()` graceful terminate→SIGTERM→SIGKILL
escalation, unix atexit orphan-reaper, `Options.user` uid, stderr-callback panic isolation, full
`Cf`/`Co`/`Cn` unicode strip, `AssistantMessageError` unknown-fallback, once-per-process shadow warning,
truncated-final-line drop via `read_until` framing.

Test parity done (former "bucket C") for the ported code: Rust ports of upstream `test_message_parser`,
`test_types`, `test_errors`, `test_rate_limit_event_repro`, `test_session_summary`, `test_session_mutations`
(local), `test_transport` (build_command), `test_subprocess_buffering`, `test_option_warnings` — no
behavioral discrepancies found. Store-layer test files map to the remaining bucket-A work below.

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
- **SessionStore ↔ runtime integration (former "bucket A") — DONE:**
  - Store-backed async readers (`store_read`): `list_sessions_from_store` (summary fast-path +
    gap-fill / bounded-concurrency slow-path), `get_session_*_from_store`, subagent variants.
  - `import_session_to_store` (`store_import`).
  - Live transcript-mirror batcher (`store_mirror`) wired into the `Query` read loop (enqueue
    `transcript_mirror` frames, flush on result, `mirror_error` surfaced) + `validate_session_store_options`.
  - Store-backed resume materialization (`session_resume`): temp `CLAUDE_CONFIG_DIR`, auth copy,
    subagent reconstruction, wired into `setup_query` for `query()`/`Client`.
  - `*_via_store` mutation variants (`mutations`).
  - Reusable conformance harness (`testing::run_session_store_conformance`).
- **Lossless raw reader (non-upstream extension):** `get_session_entries` returns a session's
  transcript as verbatim raw lines — no envelope projection, no `build_conversation_chain` selection
  (all branches/forks/pre-compaction history), no re-serialization; byte-for-byte round-trippable.
  `get_session_entries_from_store` is the store counterpart (field-lossless). Kept alongside the
  parity-faithful `get_session_messages`, not replacing it.

**Remaining:** nothing structural — the whole SDK surface is ported. Open items are the small
fidelity notes (OTEL trace propagation not ported; username→uid resolution not done) and re-syncing
to newer upstream versions.

## Reference

- Upstream: `anthropics/claude-agent-sdk-python` (readable; the reference impl) + the TypeScript SDK (minified).
- Docs: https://code.claude.com/docs/en/agent-sdk/sessions · https://code.claude.com/docs/en/agent-sdk/python
- Local Python source snapshot used for the reader port: `_internal/sessions.py` from v0.2.110.

## Conventions

- Faithful port in **idiomatic Rust** (`Result`-based, serde enums, tokio async). Cite upstream in code
  comments where logic is non-obvious. Minimal deps.
- The public API is `Result`-based: invalid *input* (bad UUID, empty title) → `Err`; a missing session
  or file degrades gracefully to `Ok(None)`/`Ok(vec![])` (the reader's documented best-effort contract).
- Every change: build + test + clippy green, then commit with a conventional message. Bump the declared
  upstream-version marker (README/NOTICE) whenever re-syncing.
