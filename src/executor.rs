use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use tracing::{debug, info};

use crate::config::SyncConfig;
use crate::error::{FastSyncError, Result, io_context};
use crate::i18n::{tr_current, tr_path, tr_source_target};
use crate::plan::{CopyReason, PlanOperation, SyncPlan};
use crate::summary::SyncSummary;
use crate::verify::verify_file;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
enum WorkerTask {
    CopyFile {
        relative_path: PathBuf,
        bytes: u64,
        reason: CopyReason,
    },
    SetMetadata {
        relative_path: PathBuf,
    },
}

#[derive(Debug, Default)]
struct WorkerReport {
    copied_files: usize,
    metadata_updates: usize,
    verified_files: usize,
    bytes_copied: u64,
}

/// 执行同步计划。
///
/// 目录创建和删除按顺序执行，文件复制与元数据修正通过有界队列交给固定数量
/// worker 处理，以避免扫描/调度阶段无限占用内存。
pub fn execute_plan(config: &SyncConfig, plan: &SyncPlan) -> Result<SyncSummary> {
    if config.dry_run {
        return Ok(dry_run_summary(config, plan));
    }

    io_context(
        tr_path("io.create_target_root", config.target.display()),
        fs::create_dir_all(&config.target),
    )?;

    let mut summary = SyncSummary {
        dry_run: false,
        ..SyncSummary::default()
    };
    let mut worker_tasks = Vec::new();
    let mut delete_ops = Vec::new();

    for operation in &plan.operations {
        match operation {
            PlanOperation::CreateDirectory { relative_path } => {
                create_directory(&config.target, relative_path)?;
                summary.created_dirs += 1;
            }
            PlanOperation::CopyFile {
                relative_path,
                bytes,
                reason,
            } => worker_tasks.push(WorkerTask::CopyFile {
                relative_path: relative_path.clone(),
                bytes: *bytes,
                reason: *reason,
            }),
            PlanOperation::SetMetadata { relative_path } => {
                worker_tasks.push(WorkerTask::SetMetadata {
                    relative_path: relative_path.clone(),
                });
            }
            PlanOperation::DeleteFile { .. }
            | PlanOperation::DeleteDirectory { .. }
            | PlanOperation::DeleteSymlink { .. } => delete_ops.push(operation.clone()),
        }
    }

    let report = run_workers(config, worker_tasks)?;
    summary.copied_files += report.copied_files;
    summary.metadata_updates += report.metadata_updates;
    summary.verified_files += report.verified_files;
    summary.bytes_copied += report.bytes_copied;

    for operation in delete_ops {
        match operation {
            PlanOperation::DeleteFile { relative_path } => {
                delete_file(&config.target, &relative_path)?;
                summary.deleted_files += 1;
            }
            PlanOperation::DeleteDirectory { relative_path } => {
                delete_directory(&config.target, &relative_path)?;
                summary.deleted_dirs += 1;
            }
            PlanOperation::DeleteSymlink { relative_path } => {
                delete_symlink(&config.target, &relative_path)?;
                summary.deleted_symlinks += 1;
            }
            _ => {}
        }
    }

    Ok(summary)
}

fn dry_run_summary(config: &SyncConfig, plan: &SyncPlan) -> SyncSummary {
    let mut summary = SyncSummary {
        dry_run: true,
        ..SyncSummary::default()
    };

    for operation in &plan.operations {
        match operation {
            PlanOperation::CreateDirectory { .. } => summary.created_dirs += 1,
            PlanOperation::CopyFile { bytes, .. } => {
                summary.copied_files += 1;
                summary.bytes_copied = summary.bytes_copied.saturating_add(*bytes);
            }
            PlanOperation::SetMetadata { .. } => summary.metadata_updates += 1,
            PlanOperation::DeleteFile { .. } => summary.deleted_files += 1,
            PlanOperation::DeleteDirectory { .. } => summary.deleted_dirs += 1,
            PlanOperation::DeleteSymlink { .. } => summary.deleted_symlinks += 1,
        }
    }
    summary.errors = 0;
    summary.source = config.source.clone();
    summary.target = config.target.clone();
    summary
}

