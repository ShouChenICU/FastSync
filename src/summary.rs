use std::path::PathBuf;

use serde::Serialize;

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
        let status = if self.errors == 0 {
            if self.dry_run {
                "预演完成"
            } else {
                "同步完成"
            }
        } else {
            "同步完成，但存在错误"
        };
        let status = if self.errors == 0 {
            paint(color, "1;32", status)
        } else {
            paint(color, "1;31", status)
        };
        let mode = if self.dry_run {
            "dry-run，仅生成计划"
        } else {
            "已执行"
        };
        let copy_label = if self.dry_run {
            "预计复制"
        } else {
            "已复制"
        };
        let delete_label = if self.dry_run {
            "预计删除"
        } else {
            "已删除"
        };
        let errors = if self.errors == 0 {
            paint(color, "32", "0")
        } else {
            paint(color, "31", &self.errors.to_string())
        };

        format!(
            "\
fastsync {status}

路径
  源目录    {}
  目标目录  {}

扫描与计划
  运行模式  {}
  源端条目  {}
  目标条目  {}
  计划操作  {}
  计划数据  {}

执行结果
  {copy_label}  {} 个文件 ({})
  元数据    {} 项
  新建目录  {} 个
  {delete_label}  {} 个文件, {} 个目录, {} 个链接
  BLAKE3比较 {} 个文件
  BLAKE3校验 {} 个文件

状态
  错误      {}
  耗时      {}
",
            self.source.display(),
            self.target.display(),
            mode,
            self.source_entries,
            self.target_entries,
            self.planned_operations,
            human_bytes(self.bytes_planned),
            self.copied_files,
            human_bytes(self.bytes_copied),
            self.metadata_updates,
            self.created_dirs,
            self.deleted_files,
            self.deleted_dirs,
            self.deleted_symlinks,
            self.blake3_compared_files,
            self.verified_files,
            errors,
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
