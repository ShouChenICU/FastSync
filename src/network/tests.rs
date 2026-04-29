use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use quinn::Endpoint;

use crate::i18n::Language;

use super::{
    MAX_MESSAGE_SIZE, NetworkCommand, PROTOCOL_VERSION, ShareMode, SyncDirection,
    cli::validate_pairing_code,
    protocol::{FileManifest, FileTransfer, Manifest, TransferOptions, WireMessage, WireMetadata},
    protocol_io::{read_message, write_message},
    session::install_crypto_provider,
    summary::{NetworkSide, NetworkSummary, ReceiveSummary},
    transfer::*,
    util::*,
};

struct ConnectedPair {
    _server_endpoint: Endpoint,
    _client_endpoint: Endpoint,
    server: quinn::Connection,
    client: quinn::Connection,
}

async fn connected_pair() -> std::result::Result<ConnectedPair, Box<dyn std::error::Error>> {
    install_crypto_provider();
    let server_endpoint = Endpoint::server(server_config()?, "127.0.0.1:0".parse()?)?;
    let server_addr = server_endpoint.local_addr()?;
    let mut client_endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
    client_endpoint.set_default_client_config(insecure_client_config());

    let server_accept_endpoint = server_endpoint.clone();
    let server_task = tokio::spawn(async move {
        let incoming = server_accept_endpoint
            .accept()
            .await
            .ok_or_else(|| std::io::Error::other("server endpoint closed before accepting"))?;
        incoming.await.map_err(std::io::Error::other)
    });
    let client = client_endpoint
        .connect(server_addr, "fastsync.local")?
        .await?;
    let server = server_task.await??;

    Ok(ConnectedPair {
        _server_endpoint: server_endpoint,
        _client_endpoint: client_endpoint,
        server,
        client,
    })
}

fn default_transfer_options() -> TransferOptions {
    TransferOptions {
        delete: false,
        strict: false,
        preserve_times: true,
        preserve_permissions: false,
        file_concurrency: 4,
    }
}

#[test]
fn send_mode_only_allows_pull() {
    assert!(ShareMode::Send.allows(SyncDirection::Pull));
    assert!(!ShareMode::Send.allows(SyncDirection::Push));
}

#[test]
fn parallel_file_stream_protocol_version_is_current() {
    assert_eq!(PROTOCOL_VERSION, 7);
}

#[test]
fn safe_join_rejects_escape_paths() {
    assert!(safe_join(Path::new("/tmp/root"), Path::new("../x")).is_err());
    assert!(safe_join(Path::new("/tmp/root"), Path::new("/x")).is_err());
}

#[test]
fn wire_paths_serialize_with_forward_slashes() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let file = FileManifest {
        path: PathBuf::from("mc").join("Aaron.flac"),
        len: 0,
        metadata: WireMetadata {
            modified_secs: None,
            modified_nanos: None,
            readonly: false,
            unix_mode: None,
        },
    };
    let request = WireMessage::RequestFile {
        path: PathBuf::from("mc").join("Aaron.flac"),
    };

    let file_json = serde_json::to_string(&file)?;
    let request_json = serde_json::to_string(&request)?;

    assert!(file_json.contains(r#""path":"mc/Aaron.flac""#));
    assert!(request_json.contains(r#""path":"mc/Aaron.flac""#));
    Ok(())
}

#[test]
fn wire_paths_accept_legacy_windows_separators()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let file_json = r#"{
            "path":"mc\\Aaron.flac",
            "len":0,
            "metadata":{
                "modified_secs":null,
                "modified_nanos":null,
                "readonly":false,
                "unix_mode":null
            }
        }"#;
    let request_json = r#"{"type":"request_file","path":"mc\\Aaron.flac"}"#;

    let file = serde_json::from_str::<FileManifest>(file_json)?;
    let request = serde_json::from_str::<WireMessage>(request_json)?;

    assert_eq!(file.path, PathBuf::from("mc").join("Aaron.flac"));
    match request {
        WireMessage::RequestFile { path } => {
            assert_eq!(path, PathBuf::from("mc").join("Aaron.flac"));
        }
        _ => panic!("expected request_file message"),
    }
    Ok(())
}

#[test]
fn wire_paths_reject_escape_paths() {
    let file_json = r#"{
            "path":"../x",
            "len":0,
            "metadata":{
                "modified_secs":null,
                "modified_nanos":null,
                "readonly":false,
                "unix_mode":null
            }
        }"#;

    assert!(serde_json::from_str::<FileManifest>(file_json).is_err());
}