fn run_workers(config: &SyncConfig, tasks: Vec<WorkerTask>) -> Result<WorkerReport> {
    if tasks.is_empty() {
        return Ok(WorkerReport::default());
    }

    let (task_sender, task_receiver) = bounded(config.queue_size);
    let (report_sender, report_receiver) = unbounded();

    thread::scope(|scope| {
        for worker_id in 0..config.threads {
            let task_receiver = task_receiver.clone();
            let report_sender = report_sender.clone();
            scope.spawn(move || worker_loop(worker_id, config, task_receiver, report_sender));
        }

        drop(report_sender);
        send_tasks(&task_sender, tasks);
        drop(task_sender);

        collect_worker_reports(config, report_receiver)
    })
}

fn send_tasks(sender: &Sender<WorkerTask>, tasks: Vec<WorkerTask>) {
    for task in tasks {
        if sender.send(task).is_err() {
            break;
        }
    }
}

fn worker_loop(
    worker_id: usize,
    config: &SyncConfig,
    receiver: Receiver<WorkerTask>,
    sender: Sender<Result<WorkerReport>>,
) {
    let mut report = WorkerReport::default();

    for task in receiver {
        let result = match task {
            WorkerTask::CopyFile {
                relative_path,
                bytes,
                reason,
            } => copy_one_file(config, &relative_path, bytes, reason, &mut report),
            WorkerTask::SetMetadata { relative_path } => {
                apply_file_metadata(config, &relative_path).map(|_| {
                    report.metadata_updates += 1;
                })
            }
        };

        if let Err(error) = result {
            let _ = sender.send(Err(error));
            if config.stop_on_error {
                debug!(worker_id, "{}", tr_current("log.worker_stop_on_error"));
                return;
            }
        }
    }

    let _ = sender.send(Ok(report));
}

fn collect_worker_reports(
    config: &SyncConfig,
    receiver: Receiver<Result<WorkerReport>>,
) -> Result<WorkerReport> {
    let mut merged = WorkerReport::default();
    let mut errors = Vec::new();

    for message in receiver {
        match message {
            Ok(report) => {
                merged.copied_files += report.copied_files;
                merged.metadata_updates += report.metadata_updates;
                merged.verified_files += report.verified_files;
                merged.bytes_copied += report.bytes_copied;
            }
            Err(error) => {
                errors.push(error.to_string());
                if config.stop_on_error || errors.len() >= config.max_errors {
                    break;
                }
            }
        }
    }

    if let Some(first) = errors.first() {
        Err(FastSyncError::Many {
            count: errors.len(),
            first: first.clone(),
        })
    } else {
        Ok(merged)
    }
}

fn copy_one_file(
    config: &SyncConfig,
    relative_path: &Path,
    bytes: u64,
    reason: CopyReason,
    report: &mut WorkerReport,
) -> Result<()> {
    let source = config.source.join(relative_path);
    let target = config.target.join(relative_path);

    if reason == CopyReason::Missing {
        copy_file_direct(&source, &target)?;
    } else if config.atomic_write {
        copy_file_atomic(&source, &target)?;
    } else {
        copy_file_direct(&source, &target)?;
    }
    apply_file_metadata(config, relative_path)?;

    if reason != CopyReason::Missing && config.verify_mode.verify_changed_files() {
        verify_file(&source, &target, relative_path)?;
        report.verified_files += 1;
    }

    report.copied_files += 1;
    report.bytes_copied = report.bytes_copied.saturating_add(bytes);
    info!(path = %relative_path.display(), bytes, "{}", tr_current("log.file_copied"));
    Ok(())
}

fn create_directory(target_root: &Path, relative_path: &Path) -> Result<()> {
    let path = target_root.join(relative_path);
    io_context(
        tr_path("io.create_directory", path.display()),
        fs::create_dir_all(path),
    )
}

