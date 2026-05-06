use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::builder::{
    NonEmptyStringValueParser, PossibleValue, PossibleValuesParser, TypedValueParser,
};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use serde::{Deserialize, Serialize};

use crate::filter::{FilterConfig, FilterMode, PathFilter};
use crate::i18n::{Language, set_language, tr};

use super::{MAX_NETWORK_FILE_CONCURRENCY, ShareMode, protocol::TransferOptions};

impl ShareMode {
    pub(super) fn allows(self, direction: SyncDirection) -> bool {
        matches!(
            (self, direction),
            (Self::Send, SyncDirection::Pull)
                | (Self::Receive, SyncDirection::Push)
                | (Self::Both, SyncDirection::Pull | SyncDirection::Push)
        )
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Receive => "receive",
            Self::Both => "both",
        }
    }
}

/// 客户端为一次连接选择的同步方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncDirection {
    /// 从服务端拉取到客户端目录。
    Pull,
    /// 从客户端推送到服务端目录。
    Push,
}

impl SyncDirection {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
        }
    }
}

/// `fastsync share` 的运行配置。
#[derive(Debug, Clone)]
pub struct ShareConfig {
    pub root: PathBuf,
    pub bind: SocketAddr,
    pub mode: ShareMode,
    pub allow_delete: bool,
    pub filter: PathFilter,
    pub code: Option<String>,
    pub max_failures: u8,
    pub language: Language,
    pub log_level: crate::config::LogLevel,
}

/// `fastsync connect` 的运行配置。
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    pub endpoint: String,
    pub directory: PathBuf,
    pub direction: SyncDirection,
    pub delete: bool,
    pub strict: bool,
    pub preserve_times: bool,
    pub preserve_permissions: bool,
    pub network_concurrency: usize,
    pub filter: PathFilter,
    pub code: Option<String>,
    pub language: Language,
    pub log_level: crate::config::LogLevel,
}

/// 解析网络子命令。
pub enum NetworkCommand {
    Share(ShareConfig),
    Connect(ConnectConfig),
}

impl NetworkCommand {
    pub fn parse_from(args: Vec<OsString>, language: Language) -> Self {
        set_language(language);
        let matches = network_command(language).get_matches_from(args);
        match matches.subcommand() {
            Some(("share", matches)) => Self::Share(ShareConfig::from_matches(matches, language)),
            Some(("s", matches)) => Self::Share(ShareConfig::from_matches(matches, language)),
            Some(("connect", matches)) => {
                Self::Connect(ConnectConfig::from_matches(matches, language))
            }
            Some(("c", matches)) => Self::Connect(ConnectConfig::from_matches(matches, language)),
            _ => unreachable!("network subcommand is selected by main"),
        }
    }
}

/// 打印网络子命令帮助，用于 `fastsync share` / `fastsync connect` 无参数时的友好回退。
pub fn print_subcommand_help(name: &str, language: Language) -> std::io::Result<()> {
    set_language(language);
    let mut command = network_command(language);
    if let Some(subcommand) = command.find_subcommand_mut(name) {
        let mut help = subcommand.clone().bin_name(format!("fastsync {name}"));
        help.print_long_help()
    } else {
        command.print_long_help()
    }
}

impl ShareConfig {
    fn from_matches(matches: &ArgMatches, fallback_language: Language) -> Self {
        Self {
            root: matches
                .get_one::<PathBuf>("directory")
                .expect("required by clap")
                .clone(),
            bind: *matches
                .get_one::<SocketAddr>("bind")
                .expect("defaulted by clap"),
            mode: share_mode_from_matches(matches),
            allow_delete: matches.get_flag("allow_delete"),
            filter: filter_from_matches(matches),
            code: matches.get_one::<String>("code").cloned(),
            max_failures: *matches
                .get_one::<u8>("max_failures")
                .expect("defaulted by clap"),
            language: *matches
                .get_one::<Language>("language")
                .unwrap_or(&fallback_language),
            log_level: *matches
                .get_one::<crate::config::LogLevel>("log_level")
                .expect("defaulted by clap"),
        }
    }
}

