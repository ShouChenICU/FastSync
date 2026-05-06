use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::SyncConfig;
use crate::error::{FastSyncError, Result, io_context};
use crate::filter::PathFilter;
use crate::hash::{Blake3Digest, blake3_reader};
use crate::i18n::{tr_path, tr_source_target};
use crate::scan::{
    FileEntry, Snapshot, scan_directory, scan_directory_filtered, scan_optional_directory,
    scan_optional_directory_filtered,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 同步端点抽象。
///
/// 核心同步编排只依赖该 trait 表达的语义：扫描、内容比较、写入提交、
/// 元数据同步和删除。远端 agent 后续可以实现同一接口，并把操作转成网络请求。
pub trait SyncEndpoint: Send + Sync {
    /// 返回端点根目录或等价的显示路径。
    fn root(&self) -> &Path;

    /// 扫描必须存在的目录，通常用于源端。
    fn scan_required(&self, follow_symlinks: bool) -> Result<Snapshot>;

    /// 按过滤规则扫描必须存在的目录。
    fn scan_required_filtered(
        &self,
        follow_symlinks: bool,
        filter: &PathFilter,
    ) -> Result<Snapshot> {
        Ok(self.scan_required(follow_symlinks)?.filtered(filter))
    }

    /// 扫描可以不存在的目录，通常用于目标端。
    fn scan_optional(&self, follow_symlinks: bool) -> Result<Snapshot>;

    /// 按过滤规则扫描可以不存在的目录。
    fn scan_optional_filtered(
        &self,
        follow_symlinks: bool,
        filter: &PathFilter,
    ) -> Result<Snapshot> {
        Ok(self.scan_optional(follow_symlinks)?.filtered(filter))
    }

    /// 确保端点根目录存在。
    fn ensure_root(&self) -> Result<()>;

    /// 创建相对路径对应的目录。
    fn create_directory(&self, relative_path: &Path) -> Result<()>;

    /// 删除相对路径对应的普通文件。
    fn delete_file(&self, relative_path: &Path) -> Result<()>;

    /// 删除相对路径对应的空目录。
    fn delete_directory(&self, relative_path: &Path) -> Result<()>;

    /// 删除相对路径对应的符号链接。
    fn delete_symlink(&self, relative_path: &Path) -> Result<()>;

    /// 打开相对路径对应的文件流。
    fn open_read(&self, relative_path: &Path) -> Result<Box<dyn Read + Send>>;

    /// 读取相对路径对应文件的基础元数据。
    fn file_metadata(&self, relative_path: &Path) -> Result<EndpointMetadata>;

    /// 将源端文件复制到当前端点。
    fn copy_file_from(
        &self,
        source: &dyn SyncEndpoint,
        relative_path: &Path,
        atomic_write: bool,
    ) -> Result<()>;

    /// 将源端文件的基础元数据同步到当前端点。
    fn apply_file_metadata_from(
        &self,
        source: &dyn SyncEndpoint,
        relative_path: &Path,
        config: &SyncConfig,
    ) -> Result<()>;

    /// 计算相对路径对应文件的 BLAKE3 摘要。
    fn blake3_file(&self, relative_path: &Path) -> Result<Blake3Digest> {
        let reader = self.open_read(relative_path)?;
        blake3_reader(relative_path, reader)
    }

    /// 判断当前端点文件与另一个端点文件的 BLAKE3 内容是否一致。
    fn same_file_content(
        &self,
        left_relative_path: &Path,
        other: &dyn SyncEndpoint,
        right_relative_path: &Path,
    ) -> Result<bool> {
        Ok(self.blake3_file(left_relative_path)? == other.blake3_file(right_relative_path)?)
    }
}

/// 端点之间可迁移的基础文件元数据。
pub struct EndpointMetadata {
    permissions: fs::Permissions,
    atime: filetime::FileTime,
    mtime: filetime::FileTime,
}

/// 本地文件系统同步端点。
///
/// 端点负责把同步阶段中的相对路径限制在自身 root 内，并封装扫描、哈希、
/// 写入、删除和元数据设置等 I/O 操作。后续远端端点应保持相同语义。
#[derive(Debug, Clone)]
pub struct LocalEndpoint {
    root: PathBuf,
}

impl LocalEndpoint {
    /// 创建以 `root` 为边界的本地端点。
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 将相对路径解析到 root 内，拒绝绝对路径和父目录逃逸。
    pub fn resolve(&self, relative_path: &Path) -> Result<PathBuf> {
        if relative_path.is_absolute()
            || relative_path.components().any(|component| {
                matches!(
                    component,
                    Component::Prefix(_) | Component::RootDir | Component::ParentDir
                )
            })
        {
            return Err(FastSyncError::PathOutsideRoot {
                path: relative_path.to_path_buf(),
            });
        }

        Ok(self.root.join(relative_path))
    }
}

impl SyncEndpoint for LocalEndpoint {
    fn root(&self) -> &Path {
        &self.root
    }

    fn scan_required(&self, follow_symlinks: bool) -> Result<Snapshot> {
        scan_directory(&self.root, follow_symlinks)
    }

    fn scan_required_filtered(
        &self,
        follow_symlinks: bool,
        filter: &PathFilter,
    ) -> Result<Snapshot> {
        scan_directory_filtered(&self.root, follow_symlinks, filter)
    }

    fn scan_optional(&self, follow_symlinks: bool) -> Result<Snapshot> {
        scan_optional_directory(&self.root, follow_symlinks)
    }

    fn scan_optional_filtered(
        &self,
        follow_symlinks: bool,
        filter: &PathFilter,
    ) -> Result<Snapshot> {
        scan_optional_directory_filtered(&self.root, follow_symlinks, filter)
    }

    fn ensure_root(&self) -> Result<()> {
        io_context(
            tr_path("io.create_target_root", self.root.display()),
            fs::create_dir_all(&self.root),
        )
    }

    fn create_directory(&self, relative_path: &Path) -> Result<()> {
        let path = self.resolve(relative_path)?;
        io_context(
            tr_path("io.create_directory", path.display()),
            fs::create_dir_all(path),
        )
    }

    fn delete_file(&self, relative_path: &Path) -> Result<()> {
        let path = self.resolve(relative_path)?;
        io_context(
            tr_path("io.delete_file", path.display()),
            fs::remove_file(path),
        )
    }

    fn delete_directory(&self, relative_path: &Path) -> Result<()> {
        let path = self.resolve(relative_path)?;
        io_context(
            tr_path("io.delete_directory", path.display()),
            fs::remove_dir(path),
        )
    }

    fn delete_symlink(&self, relative_path: &Path) -> Result<()> {
        let path = self.resolve(relative_path)?;
        io_context(
            tr_path("io.delete_symlink", path.display()),
            fs::remove_file(path),
        )
    }

    fn open_read(&self, relative_path: &Path) -> Result<Box<dyn Read + Send>> {
        let path = self.resolve(relative_path)?;
        let file = io_context(
            tr_path("io.open_source_file", path.display()),
            File::open(path),
        )?;
        Ok(Box::new(file))
    }

    fn file_metadata(&self, relative_path: &Path) -> Result<EndpointMetadata> {
        let path = self.resolve(relative_path)?;
        let metadata = io_context(
            tr_path("io.read_source_metadata", path.display()),
            fs::metadata(&path),
        )?;
        Ok(EndpointMetadata {
            permissions: metadata.permissions(),
            atime: filetime::FileTime::from_last_access_time(&metadata),
            mtime: filetime::FileTime::from_last_modification_time(&metadata),
        })
    }

    fn copy_file_from(
        &self,
        source: &dyn SyncEndpoint,
        relative_path: &Path,
        atomic_write: bool,
    ) -> Result<()> {
        let target_path = self.resolve(relative_path)?;
        if atomic_write {
            copy_file_atomic(source, relative_path, &target_path)
        } else {
            copy_file_direct(source, relative_path, &target_path)
        }
    }

    fn apply_file_metadata_from(
        &self,
        source: &dyn SyncEndpoint,
        relative_path: &Path,
        config: &SyncConfig,
    ) -> Result<()> {
        let target_path = self.resolve(relative_path)?;
        let metadata = source.file_metadata(relative_path)?;

        if config.preserve_permissions.enabled() {
            io_context(
                tr_path("io.set_target_permissions", target_path.display()),
                fs::set_permissions(&target_path, metadata.permissions),
            )?;
        }

        if config.preserve_times.enabled() {
            io_context(
                tr_path("io.set_target_times", target_path.display()),
                filetime::set_file_times(&target_path, metadata.atime, metadata.mtime),
            )?;
        }

        Ok(())
    }
}

/// 一次同步运行使用的源端点和目标端点。
#[derive(Clone)]
pub struct SyncEndpoints {
    source: Arc<dyn SyncEndpoint>,
    target: Arc<dyn SyncEndpoint>,
}

impl SyncEndpoints {
    /// 创建本地源端点和本地目标端点，保持当前 CLI 行为不变。
    pub fn local(source: PathBuf, target: PathBuf) -> Self {
        Self {
            source: Arc::new(LocalEndpoint::new(source)),
            target: Arc::new(LocalEndpoint::new(target)),
        }
    }

    /// 使用给定端点创建同步端点组。
    pub fn new(source: Arc<dyn SyncEndpoint>, target: Arc<dyn SyncEndpoint>) -> Self {
        Self { source, target }
    }

    /// 返回源端点。
    pub fn source(&self) -> &dyn SyncEndpoint {
        self.source.as_ref()
    }

    /// 返回目标端点。
    pub fn target(&self) -> &dyn SyncEndpoint {
        self.target.as_ref()
    }

    /// 扫描源端目录。
    pub fn scan_source(&self, follow_symlinks: bool) -> Result<Snapshot> {
        self.source.scan_required(follow_symlinks)
    }

    /// 按过滤规则扫描源端目录。
    pub fn scan_source_filtered(
        &self,
        follow_symlinks: bool,
        filter: &PathFilter,
    ) -> Result<Snapshot> {
        self.source.scan_required_filtered(follow_symlinks, filter)
    }

    /// 扫描目标端目录。
    pub fn scan_target(&self, follow_symlinks: bool) -> Result<Snapshot> {
        self.target.scan_optional(follow_symlinks)
    }

    /// 按过滤规则扫描目标端目录。
    pub fn scan_target_filtered(
        &self,
        follow_symlinks: bool,
        filter: &PathFilter,
    ) -> Result<Snapshot> {
        self.target.scan_optional_filtered(follow_symlinks, filter)
    }

    /// 比较源端和目标端两个快照条目的内容。
    pub fn same_content(&self, source_entry: &FileEntry, target_entry: &FileEntry) -> Result<bool> {
        self.source.same_file_content(
            &source_entry.relative_path,
            self.target.as_ref(),
            &target_entry.relative_path,
        )
    }
}

impl fmt::Debug for SyncEndpoints {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncEndpoints")
            .field("source", &self.source.root())
            .field("target", &self.target.root())
            .finish()
    }
}

