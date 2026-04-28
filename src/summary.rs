use std::path::PathBuf;

use serde::Serialize;

use crate::i18n::{Language, current_language, tr};

/// 一次同步运行的稳定摘要。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncSummary {
    pub source: PathBuf,
    pub target: PathBuf,
    pub dry_run: bool,
    pub source_entries: usize,
    pub target_entries: usize,
    pub planned_operations: usize,
    pub bytes_planned: u64,
    pub copied_files: usize,
    pub metadata_updates: usize,
    pub created_dirs: usize,
    pub deleted_files: usize,
    pub deleted_dirs: usize,
    pub deleted_symlinks: usize,
    pub blake3_compared_files: usize,
    pub verified_files: usize,
    pub bytes_copied: u64,
    pub errors: usize,
    pub duration_ms: u128,
}

impl SyncSummary {
    /// 渲染给终端用户阅读的简洁文本摘要。
    pub fn to_text(&self) -> String {
        self.to_text_with_color(false)
    }

    /// 渲染给终端用户阅读的简洁文本摘要，可按需启用 ANSI 颜色。
    pub fn to_text_with_color(&self, color: bool) -> String {
        self.to_text_with_language(current_language(), color)
    }

    /// 按指定语言渲染给终端用户阅读的简洁文本摘要。
    pub fn to_text_with_language(&self, language: Language, color: bool) -> String {
        let status = if self.errors == 0 {
            if self.dry_run {
                tr(language, "summary.status.dry_run_done")
            } else {
                tr(language, "summary.status.done")
            }
        } else {
            tr(language, "summary.status.done_with_errors")
        };
        let status = if self.errors == 0 {
            paint(color, "1;32", &status)
        } else {
            paint(color, "1;31", &status)
        };
        let mode = if self.dry_run {
            tr(language, "summary.mode.dry_run")
        } else {
            tr(language, "summary.mode.executed")
        };
        let copy_label = if self.dry_run {
            tr(language, "summary.copy.planned")
        } else {
            tr(language, "summary.copy.done")
        };
        let delete_label = if self.dry_run {
            tr(language, "summary.delete.planned")
        } else {
            tr(language, "summary.delete.done")
        };
        let errors = if self.errors == 0 {
            paint(color, "32", "0")
        } else {
            paint(color, "31", &self.errors.to_string())
        };

        format!(
            "\
fastsync {status}

{}
  {}  {}
  {}  {}

{}
  {}  {}
  {}  {}
  {}  {}
  {}  {}
  {}  {}

{}
  {copy_label}  {} {} ({})
  {}  {} {}
  {}  {} {}
  {delete_label}  {} {}, {} {}, {} {}
  {}  {} {}
  {}  {} {}

{}
  {}  {}
  {}  {}
",
            tr(language, "summary.paths"),
            tr(language, "summary.source"),
            self.source.display(),
            tr(language, "summary.target"),
            self.target.display(),
            tr(language, "summary.scan_plan"),
            tr(language, "summary.run_mode"),
            mode,
            tr(language, "summary.source_entries"),
            self.source_entries,
            tr(language, "summary.target_entries"),
            self.target_entries,
            tr(language, "summary.planned_operations"),
            self.planned_operations,
            tr(language, "summary.planned_data"),
            human_bytes(self.bytes_planned),
            tr(language, "summary.execution"),
            self.copied_files,
            tr(language, "summary.files"),
            human_bytes(self.bytes_copied),
            tr(language, "summary.metadata"),
            self.metadata_updates,
            tr(language, "summary.items"),
            tr(language, "summary.created_dirs"),
            self.created_dirs,
            tr(language, "summary.dirs"),
            self.deleted_files,
            tr(language, "summary.files"),
            self.deleted_dirs,
            tr(language, "summary.dirs"),
            self.deleted_symlinks,
            tr(language, "summary.links"),
            tr(language, "summary.blake3_compared"),
            self.blake3_compared_files,
            tr(language, "summary.files"),
            tr(language, "summary.blake3_verified"),
            self.verified_files,
            tr(language, "summary.files"),
            tr(language, "summary.status"),
            tr(language, "summary.errors"),
            errors,
            tr(language, "summary.duration"),
            human_duration(self.duration_ms)
        )
    }
}

fn paint(enabled: bool, code: &str, text: &str) -> String {
    if enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];

    for candidate in UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = candidate;
    }

    if unit == "B" {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

fn human_duration(duration_ms: u128) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms} ms")
    } else {
        format!("{:.2} s", duration_ms as f64 / 1_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_summary_defaults_to_english_labels() {
        let summary = SyncSummary {
            copied_files: 1,
            bytes_copied: 5,
            ..SyncSummary::default()
        };

        let text = summary.to_text_with_language(Language::En, false);

        assert!(text.contains("Sync complete"));
        assert!(text.contains("Paths"));
        assert!(text.contains("Files copied"));
    }

    #[test]
    fn text_summary_supports_chinese_labels() {
        let summary = SyncSummary {
            copied_files: 1,
            bytes_copied: 5,
            ..SyncSummary::default()
        };

        let text = summary.to_text_with_language(Language::ZhCn, false);

        assert!(text.contains("同步完成"));
        assert!(text.contains("路径"));
        assert!(text.contains("已复制"));
    }
}
