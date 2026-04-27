//! fastsync 的核心库入口。
//!
//! 这里负责串联“扫描 -> 比较 -> 执行 -> 校验/汇总”的主流程。

pub mod cli;
pub mod compare;
pub mod config;
pub mod error;
pub mod executor;
pub mod hash;
pub mod plan;
pub mod scan;
pub mod summary;
pub mod verify;

use std::time::Instant;

use tracing::info;

use crate::compare::build_plan;
use crate::config::SyncConfig;
use crate::error::Result;
use crate::executor::execute_plan;
use crate::scan::{scan_directory, scan_optional_directory};
use crate::summary::SyncSummary;
use crate::verify::verify_all_source_files;

/// 执行一次单向目录同步。
///
/// 输入为已经解析完成的配置，输出为稳定的同步摘要。该函数不负责解析 CLI，
/// 也不直接渲染终端输出，方便后续被测试、GUI 或服务化入口复用。
pub fn run_sync(config: SyncConfig) -> Result<SyncSummary> {
    let started = Instant::now();
    info!(
        source = %config.source.display(),
        target = %config.target.display(),
        "开始扫描目录"
    );

    let source_snapshot = scan_directory(&config.source, config.follow_symlinks)?;
    let target_snapshot = scan_optional_directory(&config.target, config.follow_symlinks)?;

    info!(
        source_entries = source_snapshot.entries.len(),
        target_entries = target_snapshot.entries.len(),
        "目录扫描完成"
    );

    let plan = build_plan(&config, &source_snapshot, &target_snapshot)?;
    info!(
        operations = plan.operations.len(),
        bytes = plan.bytes_to_copy,
        "同步计划已生成"
    );

    let mut summary = execute_plan(&config, &plan)?;
    summary.source = config.source.clone();
    summary.target = config.target.clone();
    summary.source_entries = source_snapshot.entries.len();
    summary.target_entries = target_snapshot.entries.len();
    summary.planned_operations = plan.operations.len();
    summary.bytes_planned = plan.bytes_to_copy;

    if !config.dry_run && config.verify_mode.verify_all_files() {
        let verified = verify_all_source_files(&source_snapshot, &config.target)?;
        summary.verified_files += verified;
    }

    summary.duration_ms = started.elapsed().as_millis();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::cli::Cli;
    use crate::config::SyncConfig;

    #[test]
    fn run_sync_copies_new_file() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("a.txt"), "hello")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.verify = crate::config::VerifyMode::Changed;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(target.path().join("a.txt"))?, "hello");
        assert_eq!(summary.copied_files, 1);
        assert_eq!(summary.verified_files, 0);
        assert_eq!(summary.errors, 0);
        Ok(())
    }

    #[test]
    fn dry_run_does_not_modify_target() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("a.txt"), "hello")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.dry_run = true;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert!(!target.path().join("a.txt").exists());
        assert_eq!(summary.planned_operations, 1);
        assert_eq!(summary.copied_files, 1);
        Ok(())
    }

    #[test]
    fn hash_compare_overwrites_changed_file() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("a.txt"), "new-value")?;
        fs::write(target.path().join("a.txt"), "old-value")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.compare = crate::config::CompareMode::Hash;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(
            fs::read_to_string(target.path().join("a.txt"))?,
            "new-value"
        );
        assert_eq!(summary.copied_files, 1);
        assert_eq!(summary.verified_files, 1);
        Ok(())
    }

    #[test]
    fn auto_compare_hashes_when_metadata_matches()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let source_file = source.path().join("a.txt");
        let target_file = target.path().join("a.txt");
        fs::write(&source_file, "new-value")?;
        fs::write(&target_file, "old-value")?;

        let timestamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source_file, timestamp)?;
        filetime::set_file_mtime(&target_file, timestamp)?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.compare = crate::config::CompareMode::Auto;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(target_file)?, "new-value");
        assert_eq!(summary.copied_files, 1);
        Ok(())
    }

    #[test]
    fn fast_compare_trusts_same_size_and_modified_time()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let source_file = source.path().join("a.txt");
        let target_file = target.path().join("a.txt");
        fs::write(&source_file, "new-value")?;
        fs::write(&target_file, "old-value")?;

        let timestamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source_file, timestamp)?;
        filetime::set_file_mtime(&target_file, timestamp)?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.fast = true;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(target_file)?, "old-value");
        assert_eq!(summary.copied_files, 0);
        Ok(())
    }

    #[test]
    fn delete_flag_removes_obsolete_target_file()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("kept.txt"), "keep")?;
        fs::write(target.path().join("stale.txt"), "stale")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.delete = true;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert!(!target.path().join("stale.txt").exists());
        assert_eq!(summary.deleted_files, 1);
        Ok(())
    }
}
