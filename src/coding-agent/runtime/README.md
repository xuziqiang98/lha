# codex-coding-agent

This crate implements the business logic for Codex. It is designed to be used by the various Codex UIs written in Rust.

## Architecture Position

`codex-coding-agent` sits above `codex-llm` and below user-facing surfaces such as `codex-tui`, `codex-exec`, and `codex app-server`.

- `codex-llm` is the model/runtime SDK boundary.
- `codex-agent-core` now provides the reusable turn-stream kernel shared by agent products.
- `codex-coding-agent` owns Codex-specific agent behavior, tool orchestration, config, prompts, and adapters on top of that kernel.
- UI and protocol surfaces should depend on `codex-coding-agent` rather than reimplementing agent logic.

Today this crate still contains substantial Codex-specific policy and the legacy thread/task adapters that drive the product. The regular turn path now routes through `codex-agent-core::kernel::AgentKernel`, and follow-on extractions should continue moving reusable loop concerns into `codex-agent-core` while keeping the `codex-llm` SDK boundary intact.

## Dependencies

Note that `codex-coding-agent` makes some assumptions about certain helper utilities being available in the environment. Currently, this support matrix is:

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.codex` read-only.

### Linux

Expects the binary containing `codex-coding-agent` to run the equivalent of `codex sandbox linux` (legacy alias: `codex debug landlock`) when `arg0` is `codex-linux-sandbox`. See the `codex-arg0` crate for details.

### All Platforms

Expects the binary containing `codex-coding-agent` to simulate the virtual `apply_patch` CLI when `arg1` is `--codex-run-as-apply-patch`. See the `codex-arg0` crate for details.
