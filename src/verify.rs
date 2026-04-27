use std::path::Path;

use crate::error::{FastSyncError, Result};
use crate::hash::same_blake3;
use crate::scan::Snapshot;

/// 校验单个已经复制或覆盖的文件。
pub fn verify_file(source: &Path, target: &Path, relative_path: &Path) -> Result<()> {
    if same_blake3(source, target)? {
        Ok(())
    } else {
        Err(FastSyncError::VerificationFailed(
            relative_path.to_path_buf(),
        ))
    }
}

/// 同步后校验源目录中的所有普通文件。
pub fn verify_all_source_files(source_snapshot: &Snapshot, target_root: &Path) -> Result<usize> {
    let mut verified = 0;

    for entry in source_snapshot
        .entries
        .values()
        .filter(|entry| entry.is_file())
    {
        let target = target_root.join(&entry.relative_path);
        verify_file(&entry.absolute_path, &target, &entry.relative_path)?;
        verified += 1;
    }

    Ok(verified)
}