fn delete_file(target_root: &Path, relative_path: &Path) -> Result<()> {
    let path = target_root.join(relative_path);
    io_context(
        tr_path("io.delete_file", path.display()),
        fs::remove_file(path),
    )?;
    info!(path = %relative_path.display(), "{}", tr_current("log.file_deleted"));
    Ok(())
}

fn delete_directory(target_root: &Path, relative_path: &Path) -> Result<()> {
    let path = target_root.join(relative_path);
    io_context(
        tr_path("io.delete_directory", path.display()),
        fs::remove_dir(path),
    )?;
    info!(path = %relative_path.display(), "{}", tr_current("log.directory_deleted"));
    Ok(())
}

fn delete_symlink(target_root: &Path, relative_path: &Path) -> Result<()> {
    let path = target_root.join(relative_path);
    io_context(
        tr_path("io.delete_symlink", path.display()),
        fs::remove_file(path),
    )?;
    info!(path = %relative_path.display(), "{}", tr_current("log.symlink_deleted"));
    Ok(())
}

fn copy_file_atomic(source: &Path, target: &Path) -> Result<()> {
    let parent = ensure_parent(target)?;
    let temp_path = unique_temp_path(parent);

    let copy_result = (|| {
        let source_file = io_context(
            tr_path("io.open_source_file", source.display()),
            File::open(source),
        )?;
        let temp_file = io_context(
            tr_path("io.create_temp_file", temp_path.display()),
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path),
        )?;
        let mut reader = BufReader::with_capacity(1024 * 1024, source_file);
        let mut writer = BufWriter::with_capacity(1024 * 1024, temp_file);
        io_context(
            tr_path("io.copy_to_temp_file", temp_path.display()),
            std::io::copy(&mut reader, &mut writer),
        )?;
        io_context(
            tr_path("io.flush_temp_file", temp_path.display()),
            writer.flush(),
        )?;
        let file = writer.into_inner().map_err(|error| FastSyncError::Io {
            context: tr_path("io.finish_temp_file", temp_path.display()),
            source: error.into_error(),
        })?;
        io_context(
            tr_path("io.sync_temp_file", temp_path.display()),
            file.sync_data(),
        )?;
        Ok(())
    })();

    if let Err(error) = copy_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    match fs::rename(&temp_path, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            io_context(
                tr_path("io.remove_old_target_before_replace", target.display()),
                fs::remove_file(target),
            )?;
            io_context(
                tr_path("io.rename_temp_to_target", target.display()),
                fs::rename(&temp_path, target),
            )
        }
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(FastSyncError::Io {
                context: tr_path("io.rename_temp_to_target", target.display()),
                source: error,
            })
        }
    }
}

fn copy_file_direct(source: &Path, target: &Path) -> Result<()> {
    ensure_parent(target)?;
    io_context(
        tr_source_target("io.copy_file_direct", source.display(), target.display()),
        fs::copy(source, target),
    )?;
    Ok(())
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

fn apply_file_metadata(config: &SyncConfig, relative_path: &Path) -> Result<()> {
    let source = config.source.join(relative_path);
    let target = config.target.join(relative_path);
    let source_metadata = io_context(
        tr_path("io.read_source_metadata", source.display()),
        fs::metadata(&source),
    )?;

    if config.preserve_permissions.enabled() {
        let permissions = source_metadata.permissions();
        io_context(
            tr_path("io.set_target_permissions", target.display()),
            fs::set_permissions(&target, permissions),
        )?;
    }

    if config.preserve_times.enabled() {
        let atime = filetime::FileTime::from_last_access_time(&source_metadata);
        let mtime = filetime::FileTime::from_last_modification_time(&source_metadata);
        io_context(
            tr_path("io.set_target_times", target.display()),
            filetime::set_file_times(&target, atime, mtime),
        )?;
    }

    Ok(())
}
