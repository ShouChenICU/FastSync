use std::collections::HashSet;
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, UNIX_EPOCH};

use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use quinn::{ClientConfig, Endpoint, RecvStream, SendStream};
use rand::Rng;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info, warn};

use crate::error::{FastSyncError, Result};
use crate::hash::Blake3Digest;
use crate::i18n::{Language, set_language, tr};
use crate::summary::{human_bytes, human_duration};

const DEFAULT_BIND_PORT: u16 = 7443;
const PROTOCOL_VERSION: u16 = 4;
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
const BUFFER_SIZE: usize = 1024 * 1024;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// one-shot 网络共享服务端允许的同步权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    /// 服务端只发送目录，客户端只能 pull。
    Send,
    /// 服务端只接收目录，客户端只能 push。
    Receive,
    /// 服务端允许客户端选择 pull 或 push，仍然只执行一次单向同步。
    Both,
}

impl ShareMode {
    fn allows(self, direction: SyncDirection) -> bool {
        matches!(
            (self, direction),
            (Self::Send, SyncDirection::Pull)
                | (Self::Receive, SyncDirection::Push)
                | (Self::Both, SyncDirection::Pull | SyncDirection::Push)
        )
    }

    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
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
            code: matches.get_one::<String>("code").cloned(),
            language: *matches
                .get_one::<Language>("language")
                .unwrap_or(&fallback_language),
            log_level: *matches
                .get_one::<crate::config::LogLevel>("log_level")
                .expect("defaulted by clap"),
        }
    }

    fn transfer_options(&self) -> TransferOptions {
        TransferOptions {
            delete: self.delete,
            strict: self.strict,
            preserve_times: self.preserve_times,
            preserve_permissions: self.preserve_permissions,
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
                .arg(log_level_arg(language))
                .arg(language_arg(language))
                .arg(help_arg(language)),
        )
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
        .value_parser(
            PossibleValuesParser::new([
                PossibleValue::new("en"),
                PossibleValue::new("zh-CN").aliases(["zh", "zh-cn", "zh_CN"]),
            ])
            .map(|value| Language::parse(&value).expect("validated by clap possible values")),
        )
        .default_value(language.as_locale())
        .help(tr(language, "cli.lang"))
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

fn validate_pairing_code(code: &str) -> std::result::Result<(), &'static str> {
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

/// 启动一次性 QUIC 共享服务端。
pub fn run_share(config: ShareConfig) -> Result<()> {
    install_crypto_provider();
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| other("create tokio runtime", error))?;
    runtime.block_on(run_share_async(config))
}

/// 连接一次性 QUIC 共享服务端并执行同步。
pub fn run_connect(config: ConnectConfig) -> Result<()> {
    install_crypto_provider();
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| other("create tokio runtime", error))?;
    runtime.block_on(run_connect_async(config))
}

fn install_crypto_provider() {
    let _ = quinn::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn run_share_async(config: ShareConfig) -> Result<()> {
    if !config.root.is_dir() {
        return Err(FastSyncError::InvalidSource(config.root));
    }

    let code = config.code.clone().unwrap_or_else(generate_pairing_code);
    let endpoint = Endpoint::server(server_config()?, config.bind)
        .map_err(|error| other("start QUIC server endpoint", error))?;
    let local_addr = endpoint
        .local_addr()
        .map_err(|error| other("read QUIC server local address", error))?;

    println!("{}", tr(config.language, "network.share.started"));
    println!("  endpoint: quic://{local_addr}");
    println!("  code: {code}");
    println!("  mode: {}", config.mode.as_str());
    println!("  allow delete: {}", config.allow_delete);
    println!("  root: {}", config.root.display());
    info!(
        bind = %local_addr,
        root = %config.root.display(),
        mode = config.mode.as_str(),
        allow_delete = config.allow_delete,
        "network share server started"
    );

    let mut failures = 0_u8;
    loop {
        let Some(incoming) = endpoint.accept().await else {
            return Err(other_message("accept QUIC connection", "endpoint closed"));
        };
        let remote = incoming.remote_address();
        info!(remote = %remote, "incoming QUIC connection");

        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(remote = %remote, error = %error, "QUIC handshake failed");
                continue;
            }
        };

        let result = handle_share_connection(&config, &code, connection, remote).await;
        match result {
            Ok(ShareOutcome::Completed(summary)) => {
                println!(
                    "{}",
                    NetworkSummary {
                        side: NetworkSide::Share,
                        direction: summary.direction,
                        directory: &config.root,
                        remote: &remote.to_string(),
                        files: summary.files,
                        bytes: summary.bytes,
                        deleted: summary.deleted,
                        elapsed_ms: summary.elapsed_ms,
                    }
                    .to_text(config.language)
                );
                info!(
                    remote = %remote,
                    direction = summary.direction.as_str(),
                    files = summary.files,
                    bytes = summary.bytes,
                    deleted = summary.deleted,
                    elapsed_ms = summary.elapsed_ms,
                    "network one-shot sync completed"
                );
                endpoint.close(0_u32.into(), b"done");
                endpoint.wait_idle().await;
                return Ok(());
            }
            Ok(ShareOutcome::Rejected(reason)) => {
                failures = failures.saturating_add(1);
                warn!(
                    remote = %remote,
                    reason,
                    failures,
                    max_failures = config.max_failures,
                    "network pairing rejected"
                );
                if failures >= config.max_failures {
                    endpoint.close(1_u32.into(), b"too many failed pairing attempts");
                    return Err(other_message(
                        "pairing failed",
                        "too many failed pairing attempts",
                    ));
                }
            }
            Err(error) => {
                error!(remote = %remote, error = %error, "network share connection failed");
                return Err(error);
            }
        }
    }
}

