use std::path::Path;

use crate::i18n::{Language, tr};
use crate::summary::{human_bytes, human_duration};

use super::{SyncDirection, util::throughput_text};

/// 共享端一次会话的最终结果；拒绝时携带面向日志/调用者的原因。
#[derive(Debug)]
pub(super) enum ShareOutcome {
    Completed(TransferSummary),
    Rejected(String),
}

/// 一次完整网络传输的统计信息，作为双方摘要和 Ack 的统一数据源。
#[derive(Debug)]
pub(super) struct TransferSummary {
    pub(super) direction: SyncDirection,
    pub(super) files: usize,
    pub(super) bytes: u64,
    pub(super) deleted: usize,
    pub(super) file_concurrency: usize,
    pub(super) elapsed_ms: u128,
}

/// 接收端实际落盘后的结果，用于返回给发送端确认。
#[derive(Debug)]
pub(super) struct ReceiveSummary {
    pub(super) files: usize,
    pub(super) bytes: u64,
    pub(super) deleted: usize,
}

/// 摘要渲染视角；同一传输在共享端和连接端展示不同角色。
#[derive(Debug, Clone, Copy)]
pub(super) enum NetworkSide {
    Share,
    Connect,
}

/// 可本地化的网络会话摘要输入；字段保持机器统计值，渲染时再格式化。
pub(super) struct NetworkSummary<'a> {
    pub(super) side: NetworkSide,
    pub(super) direction: SyncDirection,
    pub(super) directory: &'a Path,
    pub(super) remote: &'a str,
    pub(super) files: usize,
    pub(super) bytes: u64,
    pub(super) deleted: usize,
    pub(super) file_concurrency: usize,
    pub(super) elapsed_ms: u128,
}

impl NetworkSummary<'_> {
    /// 渲染网络会话摘要；只使用双方都能可靠得知的传输结果。
    pub(super) fn to_text(&self, language: Language) -> String {
        let side = match self.side {
            NetworkSide::Share => tr(language, "network.summary.side_share"),
            NetworkSide::Connect => tr(language, "network.summary.side_connect"),
        };
        let direction = match self.direction {
            SyncDirection::Pull => tr(language, "network.summary.direction_pull"),
            SyncDirection::Push => tr(language, "network.summary.direction_push"),
        };

        format!(
            "\
fastsync {}

{}
  {}  {}
  {}  {}
  {}  {}
  {}  {}
  {}  {}

{}
  {}  {}
  {}  {}
  {}  {}
  {}  {}
  {}  {}
",
            tr(language, "network.summary.title"),
            tr(language, "network.summary.session"),
            tr(language, "network.summary.side"),
            side,
            tr(language, "network.summary.direction"),
            direction,
            tr(language, "network.summary.directory"),
            self.directory.display(),
            tr(language, "network.summary.remote"),
            self.remote,
            tr(language, "network.summary.concurrency"),
            self.file_concurrency,
            tr(language, "network.summary.result"),
            tr(language, "network.summary.files"),
            self.files,
            tr(language, "network.summary.data"),
            human_bytes(self.bytes),
            tr(language, "network.summary.deleted"),
            self.deleted,
            tr(language, "network.summary.duration"),
            human_duration(self.elapsed_ms),
            tr(language, "network.summary.throughput"),
            throughput_text(self.bytes, self.elapsed_ms),
        )
    }
}
