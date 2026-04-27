use adam_arg0::arg0_dispatch_or_else;
use adam_common::CliConfigOverrides;
use adam_mcp_server::run_main;

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|codex_linux_sandbox_exe| async move {
        run_main(codex_linux_sandbox_exe, CliConfigOverrides::default()).await?;
        Ok(())
    })
}
