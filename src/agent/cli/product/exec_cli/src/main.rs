//! Entry-point for the `lha-exec` binary.
//!
//! When this CLI is invoked normally, it parses the standard `lha-exec` CLI
//! options and launches the non-interactive LHA agent. However, if it is
//! invoked with arg0 as `lha-linux-sandbox`, we instead treat the invocation
//! as a request to run the logic for the standalone `lha-linux-sandbox`
//! executable (i.e., parse any -s args and then run a *sandboxed* command under
//! Landlock + seccomp.
//!
//! This allows us to ship a completely separate set of functionality as part
//! of the `lha-exec` binary.
use crate::product::arg0::arg0_dispatch_or_else;
use crate::product::exec_cli::parse_with_config_overrides;
use crate::product::exec_cli::run_main;

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|codex_linux_sandbox_exe| async move {
        let cli = parse_with_config_overrides();
        run_main(cli, codex_linux_sandbox_exe).await?;
        Ok(())
    })
}