fn copy_file_direct(source: &dyn SyncEndpoint, relative_path: &Path, target: &Path) -> Result<()> {
    ensure_parent(target)?;
    let reader = source.open_read(relative_path)?;
    let target_file = io_context(
        tr_source_target(
            "io.copy_file_direct",
            source.root().join(relative_path).display(),
            target.display(),
        ),
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(target),
    )?;
    copy_stream_to_writer(
        reader,
        target_file,
        target,
        CopyContext {
            copy_key: "io.copy_to_target_file",
            flush_key: "io.flush_target_file",
            finish_key: "io.finish_target_file",
            sync_key: None,
        },
    )
}

fn copy_file_atomic(source: &dyn SyncEndpoint, relative_path: &Path, target: &Path) -> Result<()> {
    let parent = ensure_parent(target)?;
    let temp_path = unique_temp_path(parent);

    let copy_result = (|| {
        let source_reader = source.open_read(relative_path)?;
        let temp_file = io_context(
            tr_path("io.create_temp_file", temp_path.display()),
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path),
        )?;
        copy_stream_to_writer(
            source_reader,
            temp_file,
            &temp_path,
            CopyContext {
                copy_key: "io.copy_to_temp_file",
                flush_key: "io.flush_temp_file",
                finish_key: "io.finish_temp_file",
                sync_key: Some("io.sync_temp_file"),
            },
        )
    })();

    if let Err(error) = copy_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    replace_with_temp(&temp_path, target)
}

