use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use fastsync::cli::Cli;
use fastsync::config::{OutputMode, SyncConfig};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    if std::env::args_os().len() == 1 {
        let mut command = Cli::command();
        if let Err(error) = command.print_long_help() {
            eprintln!("fastsync: 输出帮助失败: {error}");
            return ExitCode::from(1);
        }
        println!();
        return ExitCode::SUCCESS;
    }

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
                    let use_color =
                        std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
                    println!("{}", summary.to_text_with_color(use_color));
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
