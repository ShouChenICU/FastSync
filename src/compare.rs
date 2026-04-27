use std::cmp::Reverse;

use crate::config::{CompareMode, SyncConfig};
use crate::error::{FastSyncError, Result};
use crate::hash::same_blake3;
use crate::plan::{CopyReason, PlanOperation, SyncPlan};
use crate::scan::{EntryKind, FileEntry, Snapshot};

/// 基于源/目标快照生成同步计划。
///
/// 比较阶段只产生任务，不直接修改文件系统。这样可以支持 dry-run、统计、
/// 重试和后续更复杂的调度策略。
pub fn build_plan(config: &SyncConfig, source: &Snapshot, target: &Snapshot) -> Result<SyncPlan> {
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
            EntryKind::File => plan_file(config, source_entry, target, &mut plan)?,
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

    match config.compare_mode {
        CompareMode::Fast => {
            if metadata_differs(source_entry, target_entry) {
                plan.push(PlanOperation::CopyFile {
                    relative_path: source_entry.relative_path.clone(),
                    bytes: source_entry.len,
                    reason: CopyReason::MetadataChanged,
                });
            }
        }
        CompareMode::Hash => {
            if !same_blake3(&source_entry.absolute_path, &target_entry.absolute_path)? {
                plan.push(PlanOperation::CopyFile {
                    relative_path: source_entry.relative_path.clone(),
                    bytes: source_entry.len,
                    reason: CopyReason::ContentChanged,
                });
            } else if metadata_differs(source_entry, target_entry) {
                plan.push(PlanOperation::SetMetadata {
                    relative_path: source_entry.relative_path.clone(),
                });
            }
        }
        CompareMode::Auto => {
            if metadata_differs(source_entry, target_entry) {
                plan.push(PlanOperation::CopyFile {
                    relative_path: source_entry.relative_path.clone(),
                    bytes: source_entry.len,
                    reason: CopyReason::MetadataChanged,
                });
            } else if !same_blake3(&source_entry.absolute_path, &target_entry.absolute_path)? {
                plan.push(PlanOperation::CopyFile {
                    relative_path: source_entry.relative_path.clone(),
                    bytes: source_entry.len,
                    reason: CopyReason::ContentChanged,
                });
            }
        }
    }

    Ok(())
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

fn metadata_differs(source: &FileEntry, target: &FileEntry) -> bool {
    source.len != target.len
        || source.modified != target.modified
        || source.readonly != target.readonly
        || {
            #[cfg(unix)]
            {
                source.mode != target.mode
            }
            #[cfg(not(unix))]
            {
                false
            }
        }
}
