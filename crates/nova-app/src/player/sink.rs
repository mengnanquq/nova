// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 进度上报的装配：`ProgressSink` 适配器 + 后台 worker。
//!
//! `ProgressReporter` 是 `&mut self` 的异步 API，而界面线程不能为一次 HTTP 请求卡住
//! （服务端不可达时 `reqwest` 可能拖到超时）。所以上报器整个搬到 tokio 任务里：
//! 界面线程只往无界 channel 里塞"该上报了"，退出时发一条 `Flush` 并**有界地**等结果。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use nova_core::{PlaybackProgress, UserPreferences};
use nova_playback::{
    FlushOutcome, ProgressReporter, ProgressSink, ProgressTrigger, ReportError, SystemClock,
};
use nova_provider::{MediaProvider, ProviderError};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::local::Local;

/// 退出时等待 `flush` 的上限：宁可少补一次进度，也不能让"退出播放页"卡住。
const FLUSH_TIMEOUT: Duration = Duration::from_millis(1500);

/// 把 `MediaProvider::report_progress` 包成 [`ProgressSink`]。
pub(crate) struct ProviderSink {
    provider: Arc<dyn MediaProvider>,
    /// 是否已经发过"开始播放"。服务端要它才会把后续进度算进会话。
    started: AtomicBool,
}

impl ProviderSink {
    /// 由 provider 构造。
    pub(crate) fn new(provider: Arc<dyn MediaProvider>) -> Self {
        Self {
            provider,
            started: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl ProgressSink for ProviderSink {
    async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
        if !self.started.swap(true, Ordering::Relaxed) {
            // 起播注册失败不阻塞进度：下一次 Progress 仍可能被服务端接受。
            if let Err(error) = self.provider.report_started(progress).await {
                tracing::debug!(%error, "起播上报失败（不影响后续进度上报）");
            }
        }
        match self.provider.report_progress(progress).await {
            Ok(()) => Ok(()),
            Err(ProviderError::Unauthorized) => Err(ReportError::Unauthorized),
            Err(error) if is_transient(&error) => Err(ReportError::Transient),
            Err(_) => Err(ReportError::Rejected),
        }
    }
}

impl ProviderSink {
    /// 结束会话：服务端只在收到这条时持久化续播点。
    async fn stop(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
        match self.provider.report_stopped(progress).await {
            Ok(()) => Ok(()),
            Err(ProviderError::Unauthorized) => Err(ReportError::Unauthorized),
            Err(error) if is_transient(&error) => Err(ReportError::Transient),
            Err(_) => Err(ReportError::Rejected),
        }
    }
}

fn is_transient(error: &ProviderError) -> bool {
    match error {
        ProviderError::Network(_) => true,
        ProviderError::Http { status } => *status >= 500 || *status == 429,
        _ => false,
    }
}

/// 上报 worker 的命令。
enum Command {
    /// 到点了，报一次。
    Submit(ProgressTrigger, PlaybackProgress),
    /// 退出前尽力补发，结果通过 oneshot 回来。
    Flush(oneshot::Sender<FlushOutcome>),
    /// 进程退出：落盘后结束任务。
    Shutdown,
}

/// 界面侧的句柄（`Send`，可以留在主线程结构里）。
pub(crate) struct Progress {
    commands: Option<UnboundedSender<Command>>,
    runtime: Handle,
}

impl Progress {
    /// 起一个 worker：补发上次没发出去的进度，然后按命令上报。
    ///
    /// 没有 provider（未登录 / 离线）时返回一个空句柄：所有上报都是 no-op，不报错。
    pub(crate) fn start(local: &Arc<Local>, runtime: &Handle) -> Self {
        let Some(provider) = local.provider_dyn() else {
            tracing::debug!("没有可用 provider，本次运行不上报播放进度");
            return Self {
                commands: None,
                runtime: runtime.clone(),
            };
        };
        let prefs: UserPreferences = local.preferences.clone();
        let pending_path = local.data_dir.join("pending-progress.json");
        let (commands, mut receiver) = unbounded_channel();
        runtime.spawn(async move {
            let sink = ProviderSink::new(provider);
            let mut reporter = ProgressReporter::new(&prefs, Arc::new(SystemClock))
                .with_pending_path(pending_path);
            match reporter.load_pending() {
                Ok(0) => {}
                Ok(loaded) => {
                    tracing::info!(loaded, "发现上次没提交的播放进度，尝试补发");
                    let outcome = reporter.flush(&sink).await;
                    tracing::info!(
                        sent = outcome.sent,
                        failed = outcome.failed,
                        persisted = outcome.persisted,
                        "播放进度补发完成"
                    );
                }
                Err(error) => tracing::warn!(%error, "读取播放进度补发文件失败"),
            }
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::Submit(trigger, progress) => {
                        let outcome = reporter.submit(&sink, trigger, &progress).await;
                        tracing::debug!(?trigger, ?outcome, "播放进度上报");
                        if trigger == ProgressTrigger::Exit {
                            // 结束会话是独立的一步：Jellyfin 靠它落盘续播点。
                            // 失败也只记日志——退出路径上不值得再阻塞用户。
                            if let Err(error) = sink.stop(&progress).await {
                                tracing::warn!(?error, "结束会话上报失败（续播点可能不准）");
                            }
                        }
                    }
                    Command::Flush(reply) => {
                        let outcome = reporter.flush(&sink).await;
                        // 接收端可能已经不等了（超时），忽略发送失败。
                        let _ = reply.send(outcome);
                    }
                    Command::Shutdown => {
                        let outcome = reporter.flush(&sink).await;
                        tracing::debug!(?outcome, "退出前补发播放进度");
                        break;
                    }
                }
            }
        });
        Self {
            commands: Some(commands),
            runtime: runtime.clone(),
        }
    }

    /// 提交一次进度（非阻塞）。
    pub(crate) fn submit(&self, trigger: ProgressTrigger, progress: PlaybackProgress) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::Submit(trigger, progress));
        }
    }

    /// 退出前尽力补发；最多等 [`FLUSH_TIMEOUT`]。
    pub(crate) fn flush(&self) -> Option<FlushOutcome> {
        let commands = self.commands.as_ref()?;
        let (reply, wait) = oneshot::channel();
        if commands.send(Command::Flush(reply)).is_err() {
            return None;
        }
        match self
            .runtime
            .block_on(async { tokio::time::timeout(FLUSH_TIMEOUT, wait).await })
        {
            Ok(Ok(outcome)) => Some(outcome),
            Ok(Err(_)) => None,
            Err(_) => {
                tracing::warn!("播放进度补发超时，已跳过（下次启动会从补发文件重试）");
                None
            }
        }
    }

    /// 退出进程：让 worker 落盘后结束。
    pub(crate) fn shutdown(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_errors_are_not_retried() {
        assert!(is_transient(&ProviderError::Network("timeout".into())));
        assert!(is_transient(&ProviderError::Http { status: 503 }));
        assert!(is_transient(&ProviderError::Http { status: 429 }));
        assert!(!is_transient(&ProviderError::Http { status: 400 }));
        assert!(!is_transient(&ProviderError::Unauthorized));
        assert!(!is_transient(&ProviderError::NotFound));
    }
}
