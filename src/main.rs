use std::process::ExitCode;

use clap::Parser;
use fastsync::cli::Cli;
use fastsync::config::{OutputMode, SyncConfig};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(&cli);

    let output = cli.output;
    let config = match SyncConfig::try_from(cli) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("fastsync: {error}");
            return ExitCode::from(2);
        }
    };

    match fastsync::run_sync(config) {
        Ok(summary) => {
            match output {
                OutputMode::Text => {
                    println!("{}", summary.to_text());
                }
                OutputMode::Json => match serde_json::to_string_pretty(&summary) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!("fastsync: 序列化 JSON 摘要失败: {error}");
                        return ExitCode::from(1);
                    }
                },
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("fastsync: {error}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing(cli: &Cli) {
    let filter = EnvFilter::new(cli.log_level.as_str());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
