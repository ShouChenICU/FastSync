use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use quinn::{RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tracing::info;

use crate::error::{FastSyncError, Result};
use crate::i18n::tr_current;

use super::{
    BUFFER_SIZE,
    protocol::{
        DirManifest, FileManifest, FileTransfer, Manifest, TransferOptions, WireMessage,
        WireMetadata,
    },
    protocol_io::{read_message, write_message},
    summary::ReceiveSummary,
    util::{
        hex_digest, io_path, other, other_message, safe_join, throughput_bps, unique_temp_path,
    },
};

pub(super) async fn send_tree(
    root: &Path,
    connection: &quinn::Connection,
    send: &mut SendStream,
    recv: &mut RecvStream,
    options: TransferOptions,
) -> Result<()> {
    let phase_started = Instant::now();
    let manifest = send_manifest(root, send).await?;
    info!(
        phase = "network_send_manifest",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        tr_current("log.phase_finished")
    );
    let phase_started = Instant::now();
    let hashes = serve_hash_requests(root, &manifest, send, recv).await?;
    info!(
        phase = "network_serve_hash_requests",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        tr_current("log.phase_finished")
    );
    let mut requested = read_requested_paths(recv).await?;

    let mut transfers = Vec::new();
    for file in &manifest.files {
        if !requested.remove(&file.path) {
            continue;
        }
        info!(
            path = %file.path.display(),
            bytes = file.len,
            "sending file"
        );
        let transfer = file_transfer(root, file, hashes.get(&file.path).cloned())?;
        transfers.push(transfer);
    }
    if !requested.is_empty() {
        return Err(other_message(
            "send requested network files",
            "peer requested paths outside the manifest",
        ));
    }

    let phase_started = Instant::now();
    let (sent_files, total_bytes) =
        send_files(root, connection, transfers, options.file_concurrency).await?;
    info!(
        phase = "network_send_files",
        elapsed_ms = phase_started.elapsed().as_millis(),
        files = sent_files,
        bytes = total_bytes,
        file_concurrency = options.file_concurrency,
        throughput_bps = throughput_bps(total_bytes, phase_started.elapsed().as_millis()),
        "{}",
        tr_current("log.phase_finished")
    );
    write_message(send, &WireMessage::Done).await?;
    send.finish()
        .map_err(|error| other("finish QUIC send stream", error))?;
    info!(
        files = sent_files,
        bytes = total_bytes,
        "finished sending tree"
    );
    Ok(())
}

pub(super) async fn send_manifest(root: &Path, send: &mut SendStream) -> Result<Manifest> {
    let snapshot = crate::scan::scan_directory(root, false)?;
    let mut manifest = Manifest {
        dirs: Vec::new(),
        files: Vec::new(),
    };

    write_message(send, &WireMessage::ManifestStart).await?;
    for entry in snapshot.entries.values() {
        match entry.kind {
            crate::scan::EntryKind::Directory => {
                let dir = DirManifest {
                    path: entry.relative_path.clone(),
                    metadata: WireMetadata::from_entry(entry),
                };
                write_message(send, &WireMessage::ManifestDir(dir.clone())).await?;
                manifest.dirs.push(dir);
            }
            crate::scan::EntryKind::File => {
                let file = FileManifest {
                    path: entry.relative_path.clone(),
                    len: entry.len,
                    metadata: WireMetadata::from_entry(entry),
                };
                write_message(send, &WireMessage::ManifestFile(file.clone())).await?;
                manifest.files.push(file);
            }
            crate::scan::EntryKind::Symlink => {}
        }
    }
    write_message(send, &WireMessage::ManifestEnd).await?;

    info!(
        root = %root.display(),
        dirs = manifest.dirs.len(),
        files = manifest.files.len(),
        bytes = manifest.files.iter().map(|file| file.len).sum::<u64>(),
        "sent manifest"
    );
    Ok(manifest)
}

pub(super) async fn serve_hash_requests(
    root: &Path,
    manifest: &Manifest,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<HashMap<PathBuf, String>> {
    let manifest_paths: HashSet<_> = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let mut hashes = HashMap::new();

    loop {
        match read_message(recv).await? {
            WireMessage::HashRequest { path } => {
                if !manifest_paths.contains(&path) {
                    return Err(other_message(
                        "serve network hash request",
                        "peer requested hash outside the manifest",
                    ));
                }
                let digest = hash_manifest_path(root, &path)?;
                write_message(
                    send,
                    &WireMessage::Hash {
                        path: path.clone(),
                        blake3: digest.clone(),
                    },
                )
                .await?;
                hashes.insert(path, digest);
            }
            WireMessage::HashRequestEnd => break,
            _ => {
                return Err(other_message(
                    "serve network hash requests",
                    "unexpected message",
                ));
            }
        }
    }

    Ok(hashes)
}

pub(super) async fn read_requested_paths(recv: &mut RecvStream) -> Result<HashSet<PathBuf>> {
    let mut requested = HashSet::new();
    loop {
        match read_message(recv).await? {
            WireMessage::RequestFile { path } => {
                requested.insert(path);
            }
            WireMessage::RequestEnd => break,
            _ => {
                return Err(other_message(
                    "read requested network files",
                    "unexpected message",
                ));
            }
        }
    }
    Ok(requested)
}

pub(super) async fn receive_tree(
    root: &Path,
    connection: &quinn::Connection,
    recv: &mut RecvStream,
    send: &mut SendStream,
    options: TransferOptions,
) -> Result<ReceiveSummary> {
    let phase_started = Instant::now();
    let manifest = receive_manifest(root, recv, options).await?;
    info!(
        phase = "network_receive_manifest",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        tr_current("log.phase_finished")
    );
    info!(
        root = %root.display(),
        dirs = manifest.dirs.len(),
        files = manifest.files.len(),
        bytes = manifest.files.iter().map(|file| file.len).sum::<u64>(),
        "receiving manifest"
    );
    let phase_started = Instant::now();
    let requested_files = send_file_requests(root, &manifest, options.strict, send, recv).await?;
    info!(
        requested_files = requested_files.len(),
        skipped_files = manifest.files.len().saturating_sub(requested_files.len()),
        strict = options.strict,
        elapsed_ms = phase_started.elapsed().as_millis(),
        "planned network file requests"
    );

    let phase_started = Instant::now();
    let (files, bytes) =
        receive_requested_files(root, connection, requested_files, options).await?;
    info!(
        phase = "network_receive_files",
        elapsed_ms = phase_started.elapsed().as_millis(),
        files,
        bytes,
        file_concurrency = options.file_concurrency,
        throughput_bps = throughput_bps(bytes, phase_started.elapsed().as_millis()),
        "{}",
        tr_current("log.phase_finished")
    );
    match read_message(recv).await? {
        WireMessage::Done => {}
        _ => return Err(other_message("receive tree", "unexpected message")),
    }
    let deleted = if options.delete {
        let phase_started = Instant::now();
        let deleted = delete_obsolete(root, &manifest).await?;
        info!(
            phase = "network_delete_obsolete",
            elapsed_ms = phase_started.elapsed().as_millis(),
            deleted,
            "{}",
            tr_current("log.phase_finished")
        );
        deleted
    } else {
        0
    };
    let phase_started = Instant::now();
    apply_file_metadata(root, &manifest.files, options)?;
    apply_directory_metadata(root, &manifest.dirs, options)?;
    info!(
        phase = "network_apply_metadata",
        elapsed_ms = phase_started.elapsed().as_millis(),
        "{}",
        tr_current("log.phase_finished")
    );
    info!(files, bytes, deleted, "finished receiving tree");
    Ok(ReceiveSummary {
        files,
        bytes,
        deleted,
    })
}

pub(super) async fn receive_manifest(
    root: &Path,
    recv: &mut RecvStream,
    options: TransferOptions,
) -> Result<Manifest> {
    match read_message(recv).await? {
        WireMessage::ManifestStart => {}
        _ => return Err(other_message("receive manifest", "unexpected message")),
    }

    tokio::fs::create_dir_all(root)
        .await
        .map_err(|error| io_path("create receive root", root, error))?;

    let mut manifest = Manifest {
        dirs: Vec::new(),
        files: Vec::new(),
    };
    loop {
        match read_message(recv).await? {
            WireMessage::ManifestDir(dir) => {
                let path = safe_join(root, &dir.path)?;
                ensure_directory_path(&path, options.delete).await?;
                tokio::fs::create_dir_all(&path)
                    .await
                    .map_err(|error| io_path("create received directory", &path, error))?;
                manifest.dirs.push(dir);
            }
            WireMessage::ManifestFile(file) => manifest.files.push(file),
            WireMessage::ManifestEnd => break,
            _ => return Err(other_message("receive manifest", "unexpected message")),
        }
    }

    Ok(manifest)
}

pub(super) async fn send_file_requests(
    root: &Path,
    manifest: &Manifest,
    strict: bool,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<Vec<PathBuf>> {
    let target_snapshot = match crate::scan::scan_optional_directory(root, false) {
        Ok(snapshot) => snapshot,
        Err(FastSyncError::InvalidTarget(path)) if path == root => {
            write_message(send, &WireMessage::HashRequestEnd).await?;
            let requested: Vec<_> = manifest
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect();
            send_requested_file_paths(requested.iter().cloned(), send).await?;
            return Ok(requested);
        }
        Err(error) => return Err(error),
    };

    let mut requested = Vec::new();
    for file in &manifest.files {
        match file_request_decision(&target_snapshot, file, strict)? {
            FileRequestDecision::Request => requested.push(file.path.clone()),
            FileRequestDecision::Skip => {}
            FileRequestDecision::CompareHash { local_path } => {
                let local_digest = hex_digest(crate::hash::blake3_file(&local_path)?);
                let remote_digest = request_remote_hash(send, recv, &file.path).await?;
                if local_digest != remote_digest {
                    requested.push(file.path.clone());
                }
            }
        }
    }
    write_message(send, &WireMessage::HashRequestEnd).await?;
    send_requested_file_paths(requested.iter().cloned(), send).await?;
    Ok(requested)
}

pub(super) async fn request_remote_hash(
    send: &mut SendStream,
    recv: &mut RecvStream,
    path: &Path,
) -> Result<String> {
    write_message(
        send,
        &WireMessage::HashRequest {
            path: path.to_path_buf(),
        },
    )
    .await?;

    match read_message(recv).await? {
        WireMessage::Hash {
            path: reply,
            blake3,
        } if reply == path => Ok(blake3),
        WireMessage::Hash { path: reply, .. } => Err(other_message(
            "read network hash response",
            format!(
                "hash response path mismatch: expected {}, got {}",
                path.display(),
                reply.display()
            ),
        )),
        _ => Err(other_message(
            "read network hash response",
            "unexpected message",
        )),
    }
}

pub(super) async fn send_requested_file_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    send: &mut SendStream,
) -> Result<usize> {
    let mut requested = 0_usize;
    for path in paths {
        write_message(send, &WireMessage::RequestFile { path }).await?;
        requested += 1;
    }
    write_message(send, &WireMessage::RequestEnd).await?;
    Ok(requested)
}

#[cfg(test)]
pub(super) fn request_files_for_local_state(
    root: &Path,
    manifest: &Manifest,
    strict: bool,
    remote_hashes: &HashMap<PathBuf, String>,
) -> Result<Vec<PathBuf>> {
    let target_snapshot = match crate::scan::scan_optional_directory(root, false) {
        Ok(snapshot) => snapshot,
        Err(FastSyncError::InvalidTarget(path)) if path == root => {
            return Ok(manifest
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect());
        }
        Err(error) => return Err(error),
    };

    let mut requested = Vec::new();
    for file in &manifest.files {
        match file_request_decision(&target_snapshot, file, strict)? {
            FileRequestDecision::Request => requested.push(file.path.clone()),
            FileRequestDecision::Skip => {}
            FileRequestDecision::CompareHash { local_path } => {
                let local_digest = hex_digest(crate::hash::blake3_file(&local_path)?);
                if remote_hashes.get(&file.path) != Some(&local_digest) {
                    requested.push(file.path.clone());
                }
            }
        }
    }

    Ok(requested)
}

pub(super) enum FileRequestDecision {
    Request,
    Skip,
    CompareHash { local_path: PathBuf },
}

pub(super) fn file_request_decision(
    target_snapshot: &crate::scan::Snapshot,
    file: &FileManifest,
    strict: bool,
) -> Result<FileRequestDecision> {
    let Some(target_entry) = target_snapshot.get(&file.path) else {
        return Ok(FileRequestDecision::Request);
    };

    if !target_entry.is_file() || target_entry.len != file.len {
        return Ok(FileRequestDecision::Request);
    }

    if !strict && content_metadata_matches(target_entry, &file.metadata) {
        Ok(FileRequestDecision::Skip)
    } else {
        Ok(FileRequestDecision::CompareHash {
            local_path: target_entry.absolute_path.clone(),
        })
    }
}

pub(super) fn content_metadata_matches(
    entry: &crate::scan::FileEntry,
    metadata: &WireMetadata,
) -> bool {
    metadata_time_matches(entry, metadata) && metadata_permissions_match(entry, metadata)
}

pub(super) fn metadata_time_matches(
    entry: &crate::scan::FileEntry,
    metadata: &WireMetadata,
) -> bool {
    let Some(source_secs) = metadata.modified_secs else {
        return entry.modified.is_none();
    };
    let Some(source_nanos) = metadata.modified_nanos else {
        return entry.modified.is_none();
    };
    let Some(target_modified) = entry.modified else {
        return false;
    };
    let Ok(target_duration) = target_modified.duration_since(UNIX_EPOCH) else {
        return false;
    };

    target_duration.as_secs() as i64 == source_secs
        && target_duration.subsec_nanos() == source_nanos
}

pub(super) fn metadata_permissions_match(
    entry: &crate::scan::FileEntry,
    metadata: &WireMetadata,
) -> bool {
    if entry.readonly != metadata.readonly {
        return false;
    }

    #[cfg(unix)]
    {
        metadata.unix_mode.is_none_or(|mode| entry.mode == mode)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
pub(super) fn build_manifest(root: &Path) -> Result<Manifest> {
    let snapshot = crate::scan::scan_directory(root, false)?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in snapshot.entries.values() {
        match entry.kind {
            crate::scan::EntryKind::Directory => dirs.push(DirManifest {
                path: entry.relative_path.clone(),
                metadata: WireMetadata::from_entry(entry),
            }),
            crate::scan::EntryKind::File => {
                files.push(FileManifest {
                    path: entry.relative_path.clone(),
                    len: entry.len,
                    metadata: WireMetadata::from_entry(entry),
                });
            }
            crate::scan::EntryKind::Symlink => {}
        }
    }

    Ok(Manifest { dirs, files })
}

#[cfg(test)]
pub(super) fn manifest_hashes(
    root: &Path,
    manifest: &Manifest,
) -> Result<HashMap<PathBuf, String>> {
    let mut hashes = HashMap::new();
    for file in &manifest.files {
        hashes.insert(file.path.clone(), hash_manifest_path(root, &file.path)?);
    }
    Ok(hashes)
}

pub(super) fn file_transfer(
    root: &Path,
    file: &FileManifest,
    cached_hash: Option<String>,
) -> Result<FileTransfer> {
    let blake3 = match cached_hash {
        Some(hash) => hash,
        None => hash_manifest_path(root, &file.path)?,
    };
    Ok(FileTransfer {
        path: file.path.clone(),
        len: file.len,
        blake3,
        metadata: file.metadata.clone(),
    })
}

pub(super) fn hash_manifest_path(root: &Path, path: &Path) -> Result<String> {
    let path = safe_join(root, path)?;
    crate::hash::blake3_file(&path).map(hex_digest)
}

pub(super) async fn send_files(
    root: &Path,
    connection: &quinn::Connection,
    transfers: Vec<FileTransfer>,
    file_concurrency: usize,
) -> Result<(usize, u64)> {
    let mut tasks = JoinSet::new();
    let mut files = 0_usize;
    let mut bytes = 0_u64;

    for transfer in transfers {
        while tasks.len() >= file_concurrency {
            let (sent_files, sent_bytes) = join_file_task(&mut tasks).await?;
            files += sent_files;
            bytes = bytes.saturating_add(sent_bytes);
        }

        let root = root.to_path_buf();
        let connection = connection.clone();
        tasks.spawn(async move {
            let bytes = transfer.len;
            let mut stream = connection
                .open_uni()
                .await
                .map_err(|error| other("open file transfer stream", error))?;
            write_message(&mut stream, &WireMessage::File(transfer.clone())).await?;
            send_file(&root, &transfer, &mut stream).await?;
            finish_file_stream(&mut stream).await?;
            Ok((1_usize, bytes))
        });
    }

    while !tasks.is_empty() {
        let (sent_files, sent_bytes) = join_file_task(&mut tasks).await?;
        files += sent_files;
        bytes = bytes.saturating_add(sent_bytes);
    }

    Ok((files, bytes))
}

pub(super) async fn finish_file_stream(send: &mut SendStream) -> Result<()> {
    send.finish()
        .map_err(|error| other("finish QUIC file stream", error))
}

pub(super) async fn join_file_task(
    tasks: &mut JoinSet<Result<(usize, u64)>>,
) -> Result<(usize, u64)> {
    match tasks.join_next().await {
        Some(Ok(result)) => result,
        Some(Err(error)) => Err(other("join network file transfer task", error)),
        None => Ok((0, 0)),
    }
}

pub(super) async fn send_file(
    root: &Path,
    file: &FileTransfer,
    send: &mut SendStream,
) -> Result<()> {
    let path = safe_join(root, &file.path)?;
    let mut input = tokio::fs::File::open(&path)
        .await
        .map_err(|error| io_path("open file for network send", &path, error))?;
    let mut remaining = file.len;
    let mut buffer = vec![0_u8; BUFFER_SIZE];

    while remaining > 0 {
        let read = input
            .read(&mut buffer)
            .await
            .map_err(|error| io_path("read file for network send", &path, error))?;
        if read == 0 {
            return Err(other_message(
                "send file",
                format!("file ended early: {}", file.path.display()),
            ));
        }
        send.write_all(&buffer[..read])
            .await
            .map_err(|error| other("write file chunk to QUIC stream", error))?;
        remaining = remaining.saturating_sub(read as u64);
    }

    Ok(())
}

pub(super) async fn receive_requested_files(
    root: &Path,
    connection: &quinn::Connection,
    requested_files: Vec<PathBuf>,
    options: TransferOptions,
) -> Result<(usize, u64)> {
    let requested: HashSet<_> = requested_files.into_iter().collect();
    let mut started = HashSet::new();
    let mut tasks = JoinSet::new();
    let mut files = 0_usize;
    let mut bytes = 0_u64;

    for _ in 0..requested.len() {
        while tasks.len() >= options.file_concurrency {
            let (received_files, received_bytes) = join_file_task(&mut tasks).await?;
            files += received_files;
            bytes = bytes.saturating_add(received_bytes);
        }

        let mut stream = connection
            .accept_uni()
            .await
            .map_err(|error| other("accept file transfer stream", error))?;
        let file = match read_message(&mut stream).await? {
            WireMessage::File(file) => file,
            _ => return Err(other_message("receive file stream", "unexpected message")),
        };
        if !requested.contains(&file.path) {
            return Err(other_message(
                "receive file stream",
                format!("unrequested file stream: {}", file.path.display()),
            ));
        }
        if !started.insert(file.path.clone()) {
            return Err(other_message(
                "receive file stream",
                format!("duplicate file stream: {}", file.path.display()),
            ));
        }

        info!(
            path = %file.path.display(),
            bytes = file.len,
            "receiving file"
        );
        let root = root.to_path_buf();
        tasks.spawn(async move {
            let bytes = file.len;
            receive_file(&root, &file, &mut stream, options).await?;
            Ok((1_usize, bytes))
        });
    }

    while !tasks.is_empty() {
        let (received_files, received_bytes) = join_file_task(&mut tasks).await?;
        files += received_files;
        bytes = bytes.saturating_add(received_bytes);
    }

    Ok((files, bytes))
}

pub(super) async fn receive_file(
    root: &Path,
    file: &FileTransfer,
    recv: &mut RecvStream,
    options: TransferOptions,
) -> Result<()> {
    let target = safe_join(root, &file.path)?;
    let Some(parent) = target.parent() else {
        return Err(other_message("receive file", "target path has no parent"));
    };
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| io_path("create received file parent", parent, error))?;
    ensure_file_path(&target, options.delete).await?;
    let temp_path = unique_temp_path(parent);
    let mut output = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|error| io_path("create network temp file", &temp_path, error))?;
    let mut hasher = blake3::Hasher::new();
    let mut remaining = file.len;
    let mut buffer = vec![0_u8; BUFFER_SIZE];

    while remaining > 0 {
        let read_len = next_chunk_len(remaining, buffer.len());
        let Some(read) = recv
            .read(&mut buffer[..read_len])
            .await
            .map_err(|error| other("read file chunk from QUIC stream", error))?
        else {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(other_message(
                "receive file",
                format!(
                    "stream ended before file completed: {}",
                    file.path.display()
                ),
            ));
        };
        output
            .write_all(&buffer[..read])
            .await
            .map_err(|error| io_path("write network temp file", &temp_path, error))?;
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    output
        .flush()
        .await
        .map_err(|error| io_path("flush network temp file", &temp_path, error))?;
    output
        .sync_data()
        .await
        .map_err(|error| io_path("sync network temp file", &temp_path, error))?;
    drop(output);

    let actual = hex_digest(*hasher.finalize().as_bytes());
    if actual != file.blake3 {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(other_message(
            "verify received file",
            format!("BLAKE3 mismatch: {}", file.path.display()),
        ));
    }

    match tokio::fs::rename(&temp_path, &target).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            tokio::fs::remove_file(&target).await.map_err(|error| {
                io_path("remove old target before network replace", &target, error)
            })?;
            tokio::fs::rename(&temp_path, &target)
                .await
                .map_err(|error| io_path("rename network temp file", &target, error))
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            Err(io_path("rename network temp file", &target, error))
        }
    }?;
    apply_path_metadata(&target, &file.metadata, options)
}

pub(super) fn next_chunk_len(remaining: u64, buffer_len: usize) -> usize {
    if remaining > buffer_len as u64 {
        buffer_len
    } else {
        remaining as usize
    }
}

pub(super) async fn ensure_directory_path(path: &Path, delete_enabled: bool) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(metadata) if delete_enabled => {
            if metadata.is_dir() {
                tokio::fs::remove_dir_all(path).await.map_err(|error| {
                    io_path("remove directory before network replace", path, error)
                })
            } else {
                tokio::fs::remove_file(path)
                    .await
                    .map_err(|error| io_path("remove file before network directory", path, error))
            }
        }
        Ok(_) => Err(other_message(
            "create received directory",
            format!(
                "target path exists and is not a directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_path("read target metadata", path, error)),
    }
}

pub(super) async fn ensure_file_path(path: &Path, delete_enabled: bool) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() && delete_enabled => tokio::fs::remove_dir_all(path)
            .await
            .map_err(|error| io_path("remove directory before network file", path, error)),
        Ok(metadata) if metadata.is_dir() => Err(other_message(
            "receive file",
            format!("target path exists and is a directory: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_path("read target metadata", path, error)),
    }
}

pub(super) async fn delete_obsolete(root: &Path, manifest: &Manifest) -> Result<usize> {
    let target_snapshot = match crate::scan::scan_optional_directory(root, false) {
        Ok(snapshot) => snapshot,
        Err(FastSyncError::InvalidTarget(path)) if path == root => return Ok(0),
        Err(error) => return Err(error),
    };
    let desired_dirs: HashSet<_> = manifest.dirs.iter().map(|dir| dir.path.clone()).collect();
    let desired_files: HashSet<_> = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    let mut obsolete: Vec<_> = target_snapshot
        .entries
        .values()
        .filter(|entry| {
            !desired_dirs.contains(&entry.relative_path)
                && !desired_files.contains(&entry.relative_path)
        })
        .cloned()
        .collect();
    obsolete.sort_by_key(|entry| std::cmp::Reverse(entry.relative_path.components().count()));

    let mut deleted = 0_usize;
    for entry in obsolete {
        let path = safe_join(root, &entry.relative_path)?;
        match entry.kind {
            crate::scan::EntryKind::Directory => {
                tokio::fs::remove_dir(&path)
                    .await
                    .map_err(|error| io_path("delete obsolete network directory", &path, error))?;
            }
            crate::scan::EntryKind::File | crate::scan::EntryKind::Symlink => {
                tokio::fs::remove_file(&path)
                    .await
                    .map_err(|error| io_path("delete obsolete network file", &path, error))?;
            }
        }
        deleted += 1;
        info!(path = %entry.relative_path.display(), "deleted obsolete network entry");
    }

    Ok(deleted)
}

pub(super) fn apply_directory_metadata(
    root: &Path,
    dirs: &[DirManifest],
    options: TransferOptions,
) -> Result<()> {
    let mut dirs = dirs.to_vec();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.path.components().count()));
    for dir in dirs {
        let path = safe_join(root, &dir.path)?;
        apply_path_metadata(&path, &dir.metadata, options)?;
    }
    Ok(())
}

pub(super) fn apply_file_metadata(
    root: &Path,
    files: &[FileManifest],
    options: TransferOptions,
) -> Result<()> {
    for file in files {
        let path = safe_join(root, &file.path)?;
        apply_path_metadata(&path, &file.metadata, options)?;
    }
    Ok(())
}

pub(super) fn apply_path_metadata(
    path: &Path,
    metadata: &WireMetadata,
    options: TransferOptions,
) -> Result<()> {
    if options.preserve_permissions {
        set_permissions(path, metadata)?;
    }
    if options.preserve_times {
        if let Some(mtime) = metadata.modified_filetime() {
            filetime::set_file_mtime(path, mtime)
                .map_err(|error| io_path("set received path modified time", path, error))?;
        }
    }
    Ok(())
}

pub(super) fn set_permissions(path: &Path, metadata: &WireMetadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = metadata.unix_mode {
            let permissions = std::fs::Permissions::from_mode(mode);
            std::fs::set_permissions(path, permissions)
                .map_err(|error| io_path("set received path permissions", path, error))?;
            return Ok(());
        }
    }

    let mut permissions = std::fs::metadata(path)
        .map_err(|error| io_path("read received path permissions", path, error))?
        .permissions();
    permissions.set_readonly(metadata.readonly);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| io_path("set received path permissions", path, error))
}
