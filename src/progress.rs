use std::sync::Arc;

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
}

impl ProgressPhase {
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            span: None,
            title: None,
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
        "{spinner:.cyan} {bar:34.cyan/blue} {pos:>7}/{len:7} {wide_msg} [{elapsed_precise}]",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("█▓░")
    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}
