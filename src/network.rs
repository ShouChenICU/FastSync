use std::sync::atomic::AtomicU64;

mod cli;
mod protocol;
mod protocol_io;
mod session;
mod summary;
mod transfer;
mod util;

#[cfg(test)]
mod tests;

pub use cli::{ConnectConfig, NetworkCommand, ShareConfig, SyncDirection, print_subcommand_help};
pub use session::{run_connect, run_share};

const DEFAULT_BIND_PORT: u16 = 7443;
const PROTOCOL_VERSION: u16 = 7;
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
const BUFFER_SIZE: usize = 1024 * 1024;
const MAX_NETWORK_FILE_CONCURRENCY: usize = 64;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// one-shot 网络共享服务端允许的同步权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareMode {
    /// 服务端只发送目录，客户端只能 pull。
    Send,
    /// 服务端只接收目录，客户端只能 push。
    Receive,
    /// 服务端允许客户端选择 pull 或 push，仍然只执行一次单向同步。
    Both,
}
