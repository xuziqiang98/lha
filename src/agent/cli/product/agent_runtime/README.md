# LHA Product Runtime

This private module implements the business logic for LHA inside the `lha`
package. It is used by the `lha` TUI, `lha exec`, `lha app-server`, and related
product surfaces.

## Architecture Position

The runtime sits above `lha-llm` and `lha-core`, and below user-facing surfaces
that are now private modules in `lha`.

- `lha-llm` is the model/runtime SDK boundary.
- `lha-core` provides the reusable turn-stream kernel and in-memory session SDK
  shared by agent products.
- `agent_runtime` owns LHA-specific agent behavior, tool orchestration, config,
  prompts, persistence integration, and adapters on top of that kernel.
- UI and protocol surfaces should reuse this module rather than reimplementing
  agent logic.

Today this module still contains substantial LHA-specific policy and the legacy
thread/task adapters that drive the product. The regular turn path now routes
through `lha_core::kernel::AgentKernel`, and follow-on extractions should
continue moving reusable loop concerns into `lha-core` while keeping the
`lha-llm` SDK boundary intact.

## Dependencies

The runtime makes some assumptions about helper functionality being available in
the installed `lha` binary.

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.lha` read-only.

### Linux

Expects the `lha` binary to run the Linux sandbox helper when `arg0` is
`lha-linux-sandbox`. The `lha` binary creates the needed arg0 alias during
startup.

### All Platforms

Expects the `lha` binary to simulate the virtual `apply_patch` CLI when `arg1`
is `--codex-run-as-apply-patch`.
