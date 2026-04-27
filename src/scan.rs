use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::error::{FastSyncError, Result, io_context};

/// 扫描到的文件系统对象类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
        }
    }
}

/// 单个目录项的稳定描述。
///
/// `relative_path` 是后续比较和执行阶段的主键；`absolute_path` 只用于实际 I/O。
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub kind: EntryKind,
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub readonly: bool,
    #[cfg(unix)]
    pub mode: u32,
}

impl FileEntry {
    /// 判断当前条目是否为普通文件。
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }

    /// 判断当前条目是否为目录。
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// 一个目录树的扫描快照。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub entries: BTreeMap<PathBuf, FileEntry>,
}

impl Snapshot {
    /// 按相对路径获取条目。
    pub fn get(&self, path: &Path) -> Option<&FileEntry> {
        self.entries.get(path)
    }
}

/// 扫描必定存在的源目录。
pub fn scan_directory(root: &Path, follow_symlinks: bool) -> Result<Snapshot> {
    if !root.is_dir() {
        return Err(FastSyncError::InvalidSource(root.to_path_buf()));
    }
    scan_existing_directory(root, follow_symlinks)
}

/// 扫描可不存在的目标目录。
///
/// 目标目录不存在时返回空快照，执行阶段会按同步计划创建目录。
pub fn scan_optional_directory(root: &Path, follow_symlinks: bool) -> Result<Snapshot> {
    if !root.exists() {
        return Ok(Snapshot::default());
    }
    if !root.is_dir() {
        return Err(FastSyncError::InvalidTarget(root.to_path_buf()));
    }
    scan_existing_directory(root, follow_symlinks)
}

fn scan_existing_directory(root: &Path, follow_symlinks: bool) -> Result<Snapshot> {
    let mut snapshot = Snapshot::default();

    for entry in WalkDir::new(root)
        .follow_links(follow_symlinks)
        .min_depth(1)
        .into_iter()
    {
        let entry = entry?;
        let absolute_path = entry.path().to_path_buf();
        let relative_path = absolute_path
            .strip_prefix(root)
            .map_err(|_| FastSyncError::PathOutsideRoot {
                path: absolute_path.clone(),
            })?
            .to_path_buf();
        let metadata = if follow_symlinks {
            io_context(
                format!("读取元数据失败: {}", absolute_path.display()),
                fs::metadata(&absolute_path),
            )?
        } else {
            io_context(
                format!("读取符号链接元数据失败: {}", absolute_path.display()),
                fs::symlink_metadata(&absolute_path),
            )?
        };
        let file_type = metadata.file_type();
        let kind = if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else {
            return Err(FastSyncError::UnsupportedEntry(relative_path));
        };

        let modified = metadata.modified().ok();
        let readonly = metadata.permissions().readonly();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };

        snapshot.entries.insert(
            relative_path.clone(),
            FileEntry {
                relative_path,
                absolute_path,
                kind,
                len: metadata.len(),
                modified,
                readonly,
                #[cfg(unix)]
                mode,
            },
        );
    }

    Ok(snapshot)
}
