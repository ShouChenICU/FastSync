use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use tracing::{debug, info};

use crate::config::SyncConfig;
use crate::endpoint::SyncEndpoints;
use crate::error::{FastSyncError, Result};
use crate::i18n::tr_current;
use crate::plan::{CopyReason, PlanOperation, SyncPlan};
use crate::summary::SyncSummary;
use crate::verify::verify_file_with_endpoints;

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
    let endpoints = SyncEndpoints::local(config.source.clone(), config.target.clone());
    execute_plan_with_endpoints(config, &endpoints, plan)
}

/// 使用显式端点执行同步计划。
///
/// 编排层只处理操作顺序、worker 调度和摘要统计；具体文件系统 I/O 交给端点层。
pub fn execute_plan_with_endpoints(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    plan: &SyncPlan,
) -> Result<SyncSummary> {
    if config.dry_run {
        return Ok(dry_run_summary(endpoints, plan));
    }

    endpoints.target().ensure_root()?;

    let mut summary = SyncSummary {
        dry_run: false,
        ..SyncSummary::default()
    };
    let mut worker_tasks = Vec::new();
    let mut delete_ops = Vec::new();

    for operation in &plan.operations {
        match operation {
            PlanOperation::CreateDirectory { relative_path } => {
                endpoints.target().create_directory(relative_path)?;
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

    let report = run_workers(config, endpoints, worker_tasks)?;
    summary.copied_files += report.copied_files;
    summary.metadata_updates += report.metadata_updates;
    summary.verified_files += report.verified_files;
    summary.bytes_copied += report.bytes_copied;

    for operation in delete_ops {
        match operation {
            PlanOperation::DeleteFile { relative_path } => {
                endpoints.target().delete_file(&relative_path)?;
                info!(path = %relative_path.display(), "{}", tr_current("log.file_deleted"));
                summary.deleted_files += 1;
            }
            PlanOperation::DeleteDirectory { relative_path } => {
                endpoints.target().delete_directory(&relative_path)?;
                info!(path = %relative_path.display(), "{}", tr_current("log.directory_deleted"));
                summary.deleted_dirs += 1;
            }
            PlanOperation::DeleteSymlink { relative_path } => {
                endpoints.target().delete_symlink(&relative_path)?;
                info!(path = %relative_path.display(), "{}", tr_current("log.symlink_deleted"));
                summary.deleted_symlinks += 1;
            }
            _ => {}
        }
    }

    Ok(summary)
}

fn dry_run_summary(endpoints: &SyncEndpoints, plan: &SyncPlan) -> SyncSummary {
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
    summary.source = endpoints.source().root().to_path_buf();
    summary.target = endpoints.target().root().to_path_buf();
    summary
}

fn run_workers(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    tasks: Vec<WorkerTask>,
) -> Result<WorkerReport> {
    if tasks.is_empty() {
        return Ok(WorkerReport::default());
    }

    let (task_sender, task_receiver) = bounded(config.queue_size);
    let (report_sender, report_receiver) = unbounded();

    thread::scope(|scope| {
        for worker_id in 0..config.threads {
            let task_receiver = task_receiver.clone();
            let report_sender = report_sender.clone();
            scope.spawn(move || {
                worker_loop(worker_id, config, endpoints, task_receiver, report_sender)
            });
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
    endpoints: &SyncEndpoints,
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
            } => copy_one_file(
                config,
                endpoints,
                &relative_path,
                bytes,
                reason,
                &mut report,
            ),
            WorkerTask::SetMetadata { relative_path } => endpoints
                .target()
                .apply_file_metadata_from(endpoints.source(), &relative_path, config)
                .map(|_| {
                    report.metadata_updates += 1;
                }),
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
    endpoints: &SyncEndpoints,
    relative_path: &Path,
    bytes: u64,
    reason: CopyReason,
    report: &mut WorkerReport,
) -> Result<()> {
    if reason == CopyReason::Missing {
        endpoints
            .target()
            .copy_file_from(endpoints.source(), relative_path, false)?;
    } else if config.atomic_write {
        endpoints
            .target()
            .copy_file_from(endpoints.source(), relative_path, true)?;
    } else {
        endpoints
            .target()
            .copy_file_from(endpoints.source(), relative_path, false)?;
    }
    endpoints
        .target()
        .apply_file_metadata_from(endpoints.source(), relative_path, config)?;

    if reason != CopyReason::Missing && config.verify_mode.verify_changed_files() {
        verify_file_with_endpoints(endpoints, relative_path)?;
        report.verified_files += 1;
    }

    report.copied_files += 1;
    report.bytes_copied = report.bytes_copied.saturating_add(bytes);
    info!(path = %relative_path.display(), bytes, "{}", tr_current("log.file_copied"));
    Ok(())
}