fn copy_stream_to_writer(
    source_reader: Box<dyn Read + Send>,
    target_file: File,
    target: &Path,
    context: CopyContext,
) -> Result<()> {
    let mut reader = BufReader::with_capacity(1024 * 1024, source_reader);
    let mut writer = BufWriter::with_capacity(1024 * 1024, target_file);
    io_context(
        tr_path(context.copy_key, target.display()),
        std::io::copy(&mut reader, &mut writer),
    )?;
    io_context(tr_path(context.flush_key, target.display()), writer.flush())?;
    let file = writer.into_inner().map_err(|error| FastSyncError::Io {
        context: tr_path(context.finish_key, target.display()),
        source: error.into_error(),
    })?;
    if let Some(sync_key) = context.sync_key {
        io_context(tr_path(sync_key, target.display()), file.sync_data())?;
    }
    Ok(())
}

struct CopyContext {
    copy_key: &'static str,
    flush_key: &'static str,
    finish_key: &'static str,
    sync_key: Option<&'static str>,
}

fn replace_with_temp(temp_path: &Path, target: &Path) -> Result<()> {
    match fs::rename(temp_path, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            io_context(
                tr_path("io.remove_old_target_before_replace", target.display()),
                fs::remove_file(target),
            )?;
            io_context(
                tr_path("io.rename_temp_to_target", target.display()),
                fs::rename(temp_path, target),
            )
        }
        Err(error) => {
            let _ = fs::remove_file(temp_path);
            Err(FastSyncError::Io {
                context: tr_path("io.rename_temp_to_target", target.display()),
                source: error,
            })
        }
    }
}

fn ensure_parent(path: &Path) -> Result<&Path> {
    let Some(parent) = path.parent() else {
        return Err(FastSyncError::Io {
            context: tr_path("io.missing_parent", path.display()),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent"),
        });
    };
    io_context(
        tr_path("io.create_parent", parent.display()),
        fs::create_dir_all(parent),
    )?;
    Ok(parent)
}

fn unique_temp_path(parent: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".fastsync.tmp.{}.{}", std::process::id(), counter))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn local_endpoint_rejects_paths_outside_root() {
        let endpoint = LocalEndpoint::new(PathBuf::from("/tmp/root"));

        assert!(endpoint.resolve(Path::new("../escape")).is_err());
        assert!(endpoint.resolve(Path::new("/absolute")).is_err());
    }

    #[test]
    fn local_endpoint_accepts_nested_relative_paths() -> Result<()> {
        let endpoint = LocalEndpoint::new(PathBuf::from("/tmp/root"));

        let path = endpoint.resolve(Path::new("nested/file.txt"))?;

        assert_eq!(path, PathBuf::from("/tmp/root/nested/file.txt"));
        Ok(())
    }
}
