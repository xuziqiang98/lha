# lha-agent

This crate implements the business logic for LHA. It is designed to be used by the various LHA UIs written in Rust.

## Architecture Position

`lha-agent` sits above `lha-llm` and below user-facing surfaces such as `lha-tui`, `lha-exec`, and `lha app-server`.

- `lha-llm` is the model/runtime SDK boundary.
- `lha-core` now provides the reusable turn-stream kernel and in-memory session SDK shared by agent products.
- `lha-agent` owns LHA-specific agent behavior, tool orchestration, config, prompts, and adapters on top of that kernel.
- UI and protocol surfaces should depend on `lha-agent` rather than reimplementing agent logic.

Today this crate still contains substantial LHA-specific policy and the legacy thread/task adapters that drive the product. The regular turn path now routes through `lha_core::kernel::AgentKernel`, and follow-on extractions should continue moving reusable loop concerns into `lha-core` while keeping the `lha-llm` SDK boundary intact.

## Dependencies

Note that `lha-agent` makes some assumptions about certain helper utilities being available in the environment. Currently, this support matrix is:

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.lha` read-only.

### Linux

Expects the binary containing `lha-agent` to run the equivalent of `codex sandbox linux` (legacy alias: `codex debug landlock`) when `arg0` is `lha-linux-sandbox`. See the `lha-arg0` crate for details.

### All Platforms

Expects the binary containing `lha-agent` to simulate the virtual `apply_patch` CLI when `arg1` is `--codex-run-as-apply-patch`. See the `lha-arg0` crate for details.