#[test]
fn streaming_manifest_keeps_each_message_small()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let files: Vec<_> = (0..20_000)
        .map(|index| FileManifest {
            path: PathBuf::from("album").join(format!("track-{index:05}.flac")),
            len: 12_345,
            metadata: WireMetadata {
                modified_secs: Some(1_700_000_000),
                modified_nanos: Some(0),
                readonly: false,
                unix_mode: None,
            },
        })
        .collect();
    let old_payload = serde_json::to_vec(&Manifest {
        dirs: Vec::new(),
        files: files.clone(),
    })?;

    let largest_stream_item = files
        .iter()
        .map(|file| serde_json::to_vec(&WireMessage::ManifestFile(file.clone())))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|payload| payload.len())
        .max()
        .expect("test manifest should contain files");

    assert!(old_payload.len() > MAX_MESSAGE_SIZE);
    assert!(largest_stream_item < MAX_MESSAGE_SIZE);
    Ok(())
}

#[test]
fn manifest_files_omit_hash_until_transfer() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let metadata = WireMetadata {
        modified_secs: None,
        modified_nanos: None,
        readonly: false,
        unix_mode: None,
    };
    let manifest_file = FileManifest {
        path: PathBuf::from("song.flac"),
        len: 10,
        metadata: metadata.clone(),
    };
    let transfer = FileTransfer {
        path: PathBuf::from("song.flac"),
        len: 10,
        blake3: "a".repeat(64),
        metadata,
    };

    let manifest_json = serde_json::to_string(&WireMessage::ManifestFile(manifest_file))?;
    let transfer_json = serde_json::to_string(&WireMessage::File(transfer))?;

    assert!(!manifest_json.contains("blake3"));
    assert!(transfer_json.contains("blake3"));
    Ok(())
}

#[test]
fn next_chunk_len_handles_remaining_larger_than_usize() {
    assert_eq!(next_chunk_len(u64::MAX, 1024), 1024);
    assert_eq!(next_chunk_len(512, 1024), 512);
    assert_eq!(next_chunk_len(0, 1024), 0);
}

#[test]
fn share_shortcuts_parse_to_receive_mode() {
    let command = NetworkCommand::parse_from(
        vec![
            OsString::from("fastsync"),
            OsString::from("s"),
            OsString::from("/tmp/inbox"),
            OsString::from("-r"),
            OsString::from("-a"),
            OsString::from("-c"),
            OsString::from("123456"),
            OsString::from("-f"),
            OsString::from("2"),
        ],
        Language::DEFAULT,
    );

    let NetworkCommand::Share(config) = command else {
        panic!("expected share command");
    };

    assert_eq!(config.mode, ShareMode::Receive);
    assert!(config.allow_delete);
    assert_eq!(config.code.as_deref(), Some("123456"));
    assert_eq!(config.max_failures, 2);
}

#[test]
fn connect_shortcuts_parse_to_push_with_delete() {
    let command = NetworkCommand::parse_from(
        vec![
            OsString::from("fastsync"),
            OsString::from("c"),
            OsString::from("example.com"),
            OsString::from("/tmp/project"),
            OsString::from("-u"),
            OsString::from("-d"),
            OsString::from("--strict"),
            OsString::from("-p"),
            OsString::from("--network-concurrency"),
            OsString::from("8"),
            OsString::from("-c"),
            OsString::from("123456"),
        ],
        Language::DEFAULT,
    );

    let NetworkCommand::Connect(config) = command else {
        panic!("expected connect command");
    };

    assert_eq!(config.direction, SyncDirection::Push);
    assert!(config.delete);
    assert!(config.strict);
    assert!(config.preserve_permissions);
    assert_eq!(config.network_concurrency, 8);
    assert_eq!(config.code.as_deref(), Some("123456"));
}