async fn handle_share_connection(
    config: &ShareConfig,
    code: &str,
    connection: quinn::Connection,
    remote: SocketAddr,
) -> Result<ShareOutcome> {
    let started = Instant::now();
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| other("accept control stream", error))?;
    let hello = read_message(&mut recv).await?;
    let WireMessage::Hello {
        code: provided_code,
        direction,
        protocol,
        options,
    } = hello
    else {
        return reject_pairing(&mut send, "expected hello".to_string()).await;
    };

    info!(
        remote = %remote,
        requested_direction = direction.as_str(),
        protocol,
        delete = options.delete,
        strict = options.strict,
        preserve_times = options.preserve_times,
        preserve_permissions = options.preserve_permissions,
        "pairing hello received"
    );

    if protocol != PROTOCOL_VERSION {
        let reason = format!("unsupported protocol version {protocol}");
        return reject_pairing(&mut send, reason).await;
    }
    if provided_code.trim() != code {
        return reject_pairing(&mut send, "invalid pairing code".to_string()).await;
    }
    if !config.mode.allows(direction) {
        let reason = format!(
            "direction {} is not allowed by server mode {}",
            direction.as_str(),
            config.mode.as_str()
        );
        return reject_pairing(&mut send, reason).await;
    }
    if direction == SyncDirection::Push && options.delete && !config.allow_delete {
        let reason = "server does not allow delete for push".to_string();
        return reject_pairing(&mut send, reason).await;
    }

    write_message(
        &mut send,
        &WireMessage::Accept {
            mode: config.mode.as_str().to_string(),
            delete_allowed: direction == SyncDirection::Pull || config.allow_delete,
        },
    )
    .await?;
    info!(
        remote = %remote,
        direction = direction.as_str(),
        "pairing accepted"
    );

    let summary = match direction {
        SyncDirection::Pull => {
            send_tree(&config.root, &mut send, &mut recv).await?;
            let ack = read_message(&mut recv).await?;
            match ack {
                WireMessage::Ack {
                    files,
                    bytes,
                    deleted,
                } => TransferSummary {
                    direction,
                    files,
                    bytes,
                    deleted,
                    elapsed_ms: started.elapsed().as_millis(),
                },
                _ => {
                    return Err(other_message(
                        "read client acknowledgement",
                        "unexpected message",
                    ));
                }
            }
        }
        SyncDirection::Push => {
            let summary = receive_tree(&config.root, &mut recv, &mut send, options).await?;
            write_message(
                &mut send,
                &WireMessage::Ack {
                    files: summary.files,
                    bytes: summary.bytes,
                    deleted: summary.deleted,
                },
            )
            .await?;
            finish_send_stream(&mut send).await?;
            TransferSummary {
                direction,
                files: summary.files,
                bytes: summary.bytes,
                deleted: summary.deleted,
                elapsed_ms: started.elapsed().as_millis(),
            }
        }
    };

    Ok(ShareOutcome::Completed(summary))
}

async fn reject_pairing(send: &mut SendStream, reason: String) -> Result<ShareOutcome> {
    write_message(
        send,
        &WireMessage::Reject {
            reason: reason.clone(),
        },
    )
    .await?;
    finish_send_stream(send).await?;
    Ok(ShareOutcome::Rejected(reason))
}

async fn run_connect_async(config: ConnectConfig) -> Result<()> {
    let started = Instant::now();
    let code = match config.code {
        Some(ref code) => code.clone(),
        None => prompt_code()?,
    };
    let remote_addr = resolve_endpoint(&config.endpoint).await?;
    let mut endpoint = Endpoint::client((Ipv4Addr::UNSPECIFIED, 0).into())
        .map_err(|error| other("create QUIC client endpoint", error))?;
    endpoint.set_default_client_config(insecure_client_config());

    info!(
        endpoint = config.endpoint,
        remote = %remote_addr,
        direction = config.direction.as_str(),
        directory = %config.directory.display(),
        "connecting to network share"
    );

    let connection = endpoint
        .connect(remote_addr, "fastsync.local")
        .map_err(|error| other("start QUIC client connection", error))?
        .await
        .map_err(|error| other("establish QUIC client connection", error))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| other("open control stream", error))?;

    write_message(
        &mut send,
        &WireMessage::Hello {
            code,
            direction: config.direction,
            protocol: PROTOCOL_VERSION,
            options: config.transfer_options(),
        },
    )
    .await?;

    match read_message(&mut recv).await? {
        WireMessage::Accept {
            mode,
            delete_allowed,
        } => {
            info!(
                server_mode = mode,
                delete_allowed, "pairing accepted by server"
            );
        }
        WireMessage::Reject { reason } => {
            return Err(other_message("pairing rejected", reason));
        }
        _ => return Err(other_message("pairing", "unexpected server response")),
    }

    let summary = match config.direction {
        SyncDirection::Pull => {
            let summary = receive_tree(
                &config.directory,
                &mut recv,
                &mut send,
                config.transfer_options(),
            )
            .await?;
            write_message(
                &mut send,
                &WireMessage::Ack {
                    files: summary.files,
                    bytes: summary.bytes,
                    deleted: summary.deleted,
                },
            )
            .await?;
            finish_send_stream(&mut send).await?;
            summary
        }
        SyncDirection::Push => {
            send_tree(&config.directory, &mut send, &mut recv).await?;
            match read_message(&mut recv).await? {
                WireMessage::Ack {
                    files,
                    bytes,
                    deleted,
                } => ReceiveSummary {
                    files,
                    bytes,
                    deleted,
                },
                _ => {
                    return Err(other_message(
                        "read server acknowledgement",
                        "unexpected message",
                    ));
                }
            }
        }
    };

    println!(
        "{}",
        NetworkSummary {
            side: NetworkSide::Connect,
            direction: config.direction,
            directory: &config.directory,
            remote: &remote_addr.to_string(),
            files: summary.files,
            bytes: summary.bytes,
            deleted: summary.deleted,
            elapsed_ms: started.elapsed().as_millis(),
        }
        .to_text(config.language)
    );
    info!(
        direction = config.direction.as_str(),
        files = summary.files,
        bytes = summary.bytes,
        deleted = summary.deleted,
        "network client sync completed"
    );
    connection.close(0_u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(())
}

async fn finish_send_stream(send: &mut SendStream) -> Result<()> {
    send.finish()
        .map_err(|error| other("finish QUIC send stream", error))?;
    send.stopped()
        .await
        .map_err(|error| other("wait for QUIC send stream delivery", error))?;
    Ok(())
}