impl ConnectConfig {
    fn from_matches(matches: &ArgMatches, fallback_language: Language) -> Self {
        Self {
            endpoint: matches
                .get_one::<String>("endpoint")
                .expect("required by clap")
                .clone(),
            directory: matches
                .get_one::<PathBuf>("directory")
                .expect("required by clap")
                .clone(),
            direction: direction_from_matches(matches),
            delete: matches.get_flag("delete"),
            strict: matches.get_flag("strict"),
            preserve_times: *matches.get_one::<bool>("preserve_times").unwrap_or(&true),
            preserve_permissions: matches.get_flag("preserve_permissions"),
            network_concurrency: *matches
                .get_one::<usize>("network_concurrency")
                .expect("defaulted by clap"),
            filter: filter_from_matches(matches),
            code: matches.get_one::<String>("code").cloned(),
            language: *matches
                .get_one::<Language>("language")
                .unwrap_or(&fallback_language),
            log_level: *matches
                .get_one::<crate::config::LogLevel>("log_level")
                .expect("defaulted by clap"),
        }
    }

    pub(super) fn transfer_options(&self) -> TransferOptions {
        TransferOptions {
            delete: self.delete,
            strict: self.strict,
            preserve_times: self.preserve_times,
            preserve_permissions: self.preserve_permissions,
            file_concurrency: self.network_concurrency,
        }
    }
}

fn network_command(language: Language) -> Command {
    Command::new("fastsync")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .subcommand_required(true)
        .subcommand(
            Command::new("share")
                .visible_alias("s")
                .disable_help_flag(true)
                .arg_required_else_help(true)
                .about(tr(language, "network.share.about"))
                .arg(
                    Arg::new("directory")
                        .value_name("DIRECTORY")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .help(tr(language, "network.share.directory")),
                )
                .arg(
                    Arg::new("bind")
                        .short('b')
                        .long("bind")
                        .value_name("ADDR")
                        .value_parser(value_parser!(SocketAddr))
                        .default_value("0.0.0.0:7443")
                        .help(tr(language, "network.share.bind")),
                )
                .arg(
                    Arg::new("mode")
                        .short('m')
                        .long("mode")
                        .value_name("MODE")
                        .value_parser(share_mode_parser())
                        .default_value("send")
                        .help(tr(language, "network.share.mode")),
                )
                .arg(
                    Arg::new("receive")
                        .short('r')
                        .long("receive")
                        .visible_alias("recv")
                        .action(ArgAction::SetTrue)
                        .conflicts_with_all(["mode", "both"])
                        .help(tr(language, "network.share.receive")),
                )
                .arg(
                    Arg::new("both")
                        .short('B')
                        .long("both")
                        .action(ArgAction::SetTrue)
                        .conflicts_with_all(["mode", "receive"])
                        .help(tr(language, "network.share.both")),
                )
                .arg(
                    Arg::new("code")
                        .short('c')
                        .long("code")
                        .value_name("CODE")
                        .value_parser(pairing_code_parser())
                        .help(tr(language, "network.share.code")),
                )
                .arg(
                    Arg::new("allow_delete")
                        .short('a')
                        .long("allow-delete")
                        .action(ArgAction::SetTrue)
                        .help(tr(language, "network.share.allow_delete")),
                )
                .arg(
                    Arg::new("max_failures")
                        .short('f')
                        .long("max-failures")
                        .value_name("N")
                        .value_parser(value_parser!(u8))
                        .default_value("5")
                        .help(tr(language, "network.share.max_failures")),
                )
                .arg(filter_arg(
                    "exclude_from",
                    'x',
                    "exclude-from",
                    FilterMode::Exclude,
                    "network.share.exclude_from",
                    language,
                ))
                .arg(filter_arg(
                    "include_from",
                    'i',
                    "include-from",
                    FilterMode::Include,
                    "network.share.include_from",
                    language,
                ))
                .arg(log_level_arg(language))
                .arg(language_arg(language))
                .arg(help_arg(language)),
        )
        .subcommand(
            Command::new("connect")
                .visible_alias("c")
                .disable_help_flag(true)
                .arg_required_else_help(true)
                .about(tr(language, "network.connect.about"))
                .arg(
                    Arg::new("endpoint")
                        .value_name("ENDPOINT")
                        .required(true)
                        .help(tr(language, "network.connect.endpoint")),
                )
                .arg(
                    Arg::new("directory")
                        .value_name("DIRECTORY")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .help(tr(language, "network.connect.directory")),
                )
                .arg(
                    Arg::new("direction")
                        .short('m')
                        .long("direction")
                        .visible_alias("mode")
                        .value_name("DIRECTION")
                        .value_parser(direction_parser())
                        .default_value("pull")
                        .help(tr(language, "network.connect.direction")),
                )
                .arg(
                    Arg::new("push")
                        .short('u')
                        .long("push")
                        .visible_alias("upload")
                        .action(ArgAction::SetTrue)
                        .conflicts_with_all(["direction", "pull"])
                        .help(tr(language, "network.connect.push")),
                )
                .arg(
                    Arg::new("pull")
                        .long("pull")
                        .visible_alias("download")
                        .action(ArgAction::SetTrue)
                        .conflicts_with_all(["direction", "push"])
                        .help(tr(language, "network.connect.pull")),
                )
                .arg(
                    Arg::new("code")
                        .short('c')
                        .long("code")
                        .value_name("CODE")
                        .value_parser(pairing_code_parser())
                        .help(tr(language, "network.connect.code")),
                )
                .arg(
                    Arg::new("delete")
                        .short('d')
                        .long("delete")
                        .action(ArgAction::SetTrue)
                        .help(tr(language, "network.connect.delete")),
                )
                .arg(
                    Arg::new("strict")
                        .long("strict")
                        .action(ArgAction::SetTrue)
                        .help(tr(language, "network.connect.strict")),
                )
                .arg(
                    Arg::new("preserve_times")
                        .long("no-preserve-times")
                        .visible_alias("no-times")
                        .action(ArgAction::SetFalse)
                        .help(tr(language, "network.connect.no_preserve_times")),
                )
                .arg(
                    Arg::new("preserve_permissions")
                        .short('p')
                        .long("preserve-permissions")
                        .visible_alias("perms")
                        .action(ArgAction::SetTrue)
                        .help(tr(language, "network.connect.preserve_permissions")),
                )
                .arg(
                    Arg::new("network_concurrency")
                        .long("network-concurrency")
                        .visible_alias("file-concurrency")
                        .value_name("N")
                        .value_parser(network_concurrency_parser())
                        .default_value("4")
                        .help(tr(language, "network.connect.network_concurrency")),
                )
                .arg(filter_arg(
                    "exclude_from",
                    'x',
                    "exclude-from",
                    FilterMode::Exclude,
                    "network.connect.exclude_from",
                    language,
                ))
                .arg(filter_arg(
                    "include_from",
                    'i',
                    "include-from",
                    FilterMode::Include,
                    "network.connect.include_from",
                    language,
                ))
                .arg(log_level_arg(language))
                .arg(language_arg(language))
                .arg(help_arg(language)),
        )
}