#[test]
fn short_mode_values_are_accepted() {
    let command = NetworkCommand::parse_from(
        vec![
            OsString::from("fastsync"),
            OsString::from("share"),
            OsString::from("/tmp/share"),
            OsString::from("-m"),
            OsString::from("b"),
        ],
        Language::DEFAULT,
    );

    let NetworkCommand::Share(config) = command else {
        panic!("expected share command");
    };

    assert_eq!(config.mode, ShareMode::Both);
}

#[test]
fn generated_pairing_code_is_six_digits() {
    let code = generate_pairing_code();

    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
}

#[test]
fn pairing_code_validation_rejects_old_dash_format() {
    assert!(validate_pairing_code("123456").is_ok());
    assert!(validate_pairing_code("123-456").is_err());
    assert!(validate_pairing_code("12345").is_err());
    assert!(validate_pairing_code("abcdef").is_err());
}

#[test]
fn network_summary_supports_chinese_labels() {
    let summary = NetworkSummary {
        side: NetworkSide::Connect,
        direction: SyncDirection::Push,
        directory: Path::new("/tmp/project"),
        remote: "127.0.0.1:7443",
        files: 2,
        bytes: 2048,
        deleted: 1,
        file_concurrency: 4,
        elapsed_ms: 1200,
    };

    let text = summary.to_text(Language::ZhCn);

    assert!(text.contains("网络同步完成"));
    assert!(text.contains("连接方"));
    assert!(text.contains("上传"));
    assert!(text.contains("文件并发"));
    assert!(text.contains("2.0 KiB"));
}

#[test]
fn request_files_skips_same_content_after_local_hash()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::write(source.path().join("same.txt"), "same content")?;
    std::fs::write(target.path().join("same.txt"), "same content")?;
    let manifest = build_manifest(source.path())?;
    let remote_hashes = manifest_hashes(source.path(), &manifest)?;

    let requested = request_files_for_local_state(target.path(), &manifest, false, &remote_hashes)?;

    assert!(requested.is_empty());
    Ok(())
}

#[test]
fn request_files_includes_missing_and_changed_files()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::write(source.path().join("changed.txt"), "source")?;
    std::fs::write(source.path().join("missing.txt"), "missing")?;
    let changed_target = target.path().join("changed.txt");
    std::fs::write(&changed_target, "target")?;
    filetime::set_file_mtime(&changed_target, filetime::FileTime::from_unix_time(1, 0))?;
    let manifest = build_manifest(source.path())?;
    let remote_hashes = manifest_hashes(source.path(), &manifest)?;

    let requested = request_files_for_local_state(target.path(), &manifest, false, &remote_hashes)?;

    assert!(requested.contains(&PathBuf::from("changed.txt")));
    assert!(requested.contains(&PathBuf::from("missing.txt")));
    assert_eq!(requested.len(), 2);
    Ok(())
}

#[test]
fn strict_request_files_hashes_even_when_metadata_matches()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::write(source.path().join("same-meta.txt"), "aaaa")?;
    let manifest = build_manifest(source.path())?;
    let remote_hashes = manifest_hashes(source.path(), &manifest)?;
    let file = manifest
        .files
        .iter()
        .find(|file| file.path == Path::new("same-meta.txt"))
        .expect("manifest should contain source file");
    let target_file = target.path().join("same-meta.txt");
    std::fs::write(&target_file, "bbbb")?;
    if let Some(mtime) = file.metadata.modified_filetime() {
        filetime::set_file_mtime(&target_file, mtime)?;
    }
    #[cfg(unix)]
    if let Some(mode) = file.metadata.unix_mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target_file, std::fs::Permissions::from_mode(mode))?;
    }

    let fast_requested =
        request_files_for_local_state(target.path(), &manifest, false, &remote_hashes)?;
    let strict_requested =
        request_files_for_local_state(target.path(), &manifest, true, &remote_hashes)?;

    assert!(fast_requested.is_empty());
    assert_eq!(strict_requested, vec![PathBuf::from("same-meta.txt")]);
    Ok(())
}

