# lha-linux-sandbox

This crate is responsible for producing:

- a `lha-linux-sandbox` standalone executable for Linux that is bundled with the Node.js version of the LHA CLI
- a lib crate that exposes the business logic of the executable as `run_main()` so that
  - the `lha-exec` CLI can check if its arg0 is `lha-linux-sandbox` and, if so, execute as if it were `lha-linux-sandbox`
  - this should also be true of the `codex` multitool CLI