async fn send_tree(root: &Path, send: &mut SendStream, recv: &mut RecvStream) -> Result<()> {
    let manifest = send_manifest(root, send).await?;
    let mut requested = read_requested_paths(recv).await?;

    let mut total_bytes = 0_u64;
    let mut sent_files = 0_usize;
    for file in &manifest.files {
        if !requested.remove(&file.path) {
            continue;
        }
        info!(
            path = %file.path.display(),
            bytes = file.len,
            "sending file"
        );
        write_message(send, &WireMessage::File(file.clone())).await?;
        send_file(root, file, send).await?;
        sent_files += 1;
        total_bytes = total_bytes.saturating_add(file.len);
    }
    if !requested.is_empty() {
        return Err(other_message(
            "send requested network files",
            "peer requested paths outside the manifest",
        ));
    }

    write_message(send, &WireMessage::Done).await?;
    send.finish()
        .map_err(|error| other("finish QUIC send stream", error))?;
    info!(
        files = sent_files,
        bytes = total_bytes,
        "finished sending tree"
    );
    Ok(())
}

async fn send_manifest(root: &Path, send: &mut SendStream) -> Result<Manifest> {
    let snapshot = crate::scan::scan_directory(root, false)?;
    let mut manifest = Manifest {
        dirs: Vec::new(),
        files: Vec::new(),
    };

    write_message(send, &WireMessage::ManifestStart).await?;
    for entry in snapshot.entries.values() {
        match entry.kind {
            crate::scan::EntryKind::Directory => {
                let dir = DirManifest {
                    path: entry.relative_path.clone(),
                    metadata: WireMetadata::from_entry(entry),
                };
                write_message(send, &WireMessage::ManifestDir(dir.clone())).await?;
                manifest.dirs.push(dir);
            }
            crate::scan::EntryKind::File => {
                let digest = crate::hash::blake3_file(&entry.absolute_path)?;
                let file = FileManifest {
                    path: entry.relative_path.clone(),
                    len: entry.len,
                    blake3: hex_digest(digest),
                    metadata: WireMetadata::from_entry(entry),
                };
                write_message(send, &WireMessage::ManifestFile(file.clone())).await?;
                manifest.files.push(file);
            }
            crate::scan::EntryKind::Symlink => {}
        }
    }
    write_message(send, &WireMessage::ManifestEnd).await?;

    info!(
        root = %root.display(),
        dirs = manifest.dirs.len(),
        files = manifest.files.len(),
        bytes = manifest.files.iter().map(|file| file.len).sum::<u64>(),
        "sent manifest"
    );
    Ok(manifest)
}

async fn read_requested_paths(recv: &mut RecvStream) -> Result<HashSet<PathBuf>> {
    let mut requested = HashSet::new();
    loop {
        match read_message(recv).await? {
            WireMessage::RequestFile { path } => {
                requested.insert(path);
            }
            WireMessage::RequestEnd => break,
            _ => {
                return Err(other_message(
                    "read requested network files",
                    "unexpected message",
                ));
            }
        }
    }
    Ok(requested)
}

async fn receive_tree(
    root: &Path,
    recv: &mut RecvStream,
    send: &mut SendStream,
    options: TransferOptions,
) -> Result<ReceiveSummary> {
    let manifest = receive_manifest(root, recv, options).await?;
    info!(
        root = %root.display(),
        dirs = manifest.dirs.len(),
        files = manifest.files.len(),
        bytes = manifest.files.iter().map(|file| file.len).sum::<u64>(),
        "receiving manifest"
    );
    let requested_files = send_file_requests(root, &manifest, options.strict, send).await?;
    info!(
        requested_files,
        skipped_files = manifest.files.len().saturating_sub(requested_files),
        strict = options.strict,
        "planned network file requests"
    );

    let mut files = 0_usize;
    let mut bytes = 0_u64;
    loop {
        match read_message(recv).await? {
            WireMessage::File(file) => {
                info!(
                    path = %file.path.display(),
                    bytes = file.len,
                    "receiving file"
                );
                receive_file(root, &file, recv, options).await?;
                files += 1;
                bytes = bytes.saturating_add(file.len);
            }
            WireMessage::Done => break,
            _ => return Err(other_message("receive tree", "unexpected message")),
        }
    }
    let deleted = if options.delete {
        delete_obsolete(root, &manifest).await?
    } else {
        0
    };
    apply_file_metadata(root, &manifest.files, options)?;
    apply_directory_metadata(root, &manifest.dirs, options)?;
    info!(files, bytes, deleted, "finished receiving tree");
    Ok(ReceiveSummary {
        files,
        bytes,
        deleted,
    })
}

async fn receive_manifest(
    root: &Path,
    recv: &mut RecvStream,
    options: TransferOptions,
) -> Result<Manifest> {
    match read_message(recv).await? {
        WireMessage::ManifestStart => {}
        _ => return Err(other_message("receive manifest", "unexpected message")),
    }

    tokio::fs::create_dir_all(root)
        .await
        .map_err(|error| io_path("create receive root", root, error))?;

    let mut manifest = Manifest {
        dirs: Vec::new(),
        files: Vec::new(),
    };
    loop {
        match read_message(recv).await? {
            WireMessage::ManifestDir(dir) => {
                let path = safe_join(root, &dir.path)?;
                ensure_directory_path(&path, options.delete).await?;
                tokio::fs::create_dir_all(&path)
                    .await
                    .map_err(|error| io_path("create received directory", &path, error))?;
                manifest.dirs.push(dir);
            }
            WireMessage::ManifestFile(file) => manifest.files.push(file),
            WireMessage::ManifestEnd => break,
            _ => return Err(other_message("receive manifest", "unexpected message")),
        }
    }

    Ok(manifest)
}

async fn send_file_requests(
    root: &Path,
    manifest: &Manifest,
    strict: bool,
    send: &mut SendStream,
) -> Result<usize> {
    let target_snapshot = match crate::scan::scan_optional_directory(root, false) {
        Ok(snapshot) => snapshot,
        Err(FastSyncError::InvalidTarget(path)) if path == root => {
            let mut requested = 0_usize;
            for file in &manifest.files {
                write_message(
                    send,
                    &WireMessage::RequestFile {
                        path: file.path.clone(),
                    },
                )
                .await?;
                requested += 1;
            }
            write_message(send, &WireMessage::RequestEnd).await?;
            return Ok(requested);
        }
        Err(error) => return Err(error),
    };

    let mut requested = 0_usize;
    for file in &manifest.files {
        if should_request_file(&target_snapshot, file, strict)? {
            write_message(
                send,
                &WireMessage::RequestFile {
                    path: file.path.clone(),
                },
            )
            .await?;
            requested += 1;
        }
    }
    write_message(send, &WireMessage::RequestEnd).await?;
    Ok(requested)
}