#[tokio::test]
#[ignore = "opens local UDP sockets"]
async fn network_hash_requests_are_batched_before_responses()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::write(source.path().join("alpha.txt"), "aaaa")?;
    std::fs::write(source.path().join("beta.txt"), "bbbb")?;
    std::fs::write(target.path().join("alpha.txt"), "aaaa")?;
    std::fs::write(target.path().join("beta.txt"), "cccc")?;
    for name in ["alpha.txt", "beta.txt"] {
        filetime::set_file_mtime(
            target.path().join(name),
            filetime::FileTime::from_unix_time(1, 0),
        )?;
    }

    let manifest = build_manifest(source.path())?;
    let remote_hashes = manifest_hashes(source.path(), &manifest)?;
    let ConnectedPair {
        _server_endpoint,
        _client_endpoint,
        server,
        client,
    } = connected_pair().await?;

    let server_task = tokio::spawn(async move {
        let (mut server_send, mut server_recv) = server
            .accept_bi()
            .await
            .map_err(|error| other("test accept control stream", error))?;
        let mut requested_hash_paths = Vec::new();
        for _ in 0..2 {
            let message =
                tokio::time::timeout(Duration::from_secs(1), read_message(&mut server_recv))
                    .await
                    .map_err(|error| other("test wait for batched hash request", error))??;
            match message {
                WireMessage::HashRequest { path } => requested_hash_paths.push(path),
                _ => return Err(other_message("test hash pipeline", "expected hash request")),
            }
        }

        for path in &requested_hash_paths {
            write_message(
                &mut server_send,
                &WireMessage::Hash {
                    path: path.clone(),
                    blake3: remote_hashes
                        .get(path)
                        .expect("hash request path should exist")
                        .clone(),
                },
            )
            .await?;
        }

        match read_message(&mut server_recv).await? {
            WireMessage::HashRequestEnd => {}
            _ => {
                return Err(other_message(
                    "test hash pipeline",
                    "expected hash request end",
                ));
            }
        }

        read_requested_paths(&mut server_recv).await
    });

    let (mut client_send, mut client_recv) = client.open_bi().await?;
    let requested = send_file_requests(
        target.path(),
        &manifest,
        false,
        &mut client_send,
        &mut client_recv,
    )
    .await?;
    let server_requested = server_task.await??;

    assert_eq!(requested, vec![PathBuf::from("beta.txt")]);
    assert!(server_requested.contains(&PathBuf::from("beta.txt")));
    assert_eq!(server_requested.len(), 1);
    Ok(())
}

#[test]
fn delete_obsolete_removes_files_and_nested_directories()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let root = tempfile::tempdir()?;
    let stale_dir = root.path().join("stale");
    std::fs::create_dir(&stale_dir)?;
    std::fs::write(stale_dir.join("old.txt"), "old")?;
    std::fs::write(root.path().join("stale.txt"), "old")?;
    std::fs::write(root.path().join("keep.txt"), "keep")?;
    let manifest = Manifest {
        dirs: Vec::new(),
        files: vec![FileManifest {
            path: PathBuf::from("keep.txt"),
            len: 4,
            metadata: WireMetadata::from_entry(
                crate::scan::scan_directory(root.path(), false)?
                    .get(Path::new("keep.txt"))
                    .expect("keep.txt should be scanned"),
            ),
        }],
    };

    let deleted = runtime.block_on(delete_obsolete(root.path(), &manifest))?;

    assert_eq!(deleted, 3);
    assert!(!stale_dir.exists());
    assert!(!root.path().join("stale.txt").exists());
    assert!(root.path().join("keep.txt").exists());
    Ok(())
}

#[test]
fn skipped_network_file_still_receives_metadata()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    let source_file = source.path().join("same.txt");
    let target_file = target.path().join("same.txt");
    std::fs::write(&source_file, "same content")?;
    std::fs::write(&target_file, "same content")?;
    let source_time = filetime::FileTime::from_unix_time(123, 0);
    let target_time = filetime::FileTime::from_unix_time(456, 0);
    filetime::set_file_mtime(&source_file, source_time)?;
    filetime::set_file_mtime(&target_file, target_time)?;
    let manifest = build_manifest(source.path())?;
    let remote_hashes = manifest_hashes(source.path(), &manifest)?;
    let requested = request_files_for_local_state(target.path(), &manifest, false, &remote_hashes)?;

    apply_file_metadata(
        target.path(),
        &manifest.files,
        TransferOptions {
            delete: false,
            strict: false,
            preserve_times: true,
            preserve_permissions: false,
            file_concurrency: 4,
        },
    )?;

    let updated_time =
        filetime::FileTime::from_last_modification_time(&std::fs::metadata(&target_file)?);
    assert!(requested.is_empty());
    assert_eq!(updated_time, source_time);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opens local UDP sockets"]
