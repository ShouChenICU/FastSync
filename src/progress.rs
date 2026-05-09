use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use indicatif::ProgressStyle;
use tracing::{Level, Span, span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::i18n::tr_current;
use crate::summary::human_bytes;

/// 本地同步流程的进度渲染入口。
///
/// 该类型只负责把扫描、比较、执行等阶段转换为 tracing-indicatif 进度
/// span。它不参与任何同步决策，因此不会改变复制、删除、校验或 dry-run
/// 语义。非交互式终端或 JSON 输出会创建禁用实例，调用方无需额外分支。
#[derive(Clone)]
pub(crate) struct SyncProgress {
    enabled: bool,
    styles: Arc<ProgressStyles>,
}

impl SyncProgress {
    /// 创建一个按需启用的进度渲染器。
    ///
    /// `enabled` 由 CLI 入口根据 stderr 是否为 TTY 和输出模式决定；库层只
    /// 接收结果，避免核心同步流程直接依赖终端环境。
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            styles: Arc::new(ProgressStyles::new()),
        }
    }

    /// 为无法预先知道总量的阶段创建 spinner。
    pub(crate) fn spinner(&self, title_key: &'static str) -> ProgressPhase {
        self.phase(title_key, None, &self.styles.spinner)
    }

    /// 为已知总量的阶段创建底部进度条。
    pub(crate) fn bar(&self, title_key: &'static str, len: usize) -> ProgressPhase {
        let phase = self.phase(title_key, Some(len as u64), &self.styles.bar);
        phase.set_message(&tr_current("progress.starting"));
        phase
    }

    fn phase(
        &self,
        title_key: &'static str,
        len: Option<u64>,
        style: &ProgressStyle,
    ) -> ProgressPhase {
        if !self.enabled {
            return ProgressPhase::disabled();
        }

        let title = tr_current(title_key);
        let span = span!(
            Level::INFO,
            "fastsync",
            indicatif.pb_show = tracing::field::Empty
        );
        span.pb_set_style(style);
        if let Some(len) = len {
            span.pb_set_length(len);
            span.pb_set_position(0);
        }
        span.pb_set_message(&title);
        span.pb_start();

        ProgressPhase {
            enabled: true,
            span: Some(span),
            title: Some(title),
            stats: Arc::new(ProgressStats::new()),
        }
    }
}

/// 单个同步阶段的进度条句柄。
///
/// 句柄可在线程间克隆，方便执行阶段 worker 在完成任务后递增进度。所有方法
/// 在禁用状态下都是 no-op，便于测试和非 TTY 环境复用同一条代码路径。
#[derive(Clone)]
pub(crate) struct ProgressPhase {
    enabled: bool,
    span: Option<Span>,
    title: Option<String>,
    stats: Arc<ProgressStats>,
}