#[cfg(test)]
fn request_files_for_local_state(
    root: &Path,
    manifest: &Manifest,
    strict: bool,
) -> Result<Vec<PathBuf>> {
    let target_snapshot = match crate::scan::scan_optional_directory(root, false) {
        Ok(snapshot) => snapshot,
        Err(FastSyncError::InvalidTarget(path)) if path == root => {
            return Ok(manifest
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect());
        }
        Err(error) => return Err(error),
    };

    let mut requested = Vec::new();
    for file in &manifest.files {
        if should_request_file(&target_snapshot, file, strict)? {
            requested.push(file.path.clone());
        }
    }

    Ok(requested)
}

fn should_request_file(
    target_snapshot: &crate::scan::Snapshot,
    file: &FileManifest,
    strict: bool,
) -> Result<bool> {
    let Some(target_entry) = target_snapshot.get(&file.path) else {
        return Ok(true);
    };

    if !target_entry.is_file() || target_entry.len != file.len {
        return Ok(true);
    }

    if !strict && content_metadata_matches(target_entry, &file.metadata) {
        Ok(false)
    } else {
        let digest = crate::hash::blake3_file(&target_entry.absolute_path)?;
        Ok(hex_digest(digest) != file.blake3)
    }
}

fn content_metadata_matches(entry: &crate::scan::FileEntry, metadata: &WireMetadata) -> bool {
    metadata_time_matches(entry, metadata) && metadata_permissions_match(entry, metadata)
}

fn metadata_time_matches(entry: &crate::scan::FileEntry, metadata: &WireMetadata) -> bool {
    let Some(source_secs) = metadata.modified_secs else {
        return entry.modified.is_none();
    };
    let Some(source_nanos) = metadata.modified_nanos else {
        return entry.modified.is_none();
    };
    let Some(target_modified) = entry.modified else {
        return false;
    };
    let Ok(target_duration) = target_modified.duration_since(UNIX_EPOCH) else {
        return false;
    };

    target_duration.as_secs() as i64 == source_secs
        && target_duration.subsec_nanos() == source_nanos
}

fn metadata_permissions_match(entry: &crate::scan::FileEntry, metadata: &WireMetadata) -> bool {
    if entry.readonly != metadata.readonly {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.unix_mode.is_none_or(|mode| entry.mode == mode)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
fn build_manifest(root: &Path) -> Result<Manifest> {
    let snapshot = crate::scan::scan_directory(root, false)?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in snapshot.entries.values() {
        match entry.kind {
            crate::scan::EntryKind::Directory => dirs.push(DirManifest {
                path: entry.relative_path.clone(),
                metadata: WireMetadata::from_entry(entry),
            }),
            crate::scan::EntryKind::File => {
                let digest = crate::hash::blake3_file(&entry.absolute_path)?;
                files.push(FileManifest {
                    path: entry.relative_path.clone(),
                    len: entry.len,
                    blake3: hex_digest(digest),
                    metadata: WireMetadata::from_entry(entry),
                });
            }
            crate::scan::EntryKind::Symlink => {}
        }
    }

    Ok(Manifest { dirs, files })
}

async fn send_file(root: &Path, file: &FileManifest, send: &mut SendStream) -> Result<()> {
    let path = safe_join(root, &file.path)?;
    let mut input = tokio::fs::File::open(&path)
        .await
        .map_err(|error| io_path("open file for network send", &path, error))?;
    let mut remaining = file.len;
    let mut buffer = vec![0_u8; BUFFER_SIZE];

    while remaining > 0 {
        let read = input
            .read(&mut buffer)
            .await
            .map_err(|error| io_path("read file for network send", &path, error))?;
        if read == 0 {
            return Err(other_message(
                "send file",
                format!("file ended early: {}", file.path.display()),
            ));
        }
        send.write_all(&buffer[..read])
            .await
            .map_err(|error| other("write file chunk to QUIC stream", error))?;
        remaining = remaining.saturating_sub(read as u64);
    }

    Ok(())
}

async fn receive_file(
    root: &Path,
    file: &FileManifest,
    recv: &mut RecvStream,
    options: TransferOptions,
) -> Result<()> {
    let target = safe_join(root, &file.path)?;
    let Some(parent) = target.parent() else {
        return Err(other_message("receive file", "target path has no parent"));
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| io_path("create received file parent", parent, error))?;
    ensure_file_path(&target, options.delete).await?;
    let temp_path = unique_temp_path(parent);
    let mut output = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|error| io_path("create network temp file", &temp_path, error))?;
    let mut hasher = blake3::Hasher::new();
    let mut remaining = file.len;
    let mut buffer = vec![0_u8; BUFFER_SIZE];

    while remaining > 0 {
        let read_len = next_chunk_len(remaining, buffer.len());
        let Some(read) = recv
            .read(&mut buffer[..read_len])
            .await
            .map_err(|error| other("read file chunk from QUIC stream", error))?
        else {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(other_message(
                "receive file",
                format!(
                    "stream ended before file completed: {}",
                    file.path.display()
                ),
            ));
        };
        output
            .write_all(&buffer[..read])
            .await
            .map_err(|error| io_path("write network temp file", &temp_path, error))?;
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    output
        .flush()
        .await
        .map_err(|error| io_path("flush network temp file", &temp_path, error))?;
    output
        .sync_data()
        .await
        .map_err(|error| io_path("sync network temp file", &temp_path, error))?;
    drop(output);

    let actual = hex_digest(*hasher.finalize().as_bytes());
    if actual != file.blake3 {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(other_message(
            "verify received file",
            format!("BLAKE3 mismatch: {}", file.path.display()),
        ));
    }

    match tokio::fs::rename(&temp_path, &target).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            tokio::fs::remove_file(&target).await.map_err(|error| {
                io_path("remove old target before network replace", &target, error)
            })?;
            tokio::fs::rename(&temp_path, &target)
                .await
                .map_err(|error| io_path("rename network temp file", &target, error))
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            Err(io_path("rename network temp file", &target, error))
        }
    }?;
    apply_path_metadata(&target, &file.metadata, options)
}

fn next_chunk_len(remaining: u64, buffer_len: usize) -> usize {
    if remaining > buffer_len as u64 {
        buffer_len
    } else {
        remaining as usize
    }
}

