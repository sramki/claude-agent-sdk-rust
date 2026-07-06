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

## Current state (2026-07-05)

- **Session reader: DONE.** `list_sessions`, `get_session_info`, `get_session_messages`,
  `list_subagents`, `get_subagent_messages` — filesystem path, sync, non-panicking.
- Faithful to **`claude-agent-sdk` Python v0.2.110** (`_internal/sessions.py`). Reproduces: path
  resolution (djb2/base36 long-path hash, NFC, git-worktree merge+dedup), 64 KiB lite head/tail scan,
  type-allow-list + skip-malformed parse, DAG→single-most-recent-branch chain build (no
  `logicalParentUuid`, keep `isCompactSummary`), subagent-file recursion.
- **108 tests** (35 unit + 71 integration + 2 doctests), `cargo clippy -D warnings` clean. Ported from
  upstream `tests/test_sessions.py` (store-backed suite deliberately skipped).
- **MIT** (matches upstream; `NOTICE` documents the port + non-affiliation). Committed `45b7f77`, no remote.
- Modules: `paths` · `parse` · `chain` · `sessions` · `types` · `lib`.

## Direction: **core-parity**, grown as needed (NOT full-parity on day one)

| Include (useful core) | Defer until demanded |
|---|---|
| session reader ✅ | MCP server config |
| **runtime: `query()` + streaming client over `claude -p --output-format stream-json`** ← NEXT | hooks / permission callbacks |
| core message / content-block / tool types + `Options` | `SessionStore` async backends |
| ported upstream tests as the sync guard | session write ops (fork / edit / import) |

## Next task — the runtime (the SDK's core)

Port the **live query path**: spawn `claude -p --output-format stream-json`, frame/parse the stream-json
protocol, expose a one-shot `query()` and a streaming client, plus the core `types.py` surface
(message/content-block/tool-use/tool-result types, `ClaudeAgentOptions`/`Options`). Port the upstream
runtime tests. Same bar: `cargo build` + `cargo test` + `cargo clippy -D warnings` green; faithful to a
declared upstream version; commit-first.

## Reference

- Upstream: `anthropics/claude-agent-sdk-python` (readable; the reference impl) + `@anthropic-ai/claude-agent-sdk` (TS, minified).
- Docs: https://code.claude.com/docs/en/agent-sdk/sessions · https://code.claude.com/docs/en/agent-sdk/python
- Local Python source snapshot used for the reader port: `_internal/sessions.py` from v0.2.110.

## Conventions

- Faithful port; cite upstream in code comments where logic is non-obvious. Minimal deps.
- Return types mirror upstream contracts (reader returns `Vec`/`Option`, empty/`None` on error paths —
  upstream tests assert exactly that; internals never panic on IO).
- Every change: build + test + clippy green, then commit with a conventional message. Bump the declared
  upstream-version marker (README/NOTICE) whenever re-syncing.