#[cfg(test)]
pub(super) fn network_command_for_test(language: Language) -> Command {
    network_command(language)
}

fn network_concurrency_parser() -> impl TypedValueParser<Value = usize> + 'static {
    clap::builder::NonEmptyStringValueParser::new().try_map(|value| {
        let concurrency = value
            .parse::<usize>()
            .map_err(|error| format!("invalid network concurrency: {error}"))?;
        if (1..=MAX_NETWORK_FILE_CONCURRENCY).contains(&concurrency) {
            Ok(concurrency)
        } else {
            Err(format!(
                "network concurrency must be between 1 and {MAX_NETWORK_FILE_CONCURRENCY}"
            ))
        }
    })
}

fn filter_arg(
    id: &'static str,
    short: char,
    long: &'static str,
    mode: FilterMode,
    help_key: &str,
    language: Language,
) -> Arg {
    let conflicts_with = if id == "exclude_from" {
        "include_from"
    } else {
        "exclude_from"
    };

    Arg::new(id)
        .short(short)
        .long(long)
        .value_name("FILE")
        .value_parser(filter_parser(mode))
        .conflicts_with(conflicts_with)
        .help(tr(language, help_key))
}

fn filter_from_matches(matches: &ArgMatches) -> PathFilter {
    matches
        .get_one::<PathFilter>("exclude_from")
        .or_else(|| matches.get_one::<PathFilter>("include_from"))
        .cloned()
        .unwrap_or_else(PathFilter::disabled)
}