async fn ensure_directory_path(path: &Path, delete_enabled: bool) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(metadata) if delete_enabled => {
            if metadata.is_dir() {
                tokio::fs::remove_dir_all(path).await.map_err(|error| {
                    io_path("remove directory before network replace", path, error)
                })
            } else {
                tokio::fs::remove_file(path)
                    .await
                    .map_err(|error| io_path("remove file before network directory", path, error))
            }
        }
        Ok(_) => Err(other_message(
            "create received directory",
            format!(
                "target path exists and is not a directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_path("read target metadata", path, error)),
    }
}

async fn ensure_file_path(path: &Path, delete_enabled: bool) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() && delete_enabled => tokio::fs::remove_dir_all(path)
            .await
            .map_err(|error| io_path("remove directory before network file", path, error)),
        Ok(metadata) if metadata.is_dir() => Err(other_message(
            "receive file",
            format!("target path exists and is a directory: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_path("read target metadata", path, error)),
    }
}

async fn delete_obsolete(root: &Path, manifest: &Manifest) -> Result<usize> {
    let target_snapshot = match crate::scan::scan_optional_directory(root, false) {
        Ok(snapshot) => snapshot,
        Err(FastSyncError::InvalidTarget(path)) if path == root => return Ok(0),
        Err(error) => return Err(error),
    };
    let desired_dirs: HashSet<_> = manifest.dirs.iter().map(|dir| dir.path.clone()).collect();
    let desired_files: HashSet<_> = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let mut obsolete: Vec<_> = target_snapshot
        .entries
        .values()
        .filter(|entry| {
            !desired_dirs.contains(&entry.relative_path)
                && !desired_files.contains(&entry.relative_path)
        })
        .cloned()
        .collect();
    obsolete.sort_by_key(|entry| std::cmp::Reverse(entry.relative_path.components().count()));

    let mut deleted = 0_usize;
    for entry in obsolete {
        let path = safe_join(root, &entry.relative_path)?;
        match entry.kind {
            crate::scan::EntryKind::Directory => {
                tokio::fs::remove_dir(&path)
                    .await
                    .map_err(|error| io_path("delete obsolete network directory", &path, error))?;
            }
            crate::scan::EntryKind::File | crate::scan::EntryKind::Symlink => {
                tokio::fs::remove_file(&path)
                    .await
                    .map_err(|error| io_path("delete obsolete network file", &path, error))?;
            }
        }
        deleted += 1;
        info!(path = %entry.relative_path.display(), "deleted obsolete network entry");
    }

    Ok(deleted)
}

fn apply_directory_metadata(
    root: &Path,
    dirs: &[DirManifest],
    options: TransferOptions,
) -> Result<()> {
    let mut dirs = dirs.to_vec();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.path.components().count()));
    for dir in dirs {
        let path = safe_join(root, &dir.path)?;
        apply_path_metadata(&path, &dir.metadata, options)?;
    }
    Ok(())
}

fn apply_file_metadata(
    root: &Path,
    files: &[FileManifest],
    options: TransferOptions,
) -> Result<()> {
    for file in files {
        let path = safe_join(root, &file.path)?;
        apply_path_metadata(&path, &file.metadata, options)?;
    }
    Ok(())
}

fn apply_path_metadata(
    path: &Path,
    metadata: &WireMetadata,
    options: TransferOptions,
) -> Result<()> {
    if options.preserve_permissions {
        set_permissions(path, metadata)?;
    }
    if options.preserve_times {
        if let Some(mtime) = metadata.modified_filetime() {
            filetime::set_file_mtime(path, mtime)
                .map_err(|error| io_path("set received path modified time", path, error))?;
        }
    }
    Ok(())
}

fn set_permissions(path: &Path, metadata: &WireMetadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = metadata.unix_mode {
            let permissions = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(path, permissions)
                .map_err(|error| io_path("set received path permissions", path, error))?;
            return Ok(());
        }
    }

    let mut permissions = std::fs::metadata(path)
        .map_err(|error| io_path("read received path permissions", path, error))?
        .permissions();
    permissions.set_readonly(metadata.readonly);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| io_path("set received path permissions", path, error))
}

async fn write_message(send: &mut SendStream, message: &WireMessage) -> Result<()> {
    let payload =
        serde_json::to_vec(message).map_err(|error| other("encode network message", error))?;
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(other_message(
            "encode network message",
            "message is too large",
        ));
    }
    send.write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .map_err(|error| other("write network message length", error))?;
    send.write_all(&payload)
        .await
        .map_err(|error| other("write network message payload", error))?;
    Ok(())
}

async fn read_message(recv: &mut RecvStream) -> Result<WireMessage> {
    let mut len = [0_u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|error| other("read network message length", error))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(other_message(
            "read network message",
            "message is too large",
        ));
    }
    let mut payload = vec![0_u8; len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|error| other("read network message payload", error))?;
    serde_json::from_slice(&payload).map_err(|error| other("decode network message", error))
}

fn server_config() -> Result<quinn::ServerConfig> {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["fastsync.local".to_string(), "localhost".to_string()])
            .map_err(|error| other("generate temporary QUIC certificate", error))?;
    quinn::ServerConfig::with_single_cert(vec![cert.der().clone()], signing_key.into())
        .map_err(|error| other("create QUIC server TLS config", error))
}

fn insecure_client_config() -> ClientConfig {
    let crypto = quinn::rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(
            rustls_dangerous::NoCertificateVerification,
        ))
        .with_no_client_auth();
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .expect("rustls client config must contain a QUIC initial cipher suite");
    ClientConfig::new(std::sync::Arc::new(crypto))
}

async fn resolve_endpoint(endpoint: &str) -> Result<SocketAddr> {
    let raw = endpoint.strip_prefix("quic://").unwrap_or(endpoint);
    let (host, port) = match raw.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|error| other("parse QUIC endpoint port", error))?;
            (host, port)
        }
        None => (raw, DEFAULT_BIND_PORT),
    };
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| other("resolve QUIC endpoint", error))?;
    addrs
        .next()
        .ok_or_else(|| other_message("resolve QUIC endpoint", "no address resolved"))
}

