use std::process::{Command, Output};

fn fastsync(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fastsync"))
        .args(args)
        .output()
        .expect("fastsync binary should run")
}

#[test]
fn no_arguments_prints_local_sync_help() {
    let output = fastsync(&[]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("A fast, reliable one-way directory synchronization tool"));
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("Use `fastsync share --help` or `fastsync connect --help`"));
}

#[test]
fn empty_share_subcommand_prints_share_help_without_starting_server() {
    let output = fastsync(&["share"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("fastsync share"));
    assert!(stdout.contains("DIRECTORY"));
}

#[test]
fn json_output_reports_successful_sync() -> Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::write(source.path().join("a.txt"), "hello")?;

    let output = fastsync(&[
        source.path().to_str().expect("temp path should be UTF-8"),
        target.path().to_str().expect("temp path should be UTF-8"),
        "--verify",
        "none",
        "--output",
        "json",
    ]);

    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(target.path().join("a.txt"))?,
        "hello"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(json["copied_files"], 1);
    assert_eq!(json["errors"], 0);
    assert_eq!(json["dry_run"], false);
    Ok(())
}

#[test]
fn invalid_source_exits_with_configuration_error() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let missing = root.path().join("missing");
    let target = root.path().join("target");

    let output = fastsync(&[
        missing.to_str().expect("temp path should be UTF-8"),
        target.to_str().expect("temp path should be UTF-8"),
    ]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("source path does not exist or is not a directory"));
    Ok(())
}
