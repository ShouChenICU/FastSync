//! fastsync 的核心库入口。
//!
//! 这里负责串联“扫描 -> 比较 -> 执行 -> 校验/汇总”的主流程。

rust_i18n::i18n!("locales", fallback = "en");

pub mod cli;
pub mod compare;
pub mod config;
pub mod endpoint;
pub mod error;
pub mod executor;
pub mod filter;
pub mod hash;
pub mod i18n;
pub mod network;
pub mod plan;
mod progress;
pub mod scan;
pub mod summary;
mod timestamp;
pub mod verify;

use std::time::Instant;

use tracing::info;

use crate::compare::build_plan_with_progress;
use crate::config::SyncConfig;
use crate::endpoint::SyncEndpoints;
use crate::error::Result;
use crate::executor::execute_plan_with_progress;
use crate::progress::SyncProgress;
use crate::summary::SyncSummary;
use crate::verify::verify_all_source_files_with_endpoints;

/// 执行一次单向目录同步。
///
/// 输入为已经解析完成的配置，输出为稳定的同步摘要。该函数不负责解析 CLI，
/// 也不直接渲染终端输出，方便后续被测试、GUI 或服务化入口复用。
pub fn run_sync(config: SyncConfig) -> Result<SyncSummary> {
    let endpoints = SyncEndpoints::local(config.source.clone(), config.target.clone());
    run_sync_with_endpoints_progress(config, endpoints, false)
}

/// 执行一次单向目录同步，并在调用方确认适合时启用交互式进度条。
///
/// 该入口供 CLI 在 text + TTY 环境下调用；库调用者默认使用 `run_sync` 即可
/// 保持无终端副作用。进度条只观察阶段进展，不改变同步计划或执行语义。
pub fn run_sync_with_progress(config: SyncConfig, progress: bool) -> Result<SyncSummary> {
    let endpoints = SyncEndpoints::local(config.source.clone(), config.target.clone());
    run_sync_with_endpoints_progress(config, endpoints, progress)
}

/// 使用给定端点执行一次单向目录同步。
///
/// 当前公开 CLI 会传入本地端点；该入口为后续远端端点接入保留稳定编排层。
pub fn run_sync_with_endpoints(
    config: SyncConfig,
    endpoints: SyncEndpoints,
) -> Result<SyncSummary> {
    run_sync_with_endpoints_progress(config, endpoints, false)
}

