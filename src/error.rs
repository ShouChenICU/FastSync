use std::path::PathBuf;

/// fastsync 统一错误类型。
///
/// 所有底层 I/O、遍历和同步语义错误都应转换为该类型，避免在核心链路中
/// 使用 `unwrap` 或丢失上下文。
pub type Result<T> = std::result::Result<T, FastSyncError>;

#[derive(Debug, thiserror::Error)]
pub enum FastSyncError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("遍历目录失败: {0}")]
    WalkDir(#[from] walkdir::Error),

    #[error("源目录不存在或不是目录: {0}")]
    InvalidSource(PathBuf),

    #[error("目标路径已存在但不是目录: {0}")]
    InvalidTarget(PathBuf),

    #[error("路径类型冲突: {relative_path}，源类型为 {source_kind}，目标类型为 {target_kind}")]
    PathTypeConflict {
        relative_path: PathBuf,
        source_kind: &'static str,
        target_kind: &'static str,
    },

    #[error("路径不在扫描根目录内: {path}")]
    PathOutsideRoot { path: PathBuf },

    #[error("执行过程中发生 {count} 个错误，首个错误: {first}")]
    Many { count: usize, first: String },

    #[error("复制后校验失败: {0}")]
    VerificationFailed(PathBuf),

    #[error("不支持的同步对象类型: {0}")]
    UnsupportedEntry(PathBuf),
}

/// 为 I/O 错误补充当前操作语义，便于用户定位失败阶段和路径。
pub fn io_context<T>(context: impl Into<String>, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|source| FastSyncError::Io {
        context: context.into(),
        source,
    })
}
