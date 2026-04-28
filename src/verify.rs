use std::path::Path;

use crate::endpoint::SyncEndpoints;
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

/// 通过端点校验单个已经复制或覆盖的文件。
pub fn verify_file_with_endpoints(endpoints: &SyncEndpoints, relative_path: &Path) -> Result<()> {
    if endpoints
        .source()
        .same_file_content(relative_path, endpoints.target(), relative_path)?
    {
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

/// 通过端点同步后校验源目录中的所有普通文件。
pub fn verify_all_source_files_with_endpoints(
    source_snapshot: &Snapshot,
    endpoints: &SyncEndpoints,
) -> Result<usize> {
    let mut verified = 0;

    for entry in source_snapshot
        .entries
        .values()
        .filter(|entry| entry.is_file())
    {
        verify_file_with_endpoints(endpoints, &entry.relative_path)?;
        verified += 1;
    }

    Ok(verified)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::error::FastSyncError;
    use crate::scan::scan_directory;

    use super::*;

    #[test]
    fn verify_file_accepts_matching_content() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let root = tempdir()?;
        let source = root.path().join("source.txt");
        let target = root.path().join("target.txt");
        fs::write(&source, "same")?;
        fs::write(&target, "same")?;

        verify_file(&source, &target, Path::new("source.txt"))?;

        Ok(())
    }

    #[test]
    fn verify_file_rejects_mismatched_content()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.txt");
        let target = root.path().join("target.txt");
        fs::write(&source, "source")?;
        fs::write(&target, "target")?;

        let error = verify_file(&source, &target, Path::new("source.txt"))
            .expect_err("mismatched content should fail");

        assert!(matches!(error, FastSyncError::VerificationFailed(_)));
        Ok(())
    }

    #[test]
    fn verify_all_source_files_ignores_directories()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::create_dir(source.path().join("nested"))?;
        fs::create_dir(target.path().join("nested"))?;
        fs::write(source.path().join("nested").join("a.txt"), "same")?;
        fs::write(target.path().join("nested").join("a.txt"), "same")?;
        let snapshot = scan_directory(source.path(), false)?;

        let verified = verify_all_source_files(&snapshot, target.path())?;

        assert_eq!(verified, 1);
        Ok(())
    }
}
