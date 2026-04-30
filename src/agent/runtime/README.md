# adam-agent

This crate implements the business logic for Adam. It is designed to be used by the various Adam UIs written in Rust.

## Architecture Position

`adam-agent` sits above `adam-llm` and below user-facing surfaces such as `adam-tui`, `adam-exec`, and `adam app-server`.

- `adam-llm` is the model/runtime SDK boundary.
- `adam-agent-core` now provides the reusable turn-stream kernel shared by agent products.
- `adam-agent` owns Adam-specific agent behavior, tool orchestration, config, prompts, and adapters on top of that kernel.
- UI and protocol surfaces should depend on `adam-agent` rather than reimplementing agent logic.

Today this crate still contains substantial Adam-specific policy and the legacy thread/task adapters that drive the product. The regular turn path now routes through `adam-agent-core::kernel::AgentKernel`, and follow-on extractions should continue moving reusable loop concerns into `adam-agent-core` while keeping the `adam-llm` SDK boundary intact.

## Dependencies

Note that `adam-agent` makes some assumptions about certain helper utilities being available in the environment. Currently, this support matrix is:

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.codex` read-only.

### Linux

Expects the binary containing `adam-agent` to run the equivalent of `codex sandbox linux` (legacy alias: `codex debug landlock`) when `arg0` is `adam-linux-sandbox`. See the `adam-arg0` crate for details.

### All Platforms

Expects the binary containing `adam-agent` to simulate the virtual `apply_patch` CLI when `arg1` is `--codex-run-as-apply-patch`. See the `adam-arg0` crate for details.