/// 使用给定端点执行一次单向目录同步，并可启用交互式进度条。
///
/// 该函数保留端点注入能力，同时让 CLI 能把 TTY 判定结果传入库层。非 CLI
/// 调用者通常应使用无进度的 `run_sync_with_endpoints`。
pub fn run_sync_with_endpoints_progress(
    config: SyncConfig,
    endpoints: SyncEndpoints,
    show_progress: bool,
) -> Result<SyncSummary> {
    let started = Instant::now();
    let progress = SyncProgress::new(show_progress);
    info!(
        source = %endpoints.source().root().display(),
        target = %endpoints.target().root().display(),
        "{}",
        crate::i18n::tr_current("log.scan_started")
    );

    let phase_started = Instant::now();
    let scan_source_progress = progress.spinner("progress.scan_source");
    let source_snapshot = endpoints.scan_source_filtered(config.follow_symlinks, &config.filter)?;
    scan_source_progress.finish();
    info!(
        phase = "scan_source",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        crate::i18n::tr_current("log.phase_finished")
    );
    let phase_started = Instant::now();
    let scan_target_progress = progress.spinner("progress.scan_target");
    let target_snapshot = endpoints.scan_target_filtered(config.follow_symlinks, &config.filter)?;
    scan_target_progress.finish();
    info!(
        phase = "scan_target",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        crate::i18n::tr_current("log.phase_finished")
    );

    info!(
        source_entries = source_snapshot.entries.len(),
        target_entries = target_snapshot.entries.len(),
        "{}",
        crate::i18n::tr_current("log.scan_finished")
    );

    let phase_started = Instant::now();
    let plan_progress = progress.bar("progress.build_plan", source_snapshot.entries.len());
    let plan = build_plan_with_progress(
        &config,
        &endpoints,
        &source_snapshot,
        &target_snapshot,
        &plan_progress,
    )?;
    plan_progress.finish();
    info!(
        phase = "build_plan",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        crate::i18n::tr_current("log.phase_finished")
    );
    info!(
        operations = plan.operations.len(),
        bytes = plan.bytes_to_copy,
        "{}",
        crate::i18n::tr_current("log.plan_built")
    );

    let phase_started = Instant::now();
    let execute_progress = progress.bar("progress.execute_plan", plan.operations.len());
    let mut summary = execute_plan_with_progress(&config, &endpoints, &plan, &execute_progress)?;
    execute_progress.finish();
    info!(
        phase = "execute_plan",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        crate::i18n::tr_current("log.phase_finished")
    );
    summary.source = endpoints.source().root().to_path_buf();
    summary.target = endpoints.target().root().to_path_buf();
    summary.source_entries = source_snapshot.entries.len();
    summary.target_entries = target_snapshot.entries.len();
    summary.planned_operations = plan.operations.len();
    summary.bytes_planned = plan.bytes_to_copy;
    summary.blake3_compared_files = plan.blake3_compared_files;

    if !config.dry_run && config.verify_mode.verify_all_files() {
        let phase_started = Instant::now();
        let verify_progress = progress.spinner("progress.verify_all");
        let verified = verify_all_source_files_with_endpoints(&source_snapshot, &endpoints)?;
        verify_progress.finish();
        info!(
            phase = "verify_all",
            elapsed_ms = phase_started.elapsed().as_millis(),
            "{}",
            crate::i18n::tr_current("log.phase_finished")
        );
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
    fn exclude_filter_protects_matching_target_paths_from_delete()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("keep.txt"), "keep")?;
        fs::write(target.path().join("stale.txt"), "stale")?;
        fs::create_dir(target.path().join("cache"))?;
        fs::write(target.path().join("cache").join("local.bin"), "local")?;
        let rules = source.path().join("rules.txt");
        fs::write(&rules, "cache/\n")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.delete = true;
        cli.filter = Some(crate::filter::FilterConfig {
            mode: crate::filter::FilterMode::Exclude,
            path: rules,
        });
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(summary.deleted_files, 1);
        assert!(!target.path().join("stale.txt").exists());
        assert_eq!(
            fs::read_to_string(target.path().join("cache").join("local.bin"))?,
            "local"
        );
        Ok(())
    }

    #[test]
    fn include_filter_limits_delete_to_whitelisted_scope()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("keep.txt"), "keep")?;
        fs::write(target.path().join("keep.txt"), "old")?;
        fs::write(target.path().join("outside.txt"), "outside")?;
        let rules = source.path().join("rules.txt");
        fs::write(&rules, "keep.txt\n")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.delete = true;
        cli.filter = Some(crate::filter::FilterConfig {
            mode: crate::filter::FilterMode::Include,
            path: rules,
        });
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(summary.deleted_files, 0);
        assert_eq!(fs::read_to_string(target.path().join("keep.txt"))?, "keep");
        assert_eq!(
            fs::read_to_string(target.path().join("outside.txt"))?,
            "outside"
        );
        Ok(())
    }

    #[test]
    fn fast_compare_hashes_same_size_file_when_metadata_differs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let source_file = source.path().join("a.txt");
        let target_file = target.path().join("a.txt");
        fs::write(&source_file, "new-value")?;
        fs::write(&target_file, "old-value")?;

        let source_timestamp = filetime::FileTime::from_unix_time(1_700_000_100, 0);
        let target_timestamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source_file, source_timestamp)?;
        filetime::set_file_mtime(&target_file, target_timestamp)?;

        let cli = Cli::for_test(source.path(), target.path());
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(target_file)?, "new-value");
        assert_eq!(summary.copied_files, 1);
        assert_eq!(summary.blake3_compared_files, 1);
        assert_eq!(summary.verified_files, 1);
        Ok(())
    }

    #[test]
    fn strict_compare_syncs_metadata_when_content_matches()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let source_file = source.path().join("a.txt");
        let target_file = target.path().join("a.txt");
        fs::write(&source_file, "same-value")?;
        fs::write(&target_file, "same-value")?;

        let source_timestamp = filetime::FileTime::from_unix_time(1_700_000_100, 0);
        let target_timestamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source_file, source_timestamp)?;
        filetime::set_file_mtime(&target_file, target_timestamp)?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.compare = crate::config::CompareMode::Strict;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(&target_file)?, "same-value");
        assert_eq!(summary.copied_files, 0);
        assert_eq!(summary.metadata_updates, 1);
        assert_eq!(summary.blake3_compared_files, 1);
        assert_eq!(summary.verified_files, 0);
        assert_eq!(
            fs::metadata(&target_file)?.modified()?,
            fs::metadata(&source_file)?.modified()?
        );
        Ok(())
    }

    #[test]
    fn disabled_metadata_sync_skips_metadata_update()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let source_file = source.path().join("a.txt");
        let target_file = target.path().join("a.txt");
        fs::write(&source_file, "same-value")?;
        fs::write(&target_file, "same-value")?;

        let source_timestamp = filetime::FileTime::from_unix_time(1_700_000_100, 0);
        let target_timestamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source_file, source_timestamp)?;
        filetime::set_file_mtime(&target_file, target_timestamp)?;
        let target_modified_before = fs::metadata(&target_file)?.modified()?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.sync_metadata = false;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(summary.copied_files, 0);
        assert_eq!(summary.metadata_updates, 0);
        assert_eq!(summary.blake3_compared_files, 1);
        assert_eq!(
            fs::metadata(&target_file)?.modified()?,
            target_modified_before
        );
        Ok(())
    }

    #[test]
    fn strict_shortcut_hashes_when_metadata_matches()
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
        cli.strict = true;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(target_file)?, "new-value");
        assert_eq!(summary.copied_files, 1);
        assert_eq!(summary.blake3_compared_files, 1);
        Ok(())
    }

    #[test]
    fn fast_compare_trusts_same_metadata() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let source_file = source.path().join("a.txt");
        let target_file = target.path().join("a.txt");
        fs::write(&source_file, "new-value")?;
        fs::write(&target_file, "old-value")?;

        let timestamp = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source_file, timestamp)?;
        filetime::set_file_mtime(&target_file, timestamp)?;

        let cli = Cli::for_test(source.path(), target.path());
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(target_file)?, "old-value");
        assert_eq!(summary.copied_files, 0);
        assert_eq!(summary.blake3_compared_files, 0);
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

    #[test]
    fn sync_creates_missing_target_root_and_nested_directories()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target_parent = tempdir()?;
        let target = target_parent.path().join("missing-target");
        fs::create_dir(source.path().join("nested"))?;
        fs::write(source.path().join("nested").join("a.txt"), "hello")?;

        let cli = Cli::for_test(source.path(), &target);
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(
            fs::read_to_string(target.join("nested").join("a.txt"))?,
            "hello"
        );
        assert_eq!(summary.created_dirs, 1);
        assert_eq!(summary.copied_files, 1);
        Ok(())
    }

    #[test]
    fn delete_flag_removes_obsolete_nested_directory_tree()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let stale_dir = target.path().join("stale");
        fs::create_dir(&stale_dir)?;
        fs::write(stale_dir.join("old.txt"), "old")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.delete = true;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert!(!stale_dir.exists());
        assert_eq!(summary.deleted_files, 1);
        assert_eq!(summary.deleted_dirs, 1);
        Ok(())
    }

    #[test]
    fn delete_disabled_preserves_obsolete_target_file()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        let stale_file = target.path().join("stale.txt");
        fs::write(&stale_file, "stale")?;

        let cli = Cli::for_test(source.path(), target.path());
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(fs::read_to_string(stale_file)?, "stale");
        assert_eq!(summary.deleted_files, 0);
        assert_eq!(summary.planned_operations, 0);
        Ok(())
    }

    #[test]
    fn verify_all_counts_all_source_files_after_sync()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("a.txt"), "alpha")?;
        fs::write(source.path().join("b.txt"), "beta")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.verify = crate::config::VerifyMode::All;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(summary.copied_files, 2);
        assert_eq!(summary.verified_files, 2);
        Ok(())
    }

    #[test]
    fn path_type_conflict_errors_when_source_file_matches_target_directory()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("item"), "file")?;
        fs::create_dir(target.path().join("item"))?;

        let cli = Cli::for_test(source.path(), target.path());
        let config = SyncConfig::try_from(cli)?;

        let error = crate::run_sync(config).expect_err("type conflict should fail");

        assert!(error.to_string().contains("item"));
        Ok(())
    }

    #[test]
    fn no_atomic_write_overwrites_changed_file()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let source = tempdir()?;
        let target = tempdir()?;
        fs::write(source.path().join("a.txt"), "new-content")?;
        fs::write(target.path().join("a.txt"), "old")?;

        let mut cli = Cli::for_test(source.path(), target.path());
        cli.atomic_write = false;
        let config = SyncConfig::try_from(cli)?;

        let summary = crate::run_sync(config)?;

        assert_eq!(
            fs::read_to_string(target.path().join("a.txt"))?,
            "new-content"
        );
        assert_eq!(summary.copied_files, 1);
        Ok(())
    }
}
