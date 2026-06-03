# Memories

LHA memories are a local, file-backed memory system modeled after Codex memories. The feature is experimental and disabled by default.

## Enable

```toml
[features]
memories = true

[memories]
use_memories = true
generate_memories = true
dedicated_tools = false
```

## Files

Memory files live under `$LHA_HOME/memories`:

- `memory_summary.md` is injected into new sessions when memory use is enabled.
- `MEMORY.md` is the durable handbook for searchable memory.
- `raw_memories.md` is regenerated from stage-1 extraction outputs.
- `rollout_summaries/` contains per-thread summaries used by consolidation.
- `extensions/ad_hoc/notes/` stores explicit user-requested memory notes.
- `phase2_workspace_diff.md` is a generated diff for consolidation.

Memory state lives in `$LHA_HOME/memories_1.sqlite`, separate from `$LHA_HOME/state.sqlite`.
When the memory feature is disabled, non-memory state features such as logs and goals do not open or require `memories_1.sqlite`.

## Config

`[memories]` supports:

- `use_memories`: inject memory read instructions when `memory_summary.md` exists.
- `generate_memories`: mark new threads as eligible for memory generation. Setting this to `false` writes a disabled marker on new threads even when the memory feature is currently off, so later backfills do not treat those threads as eligible by default.
- `dedicated_tools`: expose `memories__list`, `memories__read`, `memories__search`, and `memories__add_ad_hoc_note`.
- `disable_on_external_context`: mark threads polluted when external context should prevent memory generation.
- `max_raw_memories_for_consolidation`, `max_unused_days`, `max_rollout_age_days`, `max_rollouts_per_startup`, `min_rollout_idle_hours`, and `min_rate_limit_remaining_percent`: retention/startup tuning values.
- `extract_model` and `consolidation_model`: optional model overrides reserved for extraction/consolidation workers.
  If an override cannot be found in the local model catalog, LHA falls back to the current session model and records a diagnostic metric.

## Startup Pipeline

When the feature is enabled, a root non-ephemeral session starts a background memory task after thread startup. The task creates `$LHA_HOME/memories`, ensures the file layout exists, prunes stale stage-1 rows, then runs the two Codex-style write phases:

- Phase 1 claims recent idle interactive threads whose `memory_mode` is `enabled`, filters rollout noise such as developer instructions and skill injections, redacts common secret patterns, asks the configured model for strict JSON `{ raw_memory, rollout_summary, rollout_slug }`, and writes successful outputs to `$LHA_HOME/memories_1.sqlite`.
- Phase 2 claims a global consolidation lock, materializes selected stage-1 outputs into `raw_memories.md` and `rollout_summaries/`, computes a git-baseline workspace diff, and runs a restricted internal agent in `$LHA_HOME/memories` to update `MEMORY.md`, `memory_summary.md`, and `skills/`. On success it removes `phase2_workspace_diff.md`, resets the memory git baseline, and records the selected snapshots.

LHA currently has no rate-limit status API, so the rate-limit guard is best-effort and does not block startup when no snapshot is available.

## Read Path

When `use_memories` is true, new sessions inject developer instructions built from `memory_summary.md` if the file exists and is non-empty. The model can inspect the memory workspace with normal filesystem access or the dedicated tools when enabled. If the model uses memory, it appends a hidden `<oai-mem-citation>` block; LHA suppresses that block from streaming and final UI text, parses it, and records usage for cited rollout IDs. Malformed citation blocks are stripped and ignored.

Parsed citations are available to app-server v2 clients as optional `memoryCitation` data on agent-message thread items. LHA persists this data in rollout history, so historical reads, resume, and reconnect flows preserve memory citations rather than limiting them to the live stream.

## Dedicated Tools

When `dedicated_tools` is true, LHA exposes memory tools rooted at `$LHA_HOME/memories`:

- `memories__list`, `memories__read`, and `memories__search` only accept relative paths and reject traversal or symlink escapes.
- `memories__search` accepts either a memory file or directory path.
- `memories__add_ad_hoc_note` writes only under `extensions/ad_hoc/notes/`.
- Ad-hoc note slugs are optional. When provided, a slug must match `^[a-z0-9]+(?:-[a-z0-9]+)*$` and be at most 60 bytes; invalid slugs are rejected rather than sanitized.

## External Context

If `disable_on_external_context` is true, MCP tool calls and hosted web-search activity mark the current thread's `memory_mode` as `polluted`. Polluted threads are skipped by Phase 1, and if an already-selected thread becomes polluted, the next Phase 2 run updates the workspace so stale memory can be removed.

## TUI

Use `/memories` in the TUI to toggle:

- `Memory feature`
- `Use memories`
- `Generate memories`
- `Dedicated tools`

The read path is initial-context-only, so changing `Use memories` affects new threads. Changing `Generate memories` affects whether new threads are eligible for future extraction; turning it off records `memory_mode = "disabled"` for new threads even if the experimental memory feature is not enabled yet.
