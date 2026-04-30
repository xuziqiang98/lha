# adam-linux-sandbox

This crate is responsible for producing:

- a `adam-linux-sandbox` standalone executable for Linux that is bundled with the Node.js version of the Adam CLI
- a lib crate that exposes the business logic of the executable as `run_main()` so that
  - the `adam-exec` CLI can check if its arg0 is `adam-linux-sandbox` and, if so, execute as if it were `adam-linux-sandbox`
  - this should also be true of the `codex` multitool CLI
