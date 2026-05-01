use std::io::IsTerminal;
use std::process::ExitCode;

use fastsync::cli::Cli;
use fastsync::config::{LogLevel, OutputMode, SyncConfig};
use fastsync::i18n::{self, tr};
use fastsync::network::NetworkCommand;
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::filter::IndicatifFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

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
                init_tracing_level(config.log_level, false);
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
                init_tracing_level(config.log_level, false);
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
    let progress = should_enable_progress(cli.output);
    init_tracing_level(cli.log_level, progress);

    let output = cli.output;
    let language = cli.language;
    let config = match SyncConfig::try_from(cli) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("fastsync: {error}");
            return ExitCode::from(2);
        }
    };

    match fastsync::run_sync_with_progress(config, progress) {
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

fn should_enable_progress(output: OutputMode) -> bool {
    output == OutputMode::Text
        && std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
}

fn init_tracing_level(log_level: LogLevel, progress: bool) {
    let filter = EnvFilter::new(log_level.as_str());
    if progress {
        let indicatif_layer = IndicatifLayer::new();
        let stderr_writer = indicatif_layer.get_stderr_writer();
        let indicatif_layer = indicatif_layer.with_filter(IndicatifFilter::new(false));
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(stderr_writer)
            .with_target(false)
            .with_filter(filter);

        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(indicatif_layer)
            .init();
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_filter(filter);

        tracing_subscriber::registry().with(fmt_layer).init();
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn detects_network_subcommands_and_aliases() {
        for command in ["share", "s", "connect", "c"] {
            let args = vec![OsString::from("fastsync"), OsString::from(command)];

            assert!(is_network_command(&args));
        }

        let local_args = vec![
            OsString::from("fastsync"),
            OsString::from("source"),
            OsString::from("target"),
        ];
        assert!(!is_network_command(&local_args));
    }

    #[test]
    fn empty_network_invocation_maps_aliases_to_canonical_help() {
        assert_eq!(
            empty_network_invocation(&[OsString::from("fastsync"), OsString::from("share")]),
            Some("share")
        );
        assert_eq!(
            empty_network_invocation(&[OsString::from("fastsync"), OsString::from("s")]),
            Some("share")
        );
        assert_eq!(
            empty_network_invocation(&[OsString::from("fastsync"), OsString::from("connect")]),
            Some("connect")
        );
        assert_eq!(
            empty_network_invocation(&[OsString::from("fastsync"), OsString::from("c")]),
            Some("connect")
        );
    }

    #[test]
    fn non_empty_network_invocation_is_not_help_fallback() {
        let args = [
            OsString::from("fastsync"),
            OsString::from("share"),
            OsString::from("/tmp/share"),
        ];

        assert_eq!(empty_network_invocation(&args), None);
    }
}
