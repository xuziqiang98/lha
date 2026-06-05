use clap::Parser;
use crate::product::file_search::Cli;
use crate::product::file_search::run_cli;

#[derive(Parser)]
struct StandaloneCli {
    #[command(flatten)]
    inner: Cli,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_cli(StandaloneCli::parse().inner).await
}
