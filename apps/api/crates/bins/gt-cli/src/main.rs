//! `gt-cli` entrypoint. Parses args, resolves config from the environment, dispatches, and
//! exits with the command's status code. All logic lives in the lib so the integration test
//! can drive the same code paths in-process.

use clap::Parser;

use gt_cli::{run, Cli, Config};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let cfg = Config::from_env();
    match run(cli, cfg).await {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("gt-cli: {e:#}");
            std::process::exit(1);
        }
    }
}