fn prompt_code() -> Result<String> {
    eprint!("Pairing code: ");
    let mut code = String::new();
    std::io::stdin()
        .read_line(&mut code)
        .map_err(|error| other("read pairing code", error))?;
    let code = code.trim().to_string();
    validate_pairing_code(&code).map_err(|message| other_message("read pairing code", message))?;
    Ok(code)
}

fn generate_pairing_code() -> String {
    let mut rng = rand::rng();
    let code: u32 = rng.random_range(0..=999_999);
    format!("{code:06}")
}

fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(FastSyncError::PathOutsideRoot {
            path: relative.to_path_buf(),
        });
    }
    Ok(root.join(relative))
}

fn unique_temp_path(parent: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".fastsync.net.tmp.{}.{}",
        std::process::id(),
        counter
    ))
}

fn hex_digest(digest: Blake3Digest) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn other(context: impl Into<String>, error: impl std::fmt::Display) -> FastSyncError {
    FastSyncError::Io {
        context: context.into(),
        source: std::io::Error::other(error.to_string()),
    }
}

fn other_message(context: impl Into<String>, message: impl Into<String>) -> FastSyncError {
    FastSyncError::Io {
        context: context.into(),
        source: std::io::Error::other(message.into()),
    }
}

fn io_path(context: &'static str, path: &Path, error: std::io::Error) -> FastSyncError {
    FastSyncError::Io {
        context: format!("{context}: {}", path.display()),
        source: error,
    }
}

#[derive(Debug)]
enum ShareOutcome {
    Completed(TransferSummary),
    Rejected(String),
}

#[derive(Debug)]
struct TransferSummary {
    direction: SyncDirection,
    files: usize,
    bytes: u64,
    deleted: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct TransferOptions {
    delete: bool,
    strict: bool,
    preserve_times: bool,
    preserve_permissions: bool,
}

#[derive(Debug)]
struct ReceiveSummary {
    files: usize,
    bytes: u64,
    deleted: usize,
}

#[derive(Debug, Clone, Copy)]
enum NetworkSide {
    Share,
    Connect,
}

struct NetworkSummary<'a> {
    side: NetworkSide,
    direction: SyncDirection,
    directory: &'a Path,
    remote: &'a str,
    files: usize,
    bytes: u64,
    deleted: usize,
    elapsed_ms: u128,
}