async fn network_pull_transfers_requested_files_over_parallel_streams()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::create_dir(source.path().join("nested"))?;
    for index in 0..8 {
        std::fs::write(
            source.path().join(format!("file-{index}.txt")),
            format!("content-{index}"),
        )?;
    }
    std::fs::write(source.path().join("nested").join("deep.txt"), "deep")?;
    let pair = connected_pair().await?;
    let sender = async {
        let (mut server_send, mut server_recv) = pair.server.open_bi().await?;
        send_tree(
            source.path(),
            &pair.server,
            &mut server_send,
            &mut server_recv,
            default_transfer_options(),
        )
        .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let receiver = async {
        let (mut client_send, mut client_recv) = pair.client.accept_bi().await?;
        let summary = receive_tree(
            target.path(),
            &pair.client,
            &mut client_recv,
            &mut client_send,
            default_transfer_options(),
        )
        .await?;
        Ok::<ReceiveSummary, Box<dyn std::error::Error>>(summary)
    };

    let (_, summary) = tokio::try_join!(sender, receiver)?;

    assert_eq!(summary.files, 9);
    assert_eq!(
        std::fs::read_to_string(target.path().join("nested").join("deep.txt"))?,
        "deep"
    );
    for index in 0..8 {
        assert_eq!(
            std::fs::read_to_string(target.path().join(format!("file-{index}.txt")))?,
            format!("content-{index}")
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opens local UDP sockets"]
async fn network_push_skips_matching_files_and_transfers_only_requested()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = tempfile::tempdir()?;
    let target = tempfile::tempdir()?;
    std::fs::write(source.path().join("same.txt"), "same content")?;
    std::fs::write(source.path().join("changed.txt"), "changed new content")?;
    std::fs::write(source.path().join("missing.txt"), "missing content")?;
    std::fs::write(target.path().join("same.txt"), "same content")?;
    std::fs::write(target.path().join("changed.txt"), "old content")?;
    let pair = connected_pair().await?;
    let sender = async {
        let (mut client_send, mut client_recv) = pair.client.open_bi().await?;
        send_tree(
            source.path(),
            &pair.client,
            &mut client_send,
            &mut client_recv,
            default_transfer_options(),
        )
        .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let receiver = async {
        let (mut server_send, mut server_recv) = pair.server.accept_bi().await?;
        let summary = receive_tree(
            target.path(),
            &pair.server,
            &mut server_recv,
            &mut server_send,
            default_transfer_options(),
        )
        .await?;
        Ok::<ReceiveSummary, Box<dyn std::error::Error>>(summary)
    };

    let (_, summary) = tokio::try_join!(sender, receiver)?;

    assert_eq!(summary.files, 2);
    assert_eq!(
        std::fs::read_to_string(target.path().join("same.txt"))?,
        "same content"
    );
    assert_eq!(
        std::fs::read_to_string(target.path().join("changed.txt"))?,
        "changed new content"
    );
    assert_eq!(
        std::fs::read_to_string(target.path().join("missing.txt"))?,
        "missing content"
    );
    Ok(())
}

#[test]
fn network_file_path_rejects_existing_directory_without_delete()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let root = tempfile::tempdir()?;
    let path = root.path().join("item");
    std::fs::create_dir(&path)?;

    let error = runtime
        .block_on(ensure_file_path(&path, false))
        .expect_err("directory/file conflict should fail without delete");

    assert!(error.to_string().contains("exists and is a directory"));
    Ok(())
}

#[test]
fn network_directory_path_replaces_file_when_delete_enabled()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let root = tempfile::tempdir()?;
    let path = root.path().join("item");
    std::fs::write(&path, "file")?;

    runtime.block_on(ensure_directory_path(&path, true))?;
    std::fs::create_dir(&path)?;

    assert!(path.is_dir());
    Ok(())
}
