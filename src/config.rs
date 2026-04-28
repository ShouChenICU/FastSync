use std::path::PathBuf;

use clap::{ValueEnum, builder::PossibleValue};

use crate::cli::Cli;
use crate::error::{FastSyncError, Result};
use crate::i18n::{tr_current, tr_value};

/// 文件内容比较策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareMode {
    /// 元数据一致时信任元数据；元数据不一致但大小一致时再使用 BLAKE3 确认内容。
    Fast,
    /// 大小一致时始终使用 BLAKE3 确认内容，即使元数据一致。
    Strict,
}

/// 复制后验证强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// 不做复制后校验。
    None,
    /// 只校验发生复制或覆盖的文件。
    Changed,
    /// 校验源目录中所有普通文件。
    All,
}

impl VerifyMode {
    /// 判断是否需要校验复制或覆盖过的文件。
    pub fn verify_changed_files(self) -> bool {
        match self {
            Self::Changed | Self::All => true,
            Self::None => false,
        }
    }

    /// 判断是否需要在同步后全量校验源目录普通文件。
    pub fn verify_all_files(self) -> bool {
        match self {
            Self::All => true,
            Self::None | Self::Changed => false,
        }
    }
}

/// 元数据保留策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreserveMode {
    /// 按平台能力自动保留。
    Auto,
    /// 强制保留。
    True,
    /// 不保留。
    False,
}

impl PreserveMode {
    /// `auto` 采用“尽力保留”的策略，失败会返回错误而不是静默忽略。
    pub fn enabled(self) -> bool {
        match self {
            Self::Auto | Self::True => true,
            Self::False => false,
        }
    }
}

/// 当前实现支持的哈希算法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    /// BLAKE3，默认强校验算法。
    Blake3,
}

/// 日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// 终端/机器输出模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Text,
    Json,
}

/// 完整运行配置。
#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub source: PathBuf,
    pub target: PathBuf,
    pub dry_run: bool,
    pub delete: bool,
    pub follow_symlinks: bool,
    pub compare_mode: CompareMode,
    pub hash_algorithm: HashAlgorithm,
    pub verify_mode: VerifyMode,
    pub sync_metadata: bool,
    pub preserve_times: PreserveMode,
    pub preserve_permissions: PreserveMode,
    pub atomic_write: bool,
    pub threads: usize,
    pub queue_size: usize,
    pub max_errors: usize,
    pub stop_on_error: bool,
    pub output: OutputMode,
    pub log_level: LogLevel,
}

impl SyncConfig {
    /// 判断是否需要为同名文件生成独立的元数据同步任务。
    pub fn syncs_file_metadata(&self) -> bool {
        self.sync_metadata && (self.preserve_times.enabled() || self.preserve_permissions.enabled())
    }
}

impl TryFrom<Cli> for SyncConfig {
    type Error = FastSyncError;

    /// 将 CLI 参数规范化为核心配置，并补齐自动默认值。
    fn try_from(cli: Cli) -> Result<Self> {
        if !cli.source.is_dir() {
            return Err(FastSyncError::InvalidSource(cli.source));
        }

        let threads = match cli.threads.as_deref() {
            #[allow(non_snake_case)]
            None | Some("auto") => default_threads(),
            Some(raw) => raw.parse::<usize>().map_err(|err| FastSyncError::Io {
                context: tr_value("io.parse_threads", raw),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, err),
            })?,
        }
        .max(1);

        let queue_size = cli.queue_size.unwrap_or_else(|| threads * 4).max(1);
        let compare_mode = if cli.strict {
            CompareMode::Strict
        } else {
            cli.compare
        };

        Ok(Self {
            source: cli.source,
            target: cli.target,
            dry_run: cli.dry_run,
            delete: cli.delete,
            follow_symlinks: cli.follow_symlinks,
            compare_mode,
            hash_algorithm: cli.hash,
            verify_mode: cli.verify,
            sync_metadata: cli.sync_metadata,
            preserve_times: cli.preserve_times,
            preserve_permissions: cli.preserve_permissions,
            atomic_write: cli.atomic_write,
            threads,
            queue_size,
            max_errors: cli.max_errors,
            stop_on_error: cli.stop_on_error,
            output: cli.output,
            log_level: cli.log_level,
        })
    }
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .clamp(1, 8)
}

const COMPARE_MODE_VARIANTS: [CompareMode; 2] = [CompareMode::Fast, CompareMode::Strict];
const VERIFY_MODE_VARIANTS: [VerifyMode; 3] =
    [VerifyMode::None, VerifyMode::Changed, VerifyMode::All];
const PRESERVE_MODE_VARIANTS: [PreserveMode; 3] =
    [PreserveMode::Auto, PreserveMode::True, PreserveMode::False];
const HASH_ALGORITHM_VARIANTS: [HashAlgorithm; 1] = [HashAlgorithm::Blake3];
const LOG_LEVEL_VARIANTS: [LogLevel; 5] = [
    LogLevel::Error,
    LogLevel::Warn,
    LogLevel::Info,
    LogLevel::Debug,
    LogLevel::Trace,
];
const OUTPUT_MODE_VARIANTS: [OutputMode; 2] = [OutputMode::Text, OutputMode::Json];

impl ValueEnum for CompareMode {
    fn value_variants<'a>() -> &'a [Self] {
        &COMPARE_MODE_VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::Fast => Some(PossibleValue::new("fast").help(tr_current("value.compare.fast"))),
            Self::Strict => {
                Some(PossibleValue::new("strict").help(tr_current("value.compare.strict")))
            }
        }
    }
}

impl ValueEnum for VerifyMode {
    fn value_variants<'a>() -> &'a [Self] {
        &VERIFY_MODE_VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::None => Some(PossibleValue::new("none").help(tr_current("value.verify.none"))),
            Self::Changed => {
                Some(PossibleValue::new("changed").help(tr_current("value.verify.changed")))
            }
            Self::All => Some(PossibleValue::new("all").help(tr_current("value.verify.all"))),
        }
    }
}

impl ValueEnum for PreserveMode {
    fn value_variants<'a>() -> &'a [Self] {
        &PRESERVE_MODE_VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::Auto => Some(PossibleValue::new("auto").help(tr_current("value.preserve.auto"))),
            Self::True => Some(PossibleValue::new("true").help(tr_current("value.preserve.true"))),
            Self::False => {
                Some(PossibleValue::new("false").help(tr_current("value.preserve.false")))
            }
        }
    }
}

impl ValueEnum for HashAlgorithm {
    fn value_variants<'a>() -> &'a [Self] {
        &HASH_ALGORITHM_VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::Blake3 => {
                Some(PossibleValue::new("blake3").help(tr_current("value.hash.blake3")))
            }
        }
    }
}

impl ValueEnum for LogLevel {
    fn value_variants<'a>() -> &'a [Self] {
        &LOG_LEVEL_VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::Error => Some(PossibleValue::new("error")),
            Self::Warn => Some(PossibleValue::new("warn")),
            Self::Info => Some(PossibleValue::new("info")),
            Self::Debug => Some(PossibleValue::new("debug")),
            Self::Trace => Some(PossibleValue::new("trace")),
        }
    }
}

impl ValueEnum for OutputMode {
    fn value_variants<'a>() -> &'a [Self] {
        &OUTPUT_MODE_VARIANTS
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::Text => Some(PossibleValue::new("text")),
            Self::Json => Some(PossibleValue::new("json")),
        }
    }
}
