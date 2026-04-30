use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::i18n::{tr_current, tr_many_errors, tr_path, tr_path_source_target, tr_value};

/// fastsync 统一错误类型。
///
/// 所有底层 I/O、遍历和同步语义错误都应转换为该类型，避免在核心链路中
/// 使用 `unwrap` 或丢失上下文。
pub type Result<T> = std::result::Result<T, FastSyncError>;

#[derive(Debug)]
pub enum FastSyncError {
    Io {
        context: String,
        source: std::io::Error,
    },

    WalkDir(walkdir::Error),

    InvalidSource(PathBuf),

    InvalidTarget(PathBuf),

    PathTypeConflict {
        relative_path: PathBuf,
        source_kind: &'static str,
        target_kind: &'static str,
    },

    PathOutsideRoot {
        path: PathBuf,
    },

    Many {
        count: usize,
        first: String,
    },

    VerificationFailed(PathBuf),

    UnsupportedEntry(PathBuf),
}

impl fmt::Display for FastSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { context, source } => write!(formatter, "{context}: {source}"),
            Self::WalkDir(error) => write!(formatter, "{}", tr_value("error.walk_dir", error)),
            Self::InvalidSource(path) => {
                write!(
                    formatter,
                    "{}",
                    tr_path("error.invalid_source", path.display())
                )
            }
            Self::InvalidTarget(path) => {
                write!(
                    formatter,
                    "{}",
                    tr_path("error.invalid_target", path.display())
                )
            }
            Self::PathTypeConflict {
                relative_path,
                source_kind,
                target_kind,
            } => write!(
                formatter,
                "{}",
                tr_path_source_target(
                    "error.path_type_conflict",
                    relative_path.display(),
                    entry_kind_label(source_kind),
                    entry_kind_label(target_kind)
                )
            ),
            Self::PathOutsideRoot { path } => {
                write!(
                    formatter,
                    "{}",
                    tr_path("error.path_outside_root", path.display())
                )
            }
            Self::Many { count, first } => {
                write!(formatter, "{}", tr_many_errors(*count, first))
            }
            Self::VerificationFailed(path) => {
                write!(
                    formatter,
                    "{}",
                    tr_path("error.verification_failed", path.display())
                )
            }
            Self::UnsupportedEntry(path) => {
                write!(
                    formatter,
                    "{}",
                    tr_path("error.unsupported_entry", path.display())
                )
            }
        }
    }
}

impl Error for FastSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::WalkDir(error) => Some(error),
            _ => None,
        }
    }
}

impl From<walkdir::Error> for FastSyncError {
    fn from(error: walkdir::Error) -> Self {
        Self::WalkDir(error)
    }
}

/// 为 I/O 错误补充当前操作语义，便于用户定位失败阶段和路径。
pub fn io_context<T>(context: impl Into<String>, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|source| FastSyncError::Io {
        context: context.into(),
        source,
    })
}

fn entry_kind_label(kind: &str) -> String {
    match kind {
        "file" => tr_current("error.entry_kind.file"),
        "directory" => tr_current("error.entry_kind.directory"),
        "symlink" => tr_current("error.entry_kind.symlink"),
        _ => kind.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::path::PathBuf;

    use crate::i18n::{Language, set_language};

    use super::*;

    #[test]
    fn display_formats_path_type_conflict_with_localized_kinds() {
        set_language(Language::En);
        let error = FastSyncError::PathTypeConflict {
            relative_path: PathBuf::from("item"),
            source_kind: "file",
            target_kind: "directory",
        };

        let text = error.to_string();

        assert!(text.contains("item"));
        assert!(text.contains("file") || text.contains("文件"));
        assert!(text.contains("directory") || text.contains("目录"));
    }

    #[test]
    fn display_formats_many_and_verification_errors() {
        set_language(Language::En);
        let many = FastSyncError::Many {
            count: 2,
            first: "copy failed".to_string(),
        };
        let verification = FastSyncError::VerificationFailed(PathBuf::from("a.txt"));

        assert!(many.to_string().contains("2 errors occurred"));
        assert!(many.to_string().contains("copy failed"));
        assert!(
            verification
                .to_string()
                .contains("post-copy verification failed")
        );
        assert!(verification.to_string().contains("a.txt"));
    }

    #[test]
    fn io_error_exposes_source_error() {
        let error = io_context(
            "open file",
            std::fs::File::open("/path/that/should/not/exist"),
        )
        .expect_err("missing file should fail");

        assert!(error.to_string().contains("open file"));
        assert!(error.source().is_some());
    }
}
