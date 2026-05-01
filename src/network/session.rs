use std::net::{Ipv4Addr, SocketAddr};
use std::time::Instant;

use quinn::{Endpoint, SendStream};
use tracing::{error, info, warn};

use crate::error::{FastSyncError, Result};
use crate::i18n::tr;
use crate::progress::SyncProgress;

use super::{
    ConnectConfig, PROTOCOL_VERSION, ShareConfig, SyncDirection,
    protocol::WireMessage,
    protocol_io::{read_message, write_message},
    summary::{NetworkSide, NetworkSummary, ReceiveSummary, ShareOutcome, TransferSummary},
    transfer::{receive_tree_with_progress, send_tree_with_progress},
    util::{
        generate_pairing_code, insecure_client_config, other, other_message, prompt_code,
        resolve_endpoint, server_config,
    },
};

pub fn run_share(config: ShareConfig) -> Result<()> {
    run_share_with_progress(config, false)
}

/// 启动一次性 QUIC 共享服务端，并在调用方确认适合时启用交互式进度条。
pub fn run_share_with_progress(config: ShareConfig, progress: bool) -> Result<()> {
    install_crypto_provider();
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| other("create tokio runtime", error))?;
    runtime.block_on(run_share_async_progress(config, progress))
}

/// 连接一次性 QUIC 共享服务端并执行同步。
pub fn run_connect(config: ConnectConfig) -> Result<()> {
    run_connect_with_progress(config, false)
}

/// 连接一次性 QUIC 共享服务端并执行同步，可按需启用交互式进度条。
pub fn run_connect_with_progress(config: ConnectConfig, progress: bool) -> Result<()> {
    install_crypto_provider();
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| other("create tokio runtime", error))?;
    runtime.block_on(run_connect_async_progress(config, progress))
}