fn filter_parser(mode: FilterMode) -> impl TypedValueParser<Value = PathFilter> + 'static {
    NonEmptyStringValueParser::new().try_map(move |value| {
        let config = FilterConfig {
            mode,
            path: PathBuf::from(value),
        };
        PathFilter::from_config(Some(&config)).map_err(|error| error.to_string())
    })
}

fn log_level_arg(language: Language) -> Arg {
    Arg::new("log_level")
        .short('l')
        .long("log-level")
        .value_name("LEVEL")
        .value_parser(
            PossibleValuesParser::new(["error", "warn", "info", "debug", "trace"]).map(|value| {
                match value.as_str() {
                    "error" => crate::config::LogLevel::Error,
                    "warn" => crate::config::LogLevel::Warn,
                    "info" => crate::config::LogLevel::Info,
                    "debug" => crate::config::LogLevel::Debug,
                    "trace" => crate::config::LogLevel::Trace,
                    _ => unreachable!("validated by clap possible values"),
                }
            }),
        )
        .default_value("info")
        .help(tr(language, "cli.log_level"))
}

fn language_arg(language: Language) -> Arg {
    Arg::new("language")
        .long("lang")
        .value_name("LOCALE")
        .value_parser(language_parser())
        .default_value(language.as_locale())
        .help(tr(language, "cli.lang"))
}

fn language_parser() -> impl TypedValueParser<Value = Language> + 'static {
    NonEmptyStringValueParser::new().try_map(|value| {
        Language::parse(&value).ok_or_else(|| format!("unsupported locale: {value}"))
    })
}

fn help_arg(language: Language) -> Arg {
    Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::Help)
        .help(tr(language, "cli.help"))
}

fn pairing_code_parser() -> impl TypedValueParser<Value = String> + 'static {
    clap::builder::NonEmptyStringValueParser::new().try_map(|value| {
        validate_pairing_code(&value)
            .map(|()| value)
            .map_err(|message| message.to_string())
    })
}

pub(super) fn validate_pairing_code(code: &str) -> std::result::Result<(), &'static str> {
    if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err("pairing code must be exactly 6 digits")
    }
}

fn share_mode_from_matches(matches: &ArgMatches) -> ShareMode {
    if matches.get_flag("receive") {
        return ShareMode::Receive;
    }
    if matches.get_flag("both") {
        return ShareMode::Both;
    }
    *matches
        .get_one::<ShareMode>("mode")
        .expect("defaulted by clap")
}

fn direction_from_matches(matches: &ArgMatches) -> SyncDirection {
    if matches.get_flag("push") {
        return SyncDirection::Push;
    }
    if matches.get_flag("pull") {
        return SyncDirection::Pull;
    }
    *matches
        .get_one::<SyncDirection>("direction")
        .expect("defaulted by clap")
}

fn share_mode_parser() -> impl TypedValueParser<Value = ShareMode> + 'static {
    PossibleValuesParser::new([
        PossibleValue::new("send").aliases(["s", "download", "down"]),
        PossibleValue::new("receive").aliases(["r", "recv", "upload", "up"]),
        PossibleValue::new("both").alias("b"),
    ])
    .map(|value| match value.as_str() {
        "send" | "s" | "download" | "down" => ShareMode::Send,
        "receive" | "r" | "recv" | "upload" | "up" => ShareMode::Receive,
        "both" | "b" => ShareMode::Both,
        _ => unreachable!("validated by clap possible values"),
    })
}

fn direction_parser() -> impl TypedValueParser<Value = SyncDirection> + 'static {
    PossibleValuesParser::new([
        PossibleValue::new("pull").aliases(["p", "download", "down"]),
        PossibleValue::new("push").aliases(["u", "upload", "up"]),
    ])
    .map(|value| match value.as_str() {
        "pull" | "p" | "download" | "down" => SyncDirection::Pull,
        "push" | "u" | "upload" | "up" => SyncDirection::Push,
        _ => unreachable!("validated by clap possible values"),
    })
}
