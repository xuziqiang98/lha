use crate::product::arg0::arg0_dispatch_or_else;
use crate::product::common::CliConfigOverrides;
use crate::product::mcp_server::run_main;

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|codex_linux_sandbox_exe| async move {
        run_main(codex_linux_sandbox_exe, CliConfigOverrides::default()).await?;
        Ok(())
    })
}
