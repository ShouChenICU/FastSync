use std::io::IsTerminal;
use std::process::ExitCode;

use fastsync::cli::Cli;
use fastsync::config::{LogLevel, OutputMode, SyncConfig};
use fastsync::i18n::{self, tr};
use fastsync::network::NetworkCommand;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let language = Cli::detect_language(&args);
    i18n::set_language(language);

    if args.len() == 1 {
        let mut command = Cli::command(language);
        if let Err(error) = command.print_long_help() {
            eprintln!(
                "fastsync: {}: {error}",
                tr(language, "app.help_print_failed")
            );
            return ExitCode::from(1);
        }
        println!();
        return ExitCode::SUCCESS;
    }

    if is_network_command(&args) {
        if let Some(subcommand) = empty_network_invocation(&args) {
            if let Err(error) = fastsync::network::print_subcommand_help(subcommand, language) {
                eprintln!(
                    "fastsync: {}: {error}",
                    tr(language, "app.help_print_failed")
                );
                return ExitCode::from(1);
            }
            println!();
            return ExitCode::SUCCESS;
        }

        let command = NetworkCommand::parse_from(args, language);
        return match command {
            NetworkCommand::Share(config) => {
                i18n::set_language(config.language);
                init_tracing_level(config.log_level);
                match fastsync::network::run_share(config) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("fastsync: {error}");
                        ExitCode::from(1)
                    }
                }
            }
            NetworkCommand::Connect(config) => {
                i18n::set_language(config.language);
                init_tracing_level(config.log_level);
                match fastsync::network::run_connect(config) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("fastsync: {error}");
                        ExitCode::from(1)
                    }
                }
            }
        };
    }

    let cli = Cli::parse_from(args);
    i18n::set_language(cli.language);
    init_tracing_level(cli.log_level);

    let output = cli.output;
    let language = cli.language;
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
                    println!("{}", summary.to_text_with_language(language, use_color));
                }
                OutputMode::Json => match serde_json::to_string_pretty(&summary) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!(
                            "fastsync: {}: {error}",
                            tr(language, "app.json_summary_failed")
                        );
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

fn is_network_command(args: &[std::ffi::OsString]) -> bool {
    args.get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| matches!(arg, "share" | "s" | "connect" | "c"))
}

fn empty_network_invocation(args: &[std::ffi::OsString]) -> Option<&'static str> {
    if args.len() != 2 {
        return None;
    }

    match args.get(1).and_then(|arg| arg.to_str()) {
        Some("share" | "s") => Some("share"),
        Some("connect" | "c") => Some("connect"),
        _ => None,
    }
}

fn init_tracing_level(log_level: LogLevel) {
    let filter = EnvFilter::new(log_level.as_str());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
