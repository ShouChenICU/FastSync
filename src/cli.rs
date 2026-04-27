use std::path::PathBuf;

use clap::{ArgAction, Parser};

use crate::config::{CompareMode, HashAlgorithm, LogLevel, OutputMode, PreserveMode, VerifyMode};

/// fastsync 命令行参数。
///
/// 参数按照同步语义、比较策略、性能控制和输出行为分组，默认值优先保证安全。
#[derive(Debug, Clone, Parser)]
#[command(author, version, about = "快速、可靠的单向目录同步工具")]
pub struct Cli {
    /// 源目录。
    pub source: PathBuf,

    /// 目标目录。
    pub target: PathBuf,

    /// 只生成计划与摘要，不实际修改目标目录。
    #[arg(long)]
    pub dry_run: bool,

    /// 删除目标端源目录中不存在的多余项。默认关闭，避免误删。
    #[arg(long)]
    pub delete: bool,

    /// 遍历时跟随符号链接。默认关闭。
    #[arg(long)]
    pub follow_symlinks: bool,

    /// 文件比较策略。
    #[arg(long, value_enum, default_value_t = CompareMode::Auto)]
    pub compare: CompareMode,

    /// 内容校验哈希算法。当前 MVP 支持 BLAKE3。
    #[arg(long, value_enum, default_value_t = HashAlgorithm::Blake3)]
    pub hash: HashAlgorithm,

    /// 复制后的校验强度。
    #[arg(long, value_enum, default_value_t = VerifyMode::Changed)]
    pub verify: VerifyMode,

    /// 是否保留修改时间。
    #[arg(long, value_enum, default_value_t = PreserveMode::Auto)]
    pub preserve_times: PreserveMode,

    /// 是否保留基础权限位。
    #[arg(long, value_enum, default_value_t = PreserveMode::Auto)]
    pub preserve_permissions: PreserveMode,

    /// 禁用临时文件 + 重命名写入目标文件。
    #[arg(long = "no-atomic-write", default_value_t = true, action = ArgAction::SetFalse)]
    pub atomic_write: bool,

    /// worker 线程数，可传数字或 auto。
    #[arg(long, default_value = "auto")]
    pub threads: Option<String>,

    /// 有界任务队列长度，默认 threads * 4。
    #[arg(long)]
    pub queue_size: Option<usize>,

    /// 最大允许错误数，达到阈值后中止。
    #[arg(long, default_value_t = 100)]
    pub max_errors: usize,

    /// 首个错误后立即停止。
    #[arg(long)]
    pub stop_on_error: bool,

    /// 日志级别。
    #[arg(long, value_enum, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,

    /// 摘要输出格式。
    #[arg(long, value_enum, default_value_t = OutputMode::Text)]
    pub output: OutputMode,
}

impl Cli {
    /// 测试辅助构造器，避免单元测试依赖命令行字符串解析。
    #[cfg(test)]
    pub fn for_test(source: &std::path::Path, target: &std::path::Path) -> Self {
        Self {
            source: source.to_path_buf(),
            target: target.to_path_buf(),
            dry_run: false,
            delete: false,
            follow_symlinks: false,
            compare: CompareMode::Auto,
            hash: HashAlgorithm::Blake3,
            verify: VerifyMode::Changed,
            preserve_times: PreserveMode::Auto,
            preserve_permissions: PreserveMode::Auto,
            atomic_write: true,
            threads: Some("auto".to_string()),
            queue_size: None,
            max_errors: 100,
            stop_on_error: false,
            log_level: LogLevel::Info,
            output: OutputMode::Text,
        }
    }
}