pub(super) fn install_crypto_provider() {
    let _ = quinn::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub(super) async fn run_share_async_progress(config: ShareConfig, progress: bool) -> Result<()> {
    let progress = SyncProgress::new(progress);

    if !config.root.is_dir() {
        return Err(FastSyncError::InvalidSource(config.root));
    }

    let code = config.code.clone().unwrap_or_else(generate_pairing_code);
    let endpoint = Endpoint::server(server_config()?, config.bind)
        .map_err(|error| other("start QUIC server endpoint", error))?;
    let local_addr = endpoint
        .local_addr()
        .map_err(|error| other("read QUIC server local address", error))?;

    println!("{}", tr(config.language, "network.share.started"));
    println!("  endpoint: quic://{local_addr}");
    println!("  code: {code}");
    println!("  mode: {}", config.mode.as_str());
    println!("  allow delete: {}", config.allow_delete);
    println!("  root: {}", config.root.display());
    info!(
        bind = %local_addr,
        root = %config.root.display(),
        mode = config.mode.as_str(),
        allow_delete = config.allow_delete,
        "network share server started"
    );

    let mut failures = 0_u8;
    loop {
        let Some(incoming) = endpoint.accept().await else {
            return Err(other_message("accept QUIC connection", "endpoint closed"));
        };
        let remote = incoming.remote_address();
        info!(remote = %remote, "incoming QUIC connection");

        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(remote = %remote, error = %error, "QUIC handshake failed");
                continue;
            }
        };

        let result =
            handle_share_connection_with_progress(&config, &code, connection, remote, &progress)
                .await;
        match result {
            Ok(ShareOutcome::Completed(summary)) => {
                println!(
                    "{}",
                    NetworkSummary {
                        side: NetworkSide::Share,
                        direction: summary.direction,
                        directory: &config.root,
                        remote: &remote.to_string(),
                        files: summary.files,
                        bytes: summary.bytes,
                        deleted: summary.deleted,
                        file_concurrency: summary.file_concurrency,
                        elapsed_ms: summary.elapsed_ms,
                    }
                    .to_text(config.language)
                );
                info!(
                    remote = %remote,
                    direction = summary.direction.as_str(),
                    files = summary.files,
                    bytes = summary.bytes,
                    deleted = summary.deleted,
                    elapsed_ms = summary.elapsed_ms,
                    "network one-shot sync completed"
                );
                endpoint.close(0_u32.into(), b"done");
                endpoint.wait_idle().await;
                return Ok(());
            }
            Ok(ShareOutcome::Rejected(reason)) => {
                failures = failures.saturating_add(1);
                warn!(
                    remote = %remote,
                    reason,
                    failures,
                    max_failures = config.max_failures,
                    "network pairing rejected"
                );
                if failures >= config.max_failures {
                    endpoint.close(1_u32.into(), b"too many failed pairing attempts");
                    return Err(other_message(
                        "pairing failed",
                        "too many failed pairing attempts",
                    ));
                }
            }
            Err(error) => {
                error!(remote = %remote, error = %error, "network share connection failed");
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn handle_share_connection(
    config: &ShareConfig,
    code: &str,
    connection: quinn::Connection,
    remote: SocketAddr,
) -> Result<ShareOutcome> {
    handle_share_connection_with_progress(
        config,
        code,
        connection,
        remote,
        &SyncProgress::new(false),
    )
    .await
}

pub(super) async fn handle_share_connection_with_progress(
    config: &ShareConfig,
    code: &str,
    connection: quinn::Connection,
    remote: SocketAddr,
    progress: &SyncProgress,
) -> Result<ShareOutcome> {
    let started = Instant::now();
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| other("accept control stream", error))?;
    let hello = read_message(&mut recv).await?;
    let WireMessage::Hello {
        code: provided_code,
        direction,
        protocol,
        options,
    } = hello
    else {
        return reject_pairing(&mut send, "expected hello".to_string()).await;
    };

    info!(
        remote = %remote,
        requested_direction = direction.as_str(),
        protocol,
        delete = options.delete,
        strict = options.strict,
        preserve_times = options.preserve_times,
        preserve_permissions = options.preserve_permissions,
        file_concurrency = options.file_concurrency,
        "pairing hello received"
    );

    if protocol != PROTOCOL_VERSION {
        let reason = format!("unsupported protocol version {protocol}");
        return reject_pairing(&mut send, reason).await;
    }
    if provided_code.trim() != code {
        return reject_pairing(&mut send, "invalid pairing code".to_string()).await;
    }
    if !config.mode.allows(direction) {
        let reason = format!(
            "direction {} is not allowed by server mode {}",
            direction.as_str(),
            config.mode.as_str()
        );
        return reject_pairing(&mut send, reason).await;
    }
    if direction == SyncDirection::Push && options.delete && !config.allow_delete {
        let reason = "server does not allow delete for push".to_string();
        return reject_pairing(&mut send, reason).await;
    }

    write_message(
        &mut send,
        &WireMessage::Accept {
            mode: config.mode.as_str().to_string(),
            delete_allowed: direction == SyncDirection::Pull || config.allow_delete,
        },
    )
    .await?;
    info!(
        remote = %remote,
        direction = direction.as_str(),
        "pairing accepted"
    );

    let summary = match direction {
        SyncDirection::Pull => {
            send_tree_with_progress(
                &config.root,
                &connection,
                &mut send,
                &mut recv,
                options,
                progress,
            )
            .await?;
            let ack = read_message(&mut recv).await?;
            match ack {
                WireMessage::Ack {
                    files,
                    bytes,
                    deleted,
                } => TransferSummary {
                    direction,
                    files,
                    bytes,
                    deleted,
                    file_concurrency: options.file_concurrency,
                    elapsed_ms: started.elapsed().as_millis(),
                },
                _ => {
                    return Err(other_message(
                        "read client acknowledgement",
                        "unexpected message",
                    ));
                }
            }
        }
        SyncDirection::Push => {
            let summary = receive_tree_with_progress(
                &config.root,
                &connection,
                &mut recv,
                &mut send,
                options,
                progress,
            )
            .await?;
            write_message(
                &mut send,
                &WireMessage::Ack {
                    files: summary.files,
                    bytes: summary.bytes,
                    deleted: summary.deleted,
                },
            )
            .await?;
            finish_send_stream(&mut send).await?;
            TransferSummary {
                direction,
                files: summary.files,
                bytes: summary.bytes,
                deleted: summary.deleted,
                file_concurrency: options.file_concurrency,
                elapsed_ms: started.elapsed().as_millis(),
            }
        }
    };

    Ok(ShareOutcome::Completed(summary))
}

pub(super) async fn reject_pairing(send: &mut SendStream, reason: String) -> Result<ShareOutcome> {
    write_message(
        send,
        &WireMessage::Reject {
            reason: reason.clone(),
        },
    )
    .await?;
    finish_send_stream(send).await?;
    Ok(ShareOutcome::Rejected(reason))
}

pub(super) async fn run_connect_async_progress(
    config: ConnectConfig,
    progress: bool,
) -> Result<()> {
    let progress = SyncProgress::new(progress);
    let started = Instant::now();
    let code = match config.code {
        Some(ref code) => code.clone(),
        None => prompt_code()?,
    };
    let remote_addr = resolve_endpoint(&config.endpoint).await?;
    let mut endpoint = Endpoint::client((Ipv4Addr::UNSPECIFIED, 0).into())
        .map_err(|error| other("create QUIC client endpoint", error))?;
    endpoint.set_default_client_config(insecure_client_config());

    info!(
        endpoint = config.endpoint,
        remote = %remote_addr,
        direction = config.direction.as_str(),
        directory = %config.directory.display(),
        "connecting to network share"
    );

    let connection = endpoint
        .connect(remote_addr, "fastsync.local")
        .map_err(|error| other("start QUIC client connection", error))?
        .await
        .map_err(|error| other("establish QUIC client connection", error))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| other("open control stream", error))?;

    write_message(
        &mut send,
        &WireMessage::Hello {
            code,
            direction: config.direction,
            protocol: PROTOCOL_VERSION,
            options: config.transfer_options(),
        },
    )
    .await?;

    match read_message(&mut recv).await? {
        WireMessage::Accept {
            mode,
            delete_allowed,
        } => {
            info!(
                server_mode = mode,
                delete_allowed, "pairing accepted by server"
            );
        }
        WireMessage::Reject { reason } => {
            return Err(other_message("pairing rejected", reason));
        }
        _ => return Err(other_message("pairing", "unexpected server response")),
    }

    let summary = match config.direction {
        SyncDirection::Pull => {
            let summary = receive_tree_with_progress(
                &config.directory,
                &connection,
                &mut recv,
                &mut send,
                config.transfer_options(),
                &progress,
            )
            .await?;
            write_message(
                &mut send,
                &WireMessage::Ack {
                    files: summary.files,
                    bytes: summary.bytes,
                    deleted: summary.deleted,
                },
            )
            .await?;
            finish_send_stream(&mut send).await?;
            summary
        }
        SyncDirection::Push => {
            send_tree_with_progress(
                &config.directory,
                &connection,
                &mut send,
                &mut recv,
                config.transfer_options(),
                &progress,
            )
            .await?;
            match read_message(&mut recv).await? {
                WireMessage::Ack {
                    files,
                    bytes,
                    deleted,
                } => ReceiveSummary {
                    files,
                    bytes,
                    deleted,
                },
                _ => {
                    return Err(other_message(
                        "read server acknowledgement",
                        "unexpected message",
                    ));
                }
            }
        }
    };

    println!(
        "{}",
        NetworkSummary {
            side: NetworkSide::Connect,
            direction: config.direction,
            directory: &config.directory,
            remote: &remote_addr.to_string(),
            files: summary.files,
            bytes: summary.bytes,
            deleted: summary.deleted,
            file_concurrency: config.network_concurrency,
            elapsed_ms: started.elapsed().as_millis(),
        }
        .to_text(config.language)
    );
    info!(
        direction = config.direction.as_str(),
        files = summary.files,
        bytes = summary.bytes,
        deleted = summary.deleted,
        file_concurrency = config.network_concurrency,
        "network client sync completed"
    );
    connection.close(0_u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(())
}

pub(super) async fn finish_send_stream(send: &mut SendStream) -> Result<()> {
    send.finish()
        .map_err(|error| other("finish QUIC send stream", error))?;
    send.stopped()
        .await
        .map_err(|error| other("wait for QUIC send stream delivery", error))?;
    Ok(())
}
