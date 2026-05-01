use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use tracing::{debug, info};

use crate::config::SyncConfig;
use crate::endpoint::SyncEndpoints;
use crate::error::{FastSyncError, Result};
use crate::i18n::tr_current;
use crate::plan::{CopyReason, PlanOperation, SyncPlan};
use crate::progress::ProgressPhase;
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
    execute_plan_with_progress(config, endpoints, plan, &ProgressPhase::disabled())
}

/// 使用显式端点和进度句柄执行同步计划。
///
/// 进度在每个计划操作完成或失败后递增，帮助交互式终端观察长时间复制、
/// 元数据修正和删除过程；错误聚合、停止策略和执行顺序仍由原执行器控制。
pub(crate) fn execute_plan_with_progress(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    plan: &SyncPlan,
    progress: &ProgressPhase,
) -> Result<SyncSummary> {
    if config.dry_run {
        return Ok(dry_run_summary(endpoints, plan));
    }

    endpoints.target().ensure_root()?;

    let mut summary = SyncSummary {
        dry_run: false,
        ..SyncSummary::default()
    };
    let dispatch = prepare_plan_directories_and_deletes(endpoints, plan, progress)?;
    let report = run_workers(config, endpoints, plan, progress)?;
    summary.created_dirs += dispatch.created_dirs;
    summary.copied_files += report.copied_files;
    summary.metadata_updates += report.metadata_updates;
    summary.verified_files += report.verified_files;
    summary.bytes_copied += report.bytes_copied;

    for operation in dispatch.delete_ops {
        match operation {
            PlanOperation::DeleteFile { relative_path } => {
                endpoints.target().delete_file(&relative_path)?;
                info!(path = %relative_path.display(), "{}", tr_current("log.file_deleted"));
                summary.deleted_files += 1;
                progress.inc(1);
            }
            PlanOperation::DeleteDirectory { relative_path } => {
                endpoints.target().delete_directory(&relative_path)?;
                info!(path = %relative_path.display(), "{}", tr_current("log.directory_deleted"));
                summary.deleted_dirs += 1;
                progress.inc(1);
            }
            PlanOperation::DeleteSymlink { relative_path } => {
                endpoints.target().delete_symlink(&relative_path)?;
                info!(path = %relative_path.display(), "{}", tr_current("log.symlink_deleted"));
                summary.deleted_symlinks += 1;
                progress.inc(1);
            }
            _ => {}
        }
    }

    Ok(summary)
}

struct DispatchReport {
    created_dirs: usize,
    delete_ops: Vec<PlanOperation>,
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

fn prepare_plan_directories_and_deletes(
    endpoints: &SyncEndpoints,
    plan: &SyncPlan,
    progress: &ProgressPhase,
) -> Result<DispatchReport> {
    let mut report = DispatchReport {
        created_dirs: 0,
        delete_ops: Vec::new(),
    };

    for operation in &plan.operations {
        match operation {
            PlanOperation::CreateDirectory { relative_path } => {
                endpoints.target().create_directory(relative_path)?;
                report.created_dirs += 1;
                progress.inc(1);
            }
            PlanOperation::DeleteFile { .. }
            | PlanOperation::DeleteDirectory { .. }
            | PlanOperation::DeleteSymlink { .. } => report.delete_ops.push(operation.clone()),
            PlanOperation::CopyFile { .. } | PlanOperation::SetMetadata { .. } => {}
        }
    }

    Ok(report)
}

fn run_workers(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    plan: &SyncPlan,
    progress: &ProgressPhase,
) -> Result<WorkerReport> {
    let (task_sender, task_receiver) = bounded(config.queue_size);
    let (report_sender, report_receiver) = unbounded();

    thread::scope(|scope| {
        for worker_id in 0..config.threads {
            let task_receiver = task_receiver.clone();
            let report_sender = report_sender.clone();
            let progress = progress.clone();
            scope.spawn(move || {
                worker_loop(
                    worker_id,
                    config,
                    endpoints,
                    task_receiver,
                    report_sender,
                    progress,
                )
            });
        }

        drop(report_sender);
        dispatch_worker_operations(plan, &task_sender);
        drop(task_sender);

        collect_worker_reports(config, report_receiver)
    })
}

fn dispatch_worker_operations(plan: &SyncPlan, sender: &Sender<WorkerTask>) {
    for operation in &plan.operations {
        match operation {
            PlanOperation::CopyFile {
                relative_path,
                bytes,
                reason,
            } => {
                if sender
                    .send(WorkerTask::CopyFile {
                        relative_path: relative_path.clone(),
                        bytes: *bytes,
                        reason: *reason,
                    })
                    .is_err()
                {
                    break;
                }
            }
            PlanOperation::SetMetadata { relative_path } => {
                if sender
                    .send(WorkerTask::SetMetadata {
                        relative_path: relative_path.clone(),
                    })
                    .is_err()
                {
                    break;
                }
            }
            PlanOperation::CreateDirectory { .. }
            | PlanOperation::DeleteFile { .. }
            | PlanOperation::DeleteDirectory { .. }
            | PlanOperation::DeleteSymlink { .. } => {}
        }
    }
}

fn worker_loop(
    worker_id: usize,
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    receiver: Receiver<WorkerTask>,
    sender: Sender<Result<WorkerReport>>,
    progress: ProgressPhase,
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
            progress.inc(1);
            if config.stop_on_error {
                debug!(worker_id, "{}", tr_current("log.worker_stop_on_error"));
                return;
            }
        } else {
            progress.inc(1);
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::cli::Cli;
    use crate::config::SyncConfig;

    use super::*;

    #[test]
    fn execute_plan_streams_worker_tasks_without_losing_operations()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::create_dir(source.path().join("nested"))?;
        fs::write(source.path().join("nested").join("a.txt"), "alpha")?;
        fs::write(source.path().join("b.txt"), "beta")?;

        let cli = Cli::for_test(source.path(), target.path());
        let config = SyncConfig::try_from(cli)?;
        let mut plan = SyncPlan::default();
        plan.push(PlanOperation::CreateDirectory {
            relative_path: PathBuf::from("nested"),
        });
        plan.push(PlanOperation::CopyFile {
            relative_path: PathBuf::from("nested").join("a.txt"),
            bytes: 5,
            reason: CopyReason::Missing,
        });
        plan.push(PlanOperation::CopyFile {
            relative_path: PathBuf::from("b.txt"),
            bytes: 4,
            reason: CopyReason::Missing,
        });

        let summary = execute_plan(&config, &plan)?;

        assert_eq!(
            fs::read_to_string(target.path().join("nested").join("a.txt"))?,
            "alpha"
        );
        assert_eq!(fs::read_to_string(target.path().join("b.txt"))?, "beta");
        assert_eq!(summary.created_dirs, 1);
        assert_eq!(summary.copied_files, 2);
        assert_eq!(summary.bytes_copied, 9);
        Ok(())
    }
}
