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
    pub verified_files: usize,
    pub bytes_copied: u64,
    pub errors: usize,
    pub duration_ms: u128,
}

impl SyncSummary {
    /// 渲染给终端用户阅读的简洁文本摘要。
    pub fn to_text(&self) -> String {
        format!(
            "\
fastsync summary
  source: {}
  target: {}
  dry_run: {}
  scanned: source={} target={}
  planned_operations: {}
  copied_files: {} ({} bytes)
  metadata_updates: {}
  created_dirs: {}
  deleted: files={} dirs={} symlinks={}
  verified_files: {}
  errors: {}
  duration_ms: {}
",
            self.source.display(),
            self.target.display(),
            self.dry_run,
            self.source_entries,
            self.target_entries,
            self.planned_operations,
            self.copied_files,
            self.bytes_copied,
            self.metadata_updates,
            self.created_dirs,
            self.deleted_files,
            self.deleted_dirs,
            self.deleted_symlinks,
            self.verified_files,
            self.errors,
            self.duration_ms
        )
    }
}
