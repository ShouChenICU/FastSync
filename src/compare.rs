use std::cmp::Reverse;

use crate::config::{CompareMode, SyncConfig};
use crate::endpoint::SyncEndpoints;
use crate::error::{FastSyncError, Result};
use crate::plan::{CopyReason, PlanOperation, SyncPlan};
use crate::scan::{EntryKind, FileEntry, Snapshot};

/// 基于源/目标快照生成同步计划。
///
/// 比较阶段只产生任务，不直接修改文件系统。这样可以支持 dry-run、统计、
/// 重试和后续更复杂的调度策略。
pub fn build_plan(config: &SyncConfig, source: &Snapshot, target: &Snapshot) -> Result<SyncPlan> {
    let endpoints = SyncEndpoints::local(config.source.clone(), config.target.clone());
    build_plan_with_endpoints(config, &endpoints, source, target)
}

/// 使用显式端点生成同步计划。
///
/// 该入口把内容比较委托给端点层，避免比较阶段绑定本地文件系统路径。
pub fn build_plan_with_endpoints(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    source: &Snapshot,
    target: &Snapshot,
) -> Result<SyncPlan> {
    let mut plan = SyncPlan::default();

    for source_entry in source.entries.values() {
        match source_entry.kind {
            EntryKind::Directory => {
                if target.get(&source_entry.relative_path).is_none() {
                    plan.push(PlanOperation::CreateDirectory {
                        relative_path: source_entry.relative_path.clone(),
                    });
                } else if let Some(target_entry) = target.get(&source_entry.relative_path) {
                    ensure_same_kind(source_entry, target_entry)?;
                }
            }
            EntryKind::File => plan_file(config, endpoints, source_entry, target, &mut plan)?,
            EntryKind::Symlink => {}
        }
    }

    if config.delete {
        let mut obsolete: Vec<_> = target
            .entries
            .values()
            .filter(|entry| source.get(&entry.relative_path).is_none())
            .collect();
        obsolete.sort_by_key(|entry| Reverse(entry.relative_path.components().count()));

        for entry in obsolete {
            match entry.kind {
                EntryKind::File => plan.push(PlanOperation::DeleteFile {
                    relative_path: entry.relative_path.clone(),
                }),
                EntryKind::Directory => plan.push(PlanOperation::DeleteDirectory {
                    relative_path: entry.relative_path.clone(),
                }),
                EntryKind::Symlink => plan.push(PlanOperation::DeleteSymlink {
                    relative_path: entry.relative_path.clone(),
                }),
            }
        }
    }

    Ok(plan)
}

fn plan_file(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    source_entry: &FileEntry,
    target: &Snapshot,
    plan: &mut SyncPlan,
) -> Result<()> {
    let Some(target_entry) = target.get(&source_entry.relative_path) else {
        plan.push(PlanOperation::CopyFile {
            relative_path: source_entry.relative_path.clone(),
            bytes: source_entry.len,
            reason: CopyReason::Missing,
        });
        return Ok(());
    };

    ensure_same_kind(source_entry, target_entry)?;

    if let Some(reason) =
        content_change_reason(config, endpoints, source_entry, target_entry, plan)?
    {
        plan.push(PlanOperation::CopyFile {
            relative_path: source_entry.relative_path.clone(),
            bytes: source_entry.len,
            reason,
        });
    } else if config.syncs_file_metadata()
        && sync_metadata_differs(config, source_entry, target_entry)
    {
        plan.push(PlanOperation::SetMetadata {
            relative_path: source_entry.relative_path.clone(),
        });
    }

    Ok(())
}

fn content_change_reason(
    config: &SyncConfig,
    endpoints: &SyncEndpoints,
    source_entry: &FileEntry,
    target_entry: &FileEntry,
    plan: &mut SyncPlan,
) -> Result<Option<CopyReason>> {
    match config.compare_mode {
        CompareMode::Fast => {
            if !content_metadata_differs(source_entry, target_entry) {
                Ok(None)
            } else if source_entry.len != target_entry.len {
                Ok(Some(CopyReason::MetadataChanged))
            } else if same_content_by_blake3(endpoints, source_entry, target_entry, plan)? {
                Ok(None)
            } else {
                Ok(Some(CopyReason::ContentChanged))
            }
        }
        CompareMode::Strict => {
            if source_entry.len != target_entry.len {
                Ok(Some(CopyReason::MetadataChanged))
            } else if same_content_by_blake3(endpoints, source_entry, target_entry, plan)? {
                Ok(None)
            } else {
                Ok(Some(CopyReason::ContentChanged))
            }
        }
    }
}

fn same_content_by_blake3(
    endpoints: &SyncEndpoints,
    source_entry: &FileEntry,
    target_entry: &FileEntry,
    plan: &mut SyncPlan,
) -> Result<bool> {
    plan.record_blake3_comparison();
    endpoints.same_content(source_entry, target_entry)
}

fn ensure_same_kind(source: &FileEntry, target: &FileEntry) -> Result<()> {
    if source.kind == target.kind {
        return Ok(());
    }

    Err(FastSyncError::PathTypeConflict {
        relative_path: source.relative_path.clone(),
        source_kind: source.kind.as_str(),
        target_kind: target.kind.as_str(),
    })
}

fn content_metadata_differs(source: &FileEntry, target: &FileEntry) -> bool {
    source.len != target.len
        || source.modified != target.modified
        || permission_metadata_differs(source, target)
}

fn sync_metadata_differs(config: &SyncConfig, source: &FileEntry, target: &FileEntry) -> bool {
    let time_differs = config.preserve_times.enabled() && source.modified != target.modified;
    let permission_differs =
        config.preserve_permissions.enabled() && permission_metadata_differs(source, target);

    time_differs || permission_differs
}

fn permission_metadata_differs(source: &FileEntry, target: &FileEntry) -> bool {
    source.readonly != target.readonly || platform_permissions_differ(source, target)
}

fn platform_permissions_differ(source: &FileEntry, target: &FileEntry) -> bool {
    #[cfg(unix)]
    {
        source.mode != target.mode
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
        false
    }
}
