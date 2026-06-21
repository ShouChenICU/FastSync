use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use quinn::{ClientConfig, TransportConfig, VarInt};
use rand::Rng;
use rcgen::{CertifiedKey, generate_simple_self_signed};

use crate::error::{FastSyncError, Result};
use crate::hash::Blake3Digest;
use crate::summary::human_bytes;

use super::{DEFAULT_BIND_PORT, TEMP_COUNTER, cli::validate_pairing_code};

const NETWORK_MAX_IDLE_TIMEOUT_MS: u32 = 5 * 60 * 1000;
const NETWORK_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

pub(super) fn server_config() -> Result<quinn::ServerConfig> {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["fastsync.local".to_string(), "localhost".to_string()])
            .map_err(|error| other("generate temporary QUIC certificate", error))?;
    let mut config =
        quinn::ServerConfig::with_single_cert(vec![cert.der().clone()], signing_key.into())
            .map_err(|error| other("create QUIC server TLS config", error))?;
    config.transport_config(network_transport_config());
    Ok(config)
}

pub(super) fn insecure_client_config() -> ClientConfig {
    let crypto = quinn::rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(
            rustls_dangerous::NoCertificateVerification,
        ))
        .with_no_client_auth();
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .expect("rustls client config must contain a QUIC initial cipher suite");
    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(network_transport_config());
    config
}

/// 构建网络会话共用的 QUIC 传输参数，避免健康连接在长时间无业务数据时被默认空闲超时关闭。
pub(super) fn network_transport_config() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport.max_idle_timeout(Some(VarInt::from_u32(NETWORK_MAX_IDLE_TIMEOUT_MS).into()));
    transport.keep_alive_interval(Some(NETWORK_KEEP_ALIVE_INTERVAL));
    Arc::new(transport)
}

pub(super) async fn resolve_endpoint(endpoint: &str) -> Result<SocketAddr> {
    let raw = endpoint.strip_prefix("quic://").unwrap_or(endpoint);
    let (host, port) = match raw.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|error| other("parse QUIC endpoint port", error))?;
            (host, port)
        }
        None => (raw, DEFAULT_BIND_PORT),
    };
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| other("resolve QUIC endpoint", error))?;
    addrs
        .next()
        .ok_or_else(|| other_message("resolve QUIC endpoint", "no address resolved"))
}

pub(super) fn prompt_code() -> Result<String> {
    eprint!("Pairing code: ");
    let mut code = String::new();
    std::io::stdin()
        .read_line(&mut code)
        .map_err(|error| other("read pairing code", error))?;
    let code = code.trim().to_string();
    validate_pairing_code(&code).map_err(|message| other_message("read pairing code", message))?;
    Ok(code)
}

pub(super) fn generate_pairing_code() -> String {
    let mut rng = rand::rng();
    let code: u32 = rng.random_range(0..=999_999);
    format!("{code:06}")
}

pub(super) fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(FastSyncError::PathOutsideRoot {
            path: relative.to_path_buf(),
        });
    }
    Ok(root.join(relative))
}

pub(super) fn unique_temp_path(parent: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".fastsync.net.tmp.{}.{}",
        std::process::id(),
        counter
    ))
}

pub(super) fn hex_digest(digest: Blake3Digest) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub(super) fn throughput_bps(bytes: u64, elapsed_ms: u128) -> u64 {
    if elapsed_ms == 0 {
        return 0;
    }
    ((bytes as u128).saturating_mul(1000) / elapsed_ms).min(u64::MAX as u128) as u64
}

pub(super) fn throughput_text(bytes: u64, elapsed_ms: u128) -> String {
    let bps = throughput_bps(bytes, elapsed_ms);
    format!("{}/s", human_bytes(bps))
}

pub(super) fn other(context: impl Into<String>, error: impl std::fmt::Display) -> FastSyncError {
    FastSyncError::Io {
        context: context.into(),
        source: std::io::Error::other(error.to_string()),
    }
}

pub(super) fn other_message(
    context: impl Into<String>,
    message: impl Into<String>,
) -> FastSyncError {
    FastSyncError::Io {
        context: context.into(),
        source: std::io::Error::other(message.into()),
    }
}

pub(super) fn io_path(context: &'static str, path: &Path, error: std::io::Error) -> FastSyncError {
    FastSyncError::Io {
        context: format!("{context}: {}", path.display()),
        source: error,
    }
}
