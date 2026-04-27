set positional-arguments

# Display help
help:
    just -l

# `codex`
alias c := codex
codex *args:
    cargo run --bin codex -- "$@"

# `codex exec`
exec *args:
    cargo run --bin codex -- exec "$@"

# Run the CLI version of the file-search crate.
file-search *args:
    cargo run --bin adam-file-search -- "$@"

# format code
fmt:
    cargo fmt -- --config imports_granularity=Item 2>/dev/null

fix *args:
    cargo clippy --fix --all-features --tests --allow-dirty "$@"

clippy:
    cargo clippy --all-features --tests "$@"

install:
    rustup show active-toolchain
    cargo fetch

# Run `cargo nextest` since it's faster than `cargo test`, though including
# --no-fail-fast is important to ensure all tests are run.
#
# Run `cargo install cargo-nextest` if you don't have it installed.
test:
    cargo nextest run --no-fail-fast

# Regenerate the json schema for config.toml from the current config types.
write-config-schema:
    cargo run -p adam-coding-agent --bin adam-write-config-schema

write-models-schema:
    cargo run -p adam-coding-agent --bin adam-write-models-schema

write-state-schema:
    cargo run -p adam-coding-agent --bin adam-write-state-schema

# Regenerate vendored app-server protocol schema artifacts.
write-app-server-schema:
    cargo run -p adam-app-server-protocol --bin write_schema_fixtures

# Tail logs from the state SQLite database
log *args:
    if [ "${1:-}" = "--" ]; then shift; fi; cargo run -p adam-state --bin logs_client -- "$@"
