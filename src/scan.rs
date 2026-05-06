use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::error::{FastSyncError, Result, io_context};
use crate::filter::PathFilter;
use crate::i18n::tr_path;

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

    /// 返回只包含过滤器允许条目的新快照。
    pub fn filtered(&self, filter: &PathFilter) -> Self {
        let entries = self
            .entries
            .iter()
            .filter(|(_, entry)| filter.allows_entry(&entry.relative_path, entry.is_dir()))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect();

        Self { entries }
    }
}

/// 扫描必定存在的源目录。
pub fn scan_directory(root: &Path, follow_symlinks: bool) -> Result<Snapshot> {
    if !root.is_dir() {
        return Err(FastSyncError::InvalidSource(root.to_path_buf()));
    }
    scan_existing_directory(root, follow_symlinks, &PathFilter::disabled())
}

/// 按过滤规则扫描必定存在的源目录。
pub fn scan_directory_filtered(
    root: &Path,
    follow_symlinks: bool,
    filter: &PathFilter,
) -> Result<Snapshot> {
    if !root.is_dir() {
        return Err(FastSyncError::InvalidSource(root.to_path_buf()));
    }
    scan_existing_directory(root, follow_symlinks, filter)
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
    scan_existing_directory(root, follow_symlinks, &PathFilter::disabled())
}

/// 按过滤规则扫描可不存在的目标目录。
pub fn scan_optional_directory_filtered(
    root: &Path,
    follow_symlinks: bool,
    filter: &PathFilter,
) -> Result<Snapshot> {
    if !root.exists() {
        return Ok(Snapshot::default());
    }
    if !root.is_dir() {
        return Err(FastSyncError::InvalidTarget(root.to_path_buf()));
    }
    scan_existing_directory(root, follow_symlinks, filter)
}

fn scan_existing_directory(
    root: &Path,
    follow_symlinks: bool,
    filter: &PathFilter,
) -> Result<Snapshot> {
    let mut snapshot = Snapshot::default();

    let iter = WalkDir::new(root)
        .follow_links(follow_symlinks)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let Ok(relative_path) = entry.path().strip_prefix(root) else {
                return false;
            };
            if entry.file_type().is_dir() {
                filter.should_descend(relative_path)
            } else {
                true
            }
        });

    for entry in iter {
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
                tr_path("io.read_metadata", absolute_path.display()),
                fs::metadata(&absolute_path),
            )?
        } else {
            io_context(
                tr_path("io.read_symlink_metadata", absolute_path.display()),
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

        if !filter.allows_entry(&relative_path, kind == EntryKind::Directory) {
            continue;
        }

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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn optional_missing_directory_returns_empty_snapshot()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let missing = root.path().join("missing");

        let snapshot = scan_optional_directory(&missing, false)?;

        assert!(snapshot.entries.is_empty());
        Ok(())
    }

    #[test]
    fn scan_directory_records_nested_relative_paths()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        fs::create_dir(root.path().join("nested"))?;
        fs::write(root.path().join("nested").join("a.txt"), "hello")?;

        let snapshot = scan_directory(root.path(), false)?;

        assert!(
            snapshot
                .get(Path::new("nested"))
                .is_some_and(FileEntry::is_dir)
        );
        assert!(
            snapshot
                .get(Path::new("nested/a.txt"))
                .is_some_and(FileEntry::is_file)
        );
        Ok(())
    }

    #[test]
    fn filtered_scan_prunes_excluded_directory_subtree()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        fs::create_dir(root.path().join("cache"))?;
        fs::write(root.path().join("cache").join("tmp.bin"), "tmp")?;
        fs::write(root.path().join("keep.txt"), "keep")?;
        let filter =
            crate::filter::PathFilter::from_rules(crate::filter::FilterMode::Exclude, "cache/\n")?;

        let snapshot = scan_directory_filtered(root.path(), false, &filter)?;

        assert!(snapshot.get(Path::new("keep.txt")).is_some());
        assert!(snapshot.get(Path::new("cache")).is_none());
        assert!(snapshot.get(Path::new("cache/tmp.bin")).is_none());
        Ok(())
    }

    #[test]
    fn filtered_scan_descends_through_unmatched_include_ancestors()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        fs::create_dir(root.path().join("src"))?;
        fs::create_dir(root.path().join("src").join("bin"))?;
        fs::write(root.path().join("src").join("bin").join("main.rs"), "main")?;
        fs::write(root.path().join("src").join("bin").join("main.txt"), "main")?;
        let filter = crate::filter::PathFilter::from_rules(
            crate::filter::FilterMode::Include,
            "src/**/*.rs\n",
        )?;

        let snapshot = scan_directory_filtered(root.path(), false, &filter)?;

        assert!(snapshot.get(Path::new("src")).is_none());
        assert!(snapshot.get(Path::new("src/bin")).is_none());
        assert!(snapshot.get(Path::new("src/bin/main.rs")).is_some());
        assert!(snapshot.get(Path::new("src/bin/main.txt")).is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scan_preserves_symlink_kind_when_not_following()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        fs::write(root.path().join("target.txt"), "hello")?;
        std::os::unix::fs::symlink("target.txt", root.path().join("link.txt"))?;

        let snapshot = scan_directory(root.path(), false)?;

        let link = snapshot.get(Path::new("link.txt")).expect("link entry");
        assert_eq!(link.kind, EntryKind::Symlink);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scan_follows_symlink_when_enabled() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        fs::write(root.path().join("target.txt"), "hello")?;
        std::os::unix::fs::symlink("target.txt", root.path().join("link.txt"))?;

        let snapshot = scan_directory(root.path(), true)?;

        let link = snapshot.get(Path::new("link.txt")).expect("link entry");
        assert_eq!(link.kind, EntryKind::File);
        Ok(())
    }
}