impl NetworkSummary<'_> {
    /// 渲染网络会话摘要；只使用双方都能可靠得知的传输结果。
    fn to_text(&self, language: Language) -> String {
        let side = match self.side {
            NetworkSide::Share => tr(language, "network.summary.side_share"),
            NetworkSide::Connect => tr(language, "network.summary.side_connect"),
        };
        let direction = match self.direction {
            SyncDirection::Pull => tr(language, "network.summary.direction_pull"),
            SyncDirection::Push => tr(language, "network.summary.direction_push"),
        };

        format!(
            "\
fastsync {}

{}
  {}  {}
  {}  {}
  {}  {}
  {}  {}

{}
  {}  {}
  {}  {}
  {}  {}
  {}  {}
",
            tr(language, "network.summary.title"),
            tr(language, "network.summary.session"),
            tr(language, "network.summary.side"),
            side,
            tr(language, "network.summary.direction"),
            direction,
            tr(language, "network.summary.directory"),
            self.directory.display(),
            tr(language, "network.summary.remote"),
            self.remote,
            tr(language, "network.summary.result"),
            tr(language, "network.summary.files"),
            self.files,
            tr(language, "network.summary.data"),
            human_bytes(self.bytes),
            tr(language, "network.summary.deleted"),
            self.deleted,
            tr(language, "network.summary.duration"),
            human_duration(self.elapsed_ms),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    dirs: Vec<DirManifest>,
    files: Vec<FileManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirManifest {
    #[serde(with = "wire_path")]
    path: PathBuf,
    metadata: WireMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileManifest {
    #[serde(with = "wire_path")]
    path: PathBuf,
    len: u64,
    blake3: String,
    metadata: WireMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireMetadata {
    modified_secs: Option<i64>,
    modified_nanos: Option<u32>,
    readonly: bool,
    unix_mode: Option<u32>,
}

impl WireMetadata {
    fn from_entry(entry: &crate::scan::FileEntry) -> Self {
        let (modified_secs, modified_nanos) = entry
            .modified
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| {
                (
                    Some(duration.as_secs() as i64),
                    Some(duration.subsec_nanos()),
                )
            })
            .unwrap_or((None, None));

        Self {
            modified_secs,
            modified_nanos,
            readonly: entry.readonly,
            #[cfg(unix)]
            unix_mode: Some(entry.mode),
            #[cfg(not(unix))]
            unix_mode: None,
        }
    }

    fn modified_filetime(&self) -> Option<filetime::FileTime> {
        Some(filetime::FileTime::from_unix_time(
            self.modified_secs?,
            self.modified_nanos?,
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMessage {
    Hello {
        code: String,
        direction: SyncDirection,
        protocol: u16,
        options: TransferOptions,
    },
    Accept {
        mode: String,
        delete_allowed: bool,
    },
    Reject {
        reason: String,
    },
    ManifestStart,
    ManifestDir(DirManifest),
    ManifestFile(FileManifest),
    ManifestEnd,
    RequestFile {
        #[serde(with = "wire_path")]
        path: PathBuf,
    },
    RequestEnd,
    File(FileManifest),
    Done,
    Ack {
        files: usize,
        bytes: u64,
        deleted: usize,
    },
}

mod wire_path {
    use std::path::{Component, Path, PathBuf};

    use serde::{Deserialize, Deserializer, Serializer};

    /// 将平台相关相对路径编码成 `/` 分隔的网络路径，避免 Windows `\` 泄漏到 Android/Linux。
    pub fn serialize<S>(path: &Path, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let path = path_to_wire_string(path).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&path)
    }

    /// 解码网络相对路径；兼容旧版本 Windows 端发送的 `\` 分隔路径。
    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<PathBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        wire_string_to_path(&value).map_err(serde::de::Error::custom)
    }

    pub fn path_to_wire_string(path: &Path) -> std::result::Result<String, String> {
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => {
                    let Some(value) = value.to_str() else {
                        return Err("network paths must be valid UTF-8".to_string());
                    };
                    if value.contains(['/', '\\']) {
                        return Err("network path components cannot contain separators".to_string());
                    }
                    components.push(value);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(
                        "network paths must be relative and stay inside the root".to_string()
                    );
                }
            }
        }

        if components.is_empty() {
            return Err("network path cannot be empty".to_string());
        }
        Ok(components.join("/"))
    }

    pub fn wire_string_to_path(value: &str) -> std::result::Result<PathBuf, String> {
        if value.is_empty() {
            return Err("network path cannot be empty".to_string());
        }

        let mut path = PathBuf::new();
        for component in value.split(['/', '\\']) {
            if component.is_empty() || component == "." || component == ".." {
                return Err("network paths must be relative and stay inside the root".to_string());
            }
            if Path::new(component)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("network path components cannot contain separators".to_string());
            }
            path.push(component);
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_mode_only_allows_pull() {
        assert!(ShareMode::Send.allows(SyncDirection::Pull));
        assert!(!ShareMode::Send.allows(SyncDirection::Push));
    }

    #[test]
    fn safe_join_rejects_escape_paths() {
        assert!(safe_join(Path::new("/tmp/root"), Path::new("../x")).is_err());
        assert!(safe_join(Path::new("/tmp/root"), Path::new("/x")).is_err());
    }

    #[test]
    fn wire_paths_serialize_with_forward_slashes()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let file = FileManifest {
            path: PathBuf::from("mc").join("Aaron.flac"),
            len: 0,
            blake3: String::new(),
            metadata: WireMetadata {
                modified_secs: None,
                modified_nanos: None,
                readonly: false,
                unix_mode: None,
            },
        };
        let request = WireMessage::RequestFile {
            path: PathBuf::from("mc").join("Aaron.flac"),
        };

        let file_json = serde_json::to_string(&file)?;
        let request_json = serde_json::to_string(&request)?;

        assert!(file_json.contains(r#""path":"mc/Aaron.flac""#));
        assert!(request_json.contains(r#""path":"mc/Aaron.flac""#));
        Ok(())
    }

    #[test]
    fn wire_paths_accept_legacy_windows_separators()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let file_json = r#"{
            "path":"mc\\Aaron.flac",
            "len":0,
            "blake3":"",
            "metadata":{
                "modified_secs":null,
                "modified_nanos":null,
                "readonly":false,
                "unix_mode":null
            }
        }"#;
        let request_json = r#"{"type":"request_file","path":"mc\\Aaron.flac"}"#;

        let file = serde_json::from_str::<FileManifest>(file_json)?;
        let request = serde_json::from_str::<WireMessage>(request_json)?;

        assert_eq!(file.path, PathBuf::from("mc").join("Aaron.flac"));
        match request {
            WireMessage::RequestFile { path } => {
                assert_eq!(path, PathBuf::from("mc").join("Aaron.flac"));
            }
            _ => panic!("expected request_file message"),
        }
        Ok(())
    }

    #[test]
    fn wire_paths_reject_escape_paths() {
        let file_json = r#"{
            "path":"../x",
            "len":0,
            "blake3":"",
            "metadata":{
                "modified_secs":null,
                "modified_nanos":null,
                "readonly":false,
                "unix_mode":null
            }
        }"#;

        assert!(serde_json::from_str::<FileManifest>(file_json).is_err());
    }

    #[test]
    fn streaming_manifest_keeps_each_message_small()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let files: Vec<_> = (0..20_000)
            .map(|index| FileManifest {
                path: PathBuf::from("album").join(format!("track-{index:05}.flac")),
                len: 12_345,
                blake3: "0".repeat(64),
                metadata: WireMetadata {
                    modified_secs: Some(1_700_000_000),
                    modified_nanos: Some(0),
                    readonly: false,
                    unix_mode: None,
                },
            })
            .collect();
        let old_payload = serde_json::to_vec(&Manifest {
            dirs: Vec::new(),
            files: files.clone(),
        })?;

        let largest_stream_item = files
            .iter()
            .map(|file| serde_json::to_vec(&WireMessage::ManifestFile(file.clone())))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|payload| payload.len())
            .max()
            .expect("test manifest should contain files");

        assert!(old_payload.len() > MAX_MESSAGE_SIZE);
        assert!(largest_stream_item < MAX_MESSAGE_SIZE);
        Ok(())
    }

    #[test]
    fn next_chunk_len_handles_remaining_larger_than_usize() {
        assert_eq!(next_chunk_len(u64::MAX, 1024), 1024);
        assert_eq!(next_chunk_len(512, 1024), 512);
        assert_eq!(next_chunk_len(0, 1024), 0);
    }

    #[test]
    fn share_shortcuts_parse_to_receive_mode() {
        let command = NetworkCommand::parse_from(
            vec![
                OsString::from("fastsync"),
                OsString::from("s"),
                OsString::from("/tmp/inbox"),
                OsString::from("-r"),
                OsString::from("-a"),
                OsString::from("-c"),
                OsString::from("123456"),
                OsString::from("-f"),
                OsString::from("2"),
            ],
            Language::DEFAULT,
        );

        let NetworkCommand::Share(config) = command else {
            panic!("expected share command");
        };

        assert_eq!(config.mode, ShareMode::Receive);
        assert!(config.allow_delete);
        assert_eq!(config.code.as_deref(), Some("123456"));
        assert_eq!(config.max_failures, 2);
    }

    #[test]
    fn connect_shortcuts_parse_to_push_with_delete() {
        let command = NetworkCommand::parse_from(
            vec![
                OsString::from("fastsync"),
                OsString::from("c"),
                OsString::from("example.com"),
                OsString::from("/tmp/project"),
                OsString::from("-u"),
                OsString::from("-d"),
                OsString::from("--strict"),
                OsString::from("-p"),
                OsString::from("-c"),
                OsString::from("123456"),
            ],
            Language::DEFAULT,
        );

        let NetworkCommand::Connect(config) = command else {
            panic!("expected connect command");
        };

        assert_eq!(config.direction, SyncDirection::Push);
        assert!(config.delete);
        assert!(config.strict);
        assert!(config.preserve_permissions);
        assert_eq!(config.code.as_deref(), Some("123456"));
    }

    #[test]
    fn short_mode_values_are_accepted() {
        let command = NetworkCommand::parse_from(
            vec![
                OsString::from("fastsync"),
                OsString::from("share"),
                OsString::from("/tmp/share"),
                OsString::from("-m"),
                OsString::from("b"),
            ],
            Language::DEFAULT,
        );

        let NetworkCommand::Share(config) = command else {
            panic!("expected share command");
        };

        assert_eq!(config.mode, ShareMode::Both);
    }

    #[test]
    fn generated_pairing_code_is_six_digits() {
        let code = generate_pairing_code();

        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
    }

    #[test]
    fn pairing_code_validation_rejects_old_dash_format() {
        assert!(validate_pairing_code("123456").is_ok());
        assert!(validate_pairing_code("123-456").is_err());
        assert!(validate_pairing_code("12345").is_err());
        assert!(validate_pairing_code("abcdef").is_err());
    }

    #[test]
    fn network_summary_supports_chinese_labels() {
        let summary = NetworkSummary {
            side: NetworkSide::Connect,
            direction: SyncDirection::Push,
            directory: Path::new("/tmp/project"),
            remote: "127.0.0.1:7443",
            files: 2,
            bytes: 2048,
            deleted: 1,
            elapsed_ms: 1200,
        };

        let text = summary.to_text(Language::ZhCn);

        assert!(text.contains("网络同步完成"));
        assert!(text.contains("连接方"));
        assert!(text.contains("上传"));
        assert!(text.contains("2.0 KiB"));
    }

    #[test]
    fn request_files_skips_same_content_after_local_hash()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        std::fs::write(source.path().join("same.txt"), "same content")?;
        std::fs::write(target.path().join("same.txt"), "same content")?;
        let manifest = build_manifest(source.path())?;

        let requested = request_files_for_local_state(target.path(), &manifest, false)?;

        assert!(requested.is_empty());
        Ok(())
    }

    #[test]
    fn request_files_includes_missing_and_changed_files()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        std::fs::write(source.path().join("changed.txt"), "source")?;
        std::fs::write(source.path().join("missing.txt"), "missing")?;
        let changed_target = target.path().join("changed.txt");
        std::fs::write(&changed_target, "target")?;
        filetime::set_file_mtime(&changed_target, filetime::FileTime::from_unix_time(1, 0))?;
        let manifest = build_manifest(source.path())?;

        let requested = request_files_for_local_state(target.path(), &manifest, false)?;

        assert!(requested.contains(&PathBuf::from("changed.txt")));
        assert!(requested.contains(&PathBuf::from("missing.txt")));
        assert_eq!(requested.len(), 2);
        Ok(())
    }

    #[test]
    fn strict_request_files_hashes_even_when_metadata_matches()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        std::fs::write(source.path().join("same-meta.txt"), "aaaa")?;
        let manifest = build_manifest(source.path())?;
        let file = manifest
            .files
            .iter()
            .find(|file| file.path == Path::new("same-meta.txt"))
            .expect("manifest should contain source file");
        let target_file = target.path().join("same-meta.txt");
        std::fs::write(&target_file, "bbbb")?;
        if let Some(mtime) = file.metadata.modified_filetime() {
            filetime::set_file_mtime(&target_file, mtime)?;
        }
        #[cfg(unix)]
        if let Some(mode) = file.metadata.unix_mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target_file, std::fs::Permissions::from_mode(mode))?;
        }

        let fast_requested = request_files_for_local_state(target.path(), &manifest, false)?;
        let strict_requested = request_files_for_local_state(target.path(), &manifest, true)?;

        assert!(fast_requested.is_empty());
        assert_eq!(strict_requested, vec![PathBuf::from("same-meta.txt")]);
        Ok(())
    }

    #[test]
    fn delete_obsolete_removes_files_and_nested_directories()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let root = tempfile::tempdir()?;
        let stale_dir = root.path().join("stale");
        std::fs::create_dir(&stale_dir)?;
        std::fs::write(stale_dir.join("old.txt"), "old")?;
        std::fs::write(root.path().join("stale.txt"), "old")?;
        std::fs::write(root.path().join("keep.txt"), "keep")?;
        let manifest = Manifest {
            dirs: Vec::new(),
            files: vec![FileManifest {
                path: PathBuf::from("keep.txt"),
                len: 4,
                blake3: hex_digest(crate::hash::blake3_file(&root.path().join("keep.txt"))?),
                metadata: WireMetadata::from_entry(
                    crate::scan::scan_directory(root.path(), false)?
                        .get(Path::new("keep.txt"))
                        .expect("keep.txt should be scanned"),
                ),
            }],
        };

        let deleted = runtime.block_on(delete_obsolete(root.path(), &manifest))?;

        assert_eq!(deleted, 3);
        assert!(!stale_dir.exists());
        assert!(!root.path().join("stale.txt").exists());
        assert!(root.path().join("keep.txt").exists());
        Ok(())
    }

    #[test]
    fn skipped_network_file_still_receives_metadata()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        let source_file = source.path().join("same.txt");
        let target_file = target.path().join("same.txt");
        std::fs::write(&source_file, "same content")?;
        std::fs::write(&target_file, "same content")?;
        let source_time = filetime::FileTime::from_unix_time(123, 0);
        let target_time = filetime::FileTime::from_unix_time(456, 0);
        filetime::set_file_mtime(&source_file, source_time)?;
        filetime::set_file_mtime(&target_file, target_time)?;
        let manifest = build_manifest(source.path())?;
        let requested = request_files_for_local_state(target.path(), &manifest, false)?;

        apply_file_metadata(
            target.path(),
            &manifest.files,
            TransferOptions {
                delete: false,
                strict: false,
                preserve_times: true,
                preserve_permissions: false,
            },
        )?;

        let updated_time =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&target_file)?);
        assert!(requested.is_empty());
        assert_eq!(updated_time, source_time);
        Ok(())
    }

    #[test]
    fn network_file_path_rejects_existing_directory_without_delete()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let root = tempfile::tempdir()?;
        let path = root.path().join("item");
        std::fs::create_dir(&path)?;

        let error = runtime
            .block_on(ensure_file_path(&path, false))
            .expect_err("directory/file conflict should fail without delete");

        assert!(error.to_string().contains("exists and is a directory"));
        Ok(())
    }

    #[test]
    fn network_directory_path_replaces_file_when_delete_enabled()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let root = tempfile::tempdir()?;
        let path = root.path().join("item");
        std::fs::write(&path, "file")?;

        runtime.block_on(ensure_directory_path(&path, true))?;
        std::fs::create_dir(&path)?;

        assert!(path.is_dir());
        Ok(())
    }
}