impl ProgressPhase {
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            span: None,
            title: None,
            stats: Arc::new(ProgressStats::new()),
        }
    }

    /// 递增当前阶段已完成的工作量。
    pub(crate) fn inc(&self, delta: u64) {
        if let Some(span) = self.active_span() {
            span.pb_inc(delta);
        }
    }

    /// 更新当前阶段的状态文本。
    pub(crate) fn set_message(&self, message: &str) {
        if let Some(span) = self.active_span() {
            if let Some(title) = &self.title {
                span.pb_set_message(&format!("{title}  {message}"));
            } else {
                span.pb_set_message(message);
            }
        }
    }

    /// 记录计划阶段的核心诊断计数。
    pub(crate) fn set_plan_status(&self, hashed_files: usize, operations: usize, bytes: u64) {
        if !self.enabled {
            return;
        }

        self.set_message(&format!(
            "{}={}  {}={}  {}={}",
            tr_current("progress.hashes"),
            hashed_files,
            tr_current("progress.operations"),
            operations,
            tr_current("progress.data"),
            human_bytes(bytes)
        ));
    }

    /// 记录网络 manifest 阶段已观察到的目录、文件和总字节数。
    pub(crate) fn set_manifest_status(&self, dirs: usize, files: usize, bytes: u64) {
        if !self.enabled {
            return;
        }

        self.set_message(&format!(
            "{}={}  {}={}  {}={}",
            tr_current("progress.dirs"),
            dirs,
            tr_current("progress.files"),
            files,
            tr_current("progress.data"),
            human_bytes(bytes)
        ));
    }

    /// 记录网络请求规划阶段的哈希比较和文件请求数量。
    pub(crate) fn set_request_status(&self, hashes: usize, requests: usize) {
        if !self.enabled {
            return;
        }

        self.set_message(&format!(
            "{}={}  {}={}",
            tr_current("progress.hashes"),
            hashes,
            tr_current("progress.requests"),
            requests
        ));
    }

    /// 记录本地复制完成后的阶段平均传输状态。
    ///
    /// 本地端保留标准库复制路径，只有文件完成后才知道该文件字节数；这里基于
    /// 已完成文件累计值估算速度，不插入 per-chunk 回调以免损失内核复制优化。
    pub(crate) fn record_completed_transfer_file(&self, bytes: u64) {
        if !self.enabled {
            return;
        }

        self.stats.completed_files.fetch_add(1, Ordering::Relaxed);
        self.stats
            .transferred_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        self.refresh_transfer_status();
    }

    /// 记录网络传输中已完成的文件数。
    pub(crate) fn record_completed_transfer_files(&self, files: usize) {
        if !self.enabled {
            return;
        }

        self.stats
            .completed_files
            .fetch_add(files, Ordering::Relaxed);
        self.refresh_transfer_status();
    }

    /// 记录网络传输中的实时字节增量。
    ///
    /// 网络发送和接收本身已经按 chunk 流式处理，因此在成功写出或落盘后累加
    /// 字节不会改变协议、校验或错误语义，只影响终端可观测状态。
    pub(crate) fn add_transfer_bytes(&self, bytes: u64) {
        if !self.enabled {
            return;
        }

        self.stats
            .transferred_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        self.refresh_transfer_status();
    }

    /// 记录删除阶段已删除的目标端陈旧项数量。
    pub(crate) fn set_delete_status(&self, deleted: usize) {
        if !self.enabled {
            return;
        }

        self.set_message(&format!("{}={}", tr_current("progress.deleted"), deleted));
    }

    /// 标记阶段完成。
    ///
    /// tracing-indicatif 会在 span 关闭时移除底部进度条，最终结果仍由
    /// tracing 日志和摘要输出负责，避免多阶段同步后遗留多条完成进度。
    pub(crate) fn finish(self) {
        drop(self);
    }

    fn active_span(&self) -> Option<&Span> {
        if self.enabled {
            self.span.as_ref()
        } else {
            None
        }
    }

    fn refresh_transfer_status(&self) {
        let files = self.stats.completed_files.load(Ordering::Relaxed);
        let bytes = self.stats.transferred_bytes.load(Ordering::Relaxed);
        self.set_message(&format!(
            "{}={}  {}={}  {}={}",
            tr_current("progress.files"),
            files,
            tr_current("progress.data"),
            human_bytes(bytes),
            tr_current("progress.speed"),
            transfer_speed_text(bytes, self.stats.started.elapsed().as_millis())
        ));
    }
}

struct ProgressStats {
    started: Instant,
    completed_files: AtomicUsize,
    transferred_bytes: AtomicU64,
}

impl ProgressStats {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            completed_files: AtomicUsize::new(0),
            transferred_bytes: AtomicU64::new(0),
        }
    }
}

struct ProgressStyles {
    spinner: ProgressStyle,
    bar: ProgressStyle,
}

impl ProgressStyles {
    fn new() -> Self {
        Self {
            spinner: spinner_style(),
            bar: bar_style(),
        }
    }
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {wide_msg} [{elapsed_precise}]")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.cyan} {bar:34.cyan/blue} {pos:>7}/{len:7} {percent:>3}% {wide_msg} [{elapsed_precise}]",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("█▓░")
    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

fn transfer_speed_text(bytes: u64, elapsed_ms: u128) -> String {
    let bytes_per_second = bytes_per_second(bytes, elapsed_ms);
    format!("{}/s", human_bytes(bytes_per_second))
}

fn bytes_per_second(bytes: u64, elapsed_ms: u128) -> u64 {
    if elapsed_ms == 0 {
        return 0;
    }

    ((bytes as u128).saturating_mul(1_000) / elapsed_ms).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_per_second_handles_zero_elapsed() {
        assert_eq!(bytes_per_second(1024, 0), 0);
    }

    #[test]
    fn transfer_speed_text_uses_adaptive_units() {
        assert_eq!(transfer_speed_text(2_048, 1_000), "2.0 KiB/s");
        assert_eq!(transfer_speed_text(2 * 1024 * 1024, 1_000), "2.0 MiB/s");
    }
}
