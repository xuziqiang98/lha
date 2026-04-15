## Quickstart

### Building
From the repository root:

```
cargo build -p codex-cli --bin codey --release

export PATH="<codey binary path>:$PATH" >> ~/.bashrc
```

The resulting binary is written to `target/release/codey`.

## Structure

The workspace is organized under [`src/`](./src) with five top-level domains:

- `harness`: agent shell and execution framework
- `session`: durable task/session state
- `sandbox`: isolated execution environment
- `resources`: tools, skills, MCP, and external capabilities
- `orchestration`: multi-step flow control and component wiring

This repository is licensed under the [Apache-2.0 License](LICENSE).
