# The Claude cartridge — SDK-side spec

**Status:** locked. Non-upstream extension surface — marked as such, carried across upstream
re-syncs. The SDK is a *versioned bag of Claude knowledge*: paths + pure per-entry functions +
a blob resolver. It does **not** read, stream, merge, cursor, filter, or watch — those are the
consumer's. This spec is the SDK surface only; the consumer wires it into a streaming engine.

All new symbols live in the public `cartridge` module (re-exported at the crate root).

## 1. Locate

- `projects_dir() -> PathBuf` — Claude projects root, `CLAUDE_CONFIG_DIR`-aware. `[expose have]`
- `project_key_for_directory(cwd: Option<&Path>) -> String` — cwd → sanitized project-dir name. `[have]`
- `list_projects() -> Result<Vec<ProjectInfo>>` — one `read_dir` of the projects root; each
  real project subdir → `ProjectInfo { name, path, session_count }`. `[new]`
- `discover_transcripts(recursive: bool) -> Result<Vec<TranscriptFile>>` — every `*.jsonl` under
  the projects root. `recursive=false` = top-level session files only; `recursive=true` also
  descends into `<session>/subagents/**` (and nested `workflows/`). Each →
  `TranscriptFile { path, project, session_id, subpath, is_subagent }`. Locator only — no
  content read, no dedup, no sort, no filtering. `[new]`
  - `subpath`/`is_subagent` let the consumer choose session granularity (subagent-distinguishable).
  - Stateless snapshot: returns "what files exist now", `Result`, re-callable. It never watches;
    the engine re-invokes it for live discovery.

## 2. Interpret — pure per-entry functions

**Hot path (byte-scan, `&[u8]` in, no `Value` DOM):** run on every line at 640k+ scale.
- `entry_id(line: &[u8]) -> Option<String>` — native `uuid` via byte-scan. `[new]`
- `entry_kind(line: &[u8]) -> Option<&str>` — the top-level `type` value (written first). `[new]`

**Downstream (`&Value` in — the consumer parses once with `from_slice` and shares):**
- `envelope(&Value) -> Envelope` — cheap lineage/metadata: `entry_type, uuid, parent_uuid,
  logical_parent_uuid, session_id, is_sidechain, is_meta, is_compact_summary, timestamp`. `[new]`
  **`timestamp` lives here** (consolidated — there is no separate `entry_timestamp`).
- `to_typed(&Value) -> Option<TranscriptEntry>` — typed lens (typed shell + flattened `extra`). `[have]`
- `content_blocks(&Value) -> Vec<ContentBlock>` — parse a message payload into typed blocks. `[have]`
- `blob_refs(&Value) -> Vec<String>` — reference tokens the entry carries (`imagePasteId(s)`, …). `[new]`

## 3. Dereference — blobs

- `resolve_blob(reference: &str) -> Option<Blob>`, `Blob = Path(PathBuf) | Bytes(Vec<u8>)`. `[new]`
  On-demand only — never called eagerly in the pipeline.
  - Resolves a reference by **native store key**: a `paste-cache` hex id → `paste-cache/<id>.txt`;
    a `file-history` key → `file-history/<key>`. Returns `Path` when the file exists, else `None`.
  - **Known open item:** sessions may reference pastes by an *integer ordinal* (`imagePasteIds:[1]`)
    rather than the hex store key; that ordinal→key indirection is not yet reverse-engineered
    (version-fragile) and currently resolves to `None`. Documented, not guessed.

## 4. Stay-current

- `UPSTREAM_VERSION: &str` (`"0.2.110"`) — the Claude Code schema the interpret fns assume. `[new]`

## Obligations

1. **Interpret fns are total.** A *complete* line may still be malformed/non-JSON or missing a
   field → return `None`/empty, **never panic**. (The engine guarantees whole lines; validity is
   not guaranteed.)
2. **`&Value` fns take `&Value`** (not `&[u8]`) so the consumer parses once and shares.
3. **`resolve_blob` is on-demand** — the only I/O among the interpret/deref fns.
4. **Small outputs.** Ids, kinds, refs, small structs. `to_typed`/`content_blocks` are the only
   typed outputs and both are opt-in.
5. **Stateless.** No caching, no watching, no cursors. Every fn is pure or a plain fs read.
