use clap::Parser;
use crate::product::arg0::arg0_dispatch_or_else;
use crate::product::common::CliConfigOverrides;
use crate::product::tui_app::Cli;
use crate::product::tui_app::format_exit_messages;
use crate::product::tui_app::run_main;
use supports_color::Stream;

#[derive(Parser, Debug)]
struct TopCli {
    #[clap(flatten)]
    config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    inner: Cli,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|codex_linux_sandbox_exe| async move {
        let top_cli = TopCli::parse();
        let mut inner = top_cli.inner;
        inner
            .config_overrides
            .raw_overrides
            .splice(0..0, top_cli.config_overrides.raw_overrides);
        let exit_info = run_main(inner, codex_linux_sandbox_exe).await?;
        let color_enabled = supports_color::on(Stream::Stdout).is_some();
        for line in format_exit_messages(&exit_info, color_enabled) {
            println!("{line}");
        }
        Ok(())
    })
}
