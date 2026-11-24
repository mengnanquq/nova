// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 进度上报：节奏、去重、失败退避重试、落盘补发。
//!
//! # 节奏
//!
//! - 播放中按 [`UserPreferences::effective_report_interval`]（默认 10 秒，最少 5 秒）上报；
//! - 暂停 / 切轨 / seek / 退出立即上报（[`ProgressTrigger`]）；
//! - 位置与状态都没变的重复上报会被去重掉——但 [`ProgressTrigger::Exit`] 例外，
//!   规范要求"退出前必须提交一次进度"，宁可多一条请求。
//!
//! # 失败
//!
//! 上报失败不能阻塞播放：可重试的错误进队列，按 1s → 2s → 4s … 最多 60s 退避；
//! 不可重试的错误（认证失效、404、服务端拒绝）直接丢弃——重试只会继续失败。
//! 退出前用 [`ProgressReporter::flush`] 尽力补发一次，仍失败就把队列写到
//! `<data>/pending-progress.json`（路径由调用方给），下次启动
//! [`ProgressReporter::load_pending`] 读回后补发并删除文件。
//!
//! 为什么不进 `nova-storage` 的偏好表：偏好表是"单键 JSON"，塞一个会随播放频繁变化的队列进去
//! 会跟偏好写入互相覆盖；换张表又要把 schema 迁移甩给别的 crate。一个独立小文件更简单，
//! 也更容易在数据库坏掉时救回来。
//!
//! 时间全部走 [`Clock`]，所以测试可以确定性地验证"第 9 秒不上报、第 10 秒上报"和退避序列。

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nova_core::{PlaybackProgress, UserPreferences};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;

/// 首次失败后的重试间隔。
const RETRY_BASE: Duration = Duration::from_secs(1);
/// 重试间隔上限。
const RETRY_MAX: Duration = Duration::from_secs(60);
/// 落盘文件格式版本。
const PENDING_VERSION: u32 = 1;

/// 上报时机。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressTrigger {
    /// 定时驱动（通常会话的 500 ms 心跳）。
    Tick,
    /// 暂停。
    Pause,
    /// 切换音轨 / 字幕。
    TrackChange,
    /// seek 之后。
    Seek,
    /// 退出播放页 / 退出进程。
    Exit,
}

/// 上报失败原因。只决定"要不要重试"，不进用户文案。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// 网络或服务端暂时不可用（超时、连接重置、5xx）。
    #[error("网络或服务端暂时不可用")]
    Transient,
    /// 登录状态失效，重试没有意义。
    #[error("登录状态失效")]
    Unauthorized,
    /// 服务端明确拒绝了这条进度（4xx）。
    #[error("服务端拒绝了这条进度")]
    Rejected,
}

impl ReportError {
    /// 是否值得退避重试。
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Transient)
    }
}

/// 进度接收方。provider 侧实现（把 `nova_provider::MediaProvider::report_progress` 包一层，
/// 把它的错误映射成 [`ReportError`]）。
#[async_trait]
pub trait ProgressSink: Send + Sync {
    /// 上报一条进度。
    async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError>;
}

/// 一次上报的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    /// 不需要上报（去重或还没到间隔）。
    Skipped,
    /// 已上报成功。
    Sent,
    /// 失败但可重试，已进队列。
    Queued {
        /// 距离下次重试的时间。
        retry_in: Duration,
    },
    /// 失败且不可重试，已丢弃。
    Dropped,
}

/// 一次 [`ProgressReporter::flush`] 的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushOutcome {
    /// 成功补发的条数。
    pub sent: usize,
    /// 仍然失败的条数（含被丢弃的不可重试条目）。
    pub failed: usize,
    /// 队列里还剩几条。
    pub queued: usize,
    /// 是否成功落盘（队列为空时表示"文件已清掉"）。
    pub persisted: bool,
}

/// 去重用的快照：位置 + 播放/暂停 + 看完状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fingerprint {
    position: Duration,
    paused: bool,
    played: bool,
}

impl Fingerprint {
    fn of(progress: &PlaybackProgress) -> Self {
        Self {
            position: progress.position,
            paused: progress.paused,
            played: progress.played,
        }
    }
}

/// 一条待补发的进度。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingEntry {
    progress: PlaybackProgress,
    attempts: u32,
}

/// 落盘文件（带版本号，将来改格式能识别）。
#[derive(Debug, Serialize, Deserialize)]
struct PendingFile {
    version: u32,
    pending: Vec<PendingEntry>,
}

/// 进度上报器。
pub struct ProgressReporter {
    interval: Duration,
    clock: Arc<dyn Clock>,
    pending_path: Option<PathBuf>,
    queue: Vec<PendingEntry>,
    next_retry_at: Option<Instant>,
    last_attempt: Option<Fingerprint>,
    last_report_at: Option<Instant>,
}

impl ProgressReporter {
    /// 由用户偏好与时钟构造。
    pub fn new(prefs: &UserPreferences, clock: Arc<dyn Clock>) -> Self {
        Self {
            interval: prefs.effective_report_interval(),
            clock,
            pending_path: None,
            queue: Vec::new(),
            next_retry_at: None,
            last_attempt: None,
            last_report_at: None,
        }
    }

    /// 设置补发文件路径（通常是 `<data>/pending-progress.json`）。
    pub fn with_pending_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.pending_path = Some(path.into());
        self
    }

    /// 当前上报间隔（已按偏好的下限夹过）。
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// 队列里待补发的条数。
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// 是否有待补发的进度。
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// 下一次重试的时刻；`None` 表示队列为空。
    pub fn next_retry_at(&self) -> Option<Instant> {
        self.next_retry_at
    }

    /// 补发文件路径。
    pub fn pending_path(&self) -> Option<&std::path::Path> {
        self.pending_path.as_deref()
    }

    /// 这一次要不要真的上报。
    ///
    /// 去重规则：位置与状态都没变就不重复上报（`Exit` 除外）。
    /// 定时驱动还要额外满足上报间隔。
    pub fn should_report(&self, trigger: ProgressTrigger, progress: &PlaybackProgress) -> bool {
        if trigger == ProgressTrigger::Exit {
            return true;
        }
        if self.last_attempt == Some(Fingerprint::of(progress)) {
            return false;
        }
        if trigger == ProgressTrigger::Tick {
            return match self.last_report_at {
                Some(at) => self.clock.now().saturating_duration_since(at) >= self.interval,
                None => true,
            };
        }
        true
    }

    /// 按触发时机上报一次（含队列补发）。
    ///
    /// 队列里还有发不出去的进度时（退避未到期，或刚补发又失败），不会为同一条进度
    /// 额外打请求，只把队列里的位置更新成最新的；退出（[`ProgressTrigger::Exit`]）
    /// 例外：它总是尽力再发一次。
    pub async fn submit(
        &mut self,
        sink: &dyn ProgressSink,
        trigger: ProgressTrigger,
        progress: &PlaybackProgress,
    ) -> ReportOutcome {
        // 定时驱动时顺带补发，这样调用方不需要额外的定时任务。
        let drained = if trigger == ProgressTrigger::Exit {
            if self.queue.is_empty() {
                None
            } else {
                Some(self.drain(sink).await)
            }
        } else if self.retry_due() {
            Some(self.drain(sink).await)
        } else {
            None
        };

        if !self.queue.is_empty() && trigger != ProgressTrigger::Exit {
            self.refresh_pending(progress);
            return match drained {
                Some(outcome) if outcome.failed > 0 => ReportOutcome::Queued {
                    retry_in: self.retry_in(),
                },
                _ => ReportOutcome::Skipped,
            };
        }

        if !self.should_report(trigger, progress) {
            return ReportOutcome::Skipped;
        }
        self.last_attempt = Some(Fingerprint::of(progress));
        match sink.report(progress).await {
            Ok(()) => {
                self.record_success(progress);
                ReportOutcome::Sent
            }
            Err(error) if error.is_retryable() => {
                let retry_in = self.enqueue(progress);
                ReportOutcome::Queued { retry_in }
            }
            Err(_) => ReportOutcome::Dropped,
        }
    }

    /// 尽力补发队列里的全部进度（忽略退避；用于退出前）。
    ///
    /// 还失败就把队列落盘。队列清空时删除补发文件。
    pub async fn flush(&mut self, sink: &dyn ProgressSink) -> FlushOutcome {
        self.drain(sink).await
    }

    /// 从补发文件读回上次没发出去的进度。
    ///
    /// 文件在读取成功后立即删除（数据已在内存里）；真的补发失败会重新落盘。
    /// 返回读到的条数。
    pub fn load_pending(&mut self) -> io::Result<usize> {
        let Some(path) = self.pending_path.clone() else {
            return Ok(0);
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let file: PendingFile = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let loaded = file.pending.len();
        for entry in file.pending {
            // 同一目标（条目 + 资源）只留一条：内存里已有的更新，优先保留。
            if !self
                .queue
                .iter()
                .any(|existing| same_target(&existing.progress, &entry.progress))
            {
                self.queue.push(entry);
            }
        }
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        if !self.queue.is_empty() && self.next_retry_at.is_none() {
            self.next_retry_at = Some(self.clock.now());
        }
        Ok(loaded)
    }

    /// 把队列落盘；队列为空时删除文件。
    pub fn persist(&self) -> io::Result<()> {
        let Some(path) = &self.pending_path else {
            return Ok(());
        };
        if self.queue.is_empty() {
            return match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let payload = PendingFile {
            version: PENDING_VERSION,
            pending: self.queue.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        // 先写临时文件再改名：中途被杀掉也不会留下半个 JSON。
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn retry_due(&self) -> bool {
        match self.next_retry_at {
            Some(at) => self.clock.now() >= at,
            None => false,
        }
    }

    fn retry_in(&self) -> Duration {
        match self.next_retry_at {
            Some(at) => at.saturating_duration_since(self.clock.now()),
            None => Duration::ZERO,
        }
    }

    /// 队列里已有同一目标的进度时，把它更新成最新位置（不发请求）。
    fn refresh_pending(&mut self, progress: &PlaybackProgress) {
        if let Some(entry) = self
            .queue
            .iter_mut()
            .find(|entry| same_target(&entry.progress, progress))
        {
            entry.progress = progress.clone();
        }
    }

    fn record_success(&mut self, progress: &PlaybackProgress) {
        self.last_attempt = Some(Fingerprint::of(progress));
        self.last_report_at = Some(self.clock.now());
        // 已经报上去的位置比队列里的旧位置更新，旧的没必要再发。
        self.queue
            .retain(|entry| !same_target(&entry.progress, progress));
        if self.queue.is_empty() {
            self.next_retry_at = None;
        }
    }

    fn enqueue(&mut self, progress: &PlaybackProgress) -> Duration {
        let attempts = match self
            .queue
            .iter_mut()
            .find(|entry| same_target(&entry.progress, progress))
        {
            Some(entry) => {
                entry.progress = progress.clone();
                entry.attempts = entry.attempts.saturating_add(1);
                entry.attempts
            }
            None => {
                self.queue.push(PendingEntry {
                    progress: progress.clone(),
                    attempts: 1,
                });
                1
            }
        };
        let retry_in = backoff(attempts);
        self.next_retry_at = Some(self.clock.now() + retry_in);
        retry_in
    }

    async fn drain(&mut self, sink: &dyn ProgressSink) -> FlushOutcome {
        let entries = std::mem::take(&mut self.queue);
        let mut sent = 0usize;
        let mut failed = 0usize;
        let mut max_attempts = 0u32;
        for mut entry in entries {
            match sink.report(&entry.progress).await {
                Ok(()) => {
                    sent += 1;
                    self.record_success(&entry.progress);
                }
                Err(error) if error.is_retryable() => {
                    failed += 1;
                    entry.attempts = entry.attempts.saturating_add(1);
                    max_attempts = max_attempts.max(entry.attempts);
                    self.queue.push(entry);
                }
                // 不可重试：重试多少次都一样，直接丢。
                Err(_) => failed += 1,
            }
        }

        if self.queue.is_empty() {
            self.next_retry_at = None;
        } else {
            self.next_retry_at = Some(self.clock.now() + backoff(max_attempts));
        }
        let persisted = match self.persist() {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(error = %error, "播放进度补发文件写入失败");
                false
            }
        };
        FlushOutcome {
            sent,
            failed,
            queued: self.queue.len(),
            persisted,
        }
    }
}

impl std::fmt::Debug for ProgressReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressReporter")
            .field("interval", &self.interval)
            .field("queued", &self.queue.len())
            .field("pending_path", &self.pending_path)
            .finish()
    }
}

fn backoff(attempts: u32) -> Duration {
    let shift = attempts.saturating_sub(1).min(6);
    RETRY_BASE.saturating_mul(1u32 << shift).min(RETRY_MAX)
}

fn same_target(left: &PlaybackProgress, right: &PlaybackProgress) -> bool {
    left.item_id == right.item_id && left.source_id == right.source_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use nova_core::{ItemId, PlayMethod, SourceId};
    use std::sync::Mutex;

    struct RecordingSink {
        reports: Mutex<Vec<PlaybackProgress>>,
        failures: Mutex<Vec<ReportError>>,
        always_fail: Mutex<Option<ReportError>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                reports: Mutex::new(Vec::new()),
                failures: Mutex::new(Vec::new()),
                always_fail: Mutex::new(None),
            }
        }

        fn always_failing(error: ReportError) -> Self {
            let sink = Self::new();
            *lock(&sink.always_fail) = Some(error);
            sink
        }

        fn reported(&self) -> Vec<PlaybackProgress> {
            lock(&self.reports).clone()
        }

        fn count(&self) -> usize {
            lock(&self.reports).len()
        }
    }

    #[async_trait]
    impl ProgressSink for RecordingSink {
        async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
            if let Some(error) = *lock(&self.always_fail) {
                return Err(error);
            }
            let mut failures = lock(&self.failures);
            // 队列里的失败按顺序消耗一次，空队列表示"这次能成"。
            if !failures.is_empty() {
                return Err(failures.remove(0));
            }
            drop(failures);
            lock(&self.reports).push(progress.clone());
            Ok(())
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn progress(secs: u64, paused: bool) -> PlaybackProgress {
        PlaybackProgress {
            item_id: ItemId::new("item-1"),
            source_id: SourceId::new("source-1"),
            position: Duration::from_secs(secs),
            duration: Some(Duration::from_secs(600)),
            played: false,
            paused,
            method: PlayMethod::DirectPlay,
        }
    }

    fn reporter() -> (ProgressReporter, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new());
        let reporter = ProgressReporter::new(&UserPreferences::default(), clock.clone());
        (reporter, clock)
    }

    #[tokio::test]
    async fn periodic_report_follows_interval() {
        let (mut reporter, clock) = reporter();
        let sink = RecordingSink::new();
        assert_eq!(reporter.interval(), Duration::from_secs(10));

        let outcome = reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(0, false))
            .await;
        assert_eq!(outcome, ReportOutcome::Sent);
        assert_eq!(sink.count(), 1);

        // 位置变了但还没到间隔：不上报。
        clock.advance(Duration::from_secs(9));
        let outcome = reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(9, false))
            .await;
        assert_eq!(outcome, ReportOutcome::Skipped);
        assert_eq!(sink.count(), 1);

        // 满 10 秒：上报。
        clock.advance(Duration::from_secs(1));
        let outcome = reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(10, false))
            .await;
        assert_eq!(outcome, ReportOutcome::Sent);
        assert_eq!(sink.count(), 2);
        assert_eq!(sink.reported()[1].position, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn pause_seek_and_track_change_report_immediately() {
        let (mut reporter, _clock) = reporter();
        let sink = RecordingSink::new();

        reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(0, false))
            .await;
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::Pause, &progress(0, true))
                .await,
            ReportOutcome::Sent
        );
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::Seek, &progress(300, true))
                .await,
            ReportOutcome::Sent
        );
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::TrackChange, &progress(301, false))
                .await,
            ReportOutcome::Sent
        );
        assert_eq!(sink.count(), 4);
    }

    #[tokio::test]
    async fn duplicate_reports_are_deduplicated() {
        let (mut reporter, clock) = reporter();
        let sink = RecordingSink::new();
        let same = progress(120, false);

        assert_eq!(
            reporter.submit(&sink, ProgressTrigger::Tick, &same).await,
            ReportOutcome::Sent
        );
        clock.advance(Duration::from_secs(30));
        assert_eq!(
            reporter.submit(&sink, ProgressTrigger::Tick, &same).await,
            ReportOutcome::Skipped
        );
        assert_eq!(
            reporter.submit(&sink, ProgressTrigger::Seek, &same).await,
            ReportOutcome::Skipped
        );
        assert_eq!(sink.count(), 1);

        // 只变暂停状态也算变化。
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::Pause, &progress(120, true))
                .await,
            ReportOutcome::Sent
        );
        assert_eq!(sink.count(), 2);
    }

    #[tokio::test]
    async fn exit_always_reports() {
        let (mut reporter, _clock) = reporter();
        let sink = RecordingSink::new();
        let same = progress(42, false);
        reporter.submit(&sink, ProgressTrigger::Exit, &same).await;
        reporter.submit(&sink, ProgressTrigger::Exit, &same).await;
        assert_eq!(sink.count(), 2, "退出必须提交进度，不去重");
    }

    #[tokio::test]
    async fn failure_enqueues_and_retries_with_exponential_backoff() {
        let (mut reporter, clock) = reporter();
        let sink = RecordingSink::always_failing(ReportError::Transient);

        let outcome = reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(10, false))
            .await;
        assert_eq!(
            outcome,
            ReportOutcome::Queued {
                retry_in: Duration::from_secs(1)
            }
        );
        assert_eq!(reporter.queued(), 1);

        // 退避未到期：即便位置变了也不会重试（间隔内）。
        clock.advance(Duration::from_millis(500));
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::Tick, &progress(20, false))
                .await,
            ReportOutcome::Skipped
        );
        assert_eq!(reporter.queued(), 1);

        // 退避到期：重试一次（同一条进度又被拒），间隔翻倍成 2s。
        clock.advance(Duration::from_millis(500));
        let outcome = reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(30, false))
            .await;
        assert_eq!(
            outcome,
            ReportOutcome::Queued {
                retry_in: Duration::from_secs(2)
            }
        );
        assert_eq!(reporter.queued(), 1, "同一目标只保留最新一条");

        // 再来一轮：4s。
        clock.advance(Duration::from_secs(2));
        let outcome = reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(40, false))
            .await;
        assert_eq!(
            outcome,
            ReportOutcome::Queued {
                retry_in: Duration::from_secs(4)
            }
        );
    }

    #[tokio::test]
    async fn retry_after_backoff_succeeds() {
        let (mut reporter, clock) = reporter();
        let sink = RecordingSink::new();
        lock(&sink.failures).push(ReportError::Transient);

        let progress_10 = progress(10, false);
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::Tick, &progress_10)
                .await,
            ReportOutcome::Queued {
                retry_in: Duration::from_secs(1)
            }
        );

        clock.advance(Duration::from_secs(1));
        // 定时驱动：先补发队列（成功），当前进度因去重/间隔不再重复上报。
        reporter
            .submit(&sink, ProgressTrigger::Tick, &progress(20, false))
            .await;
        assert_eq!(reporter.queued(), 0);
        assert!(reporter.next_retry_at().is_none());
        assert_eq!(sink.count(), 1);
        assert_eq!(sink.reported()[0].position, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn non_retryable_failure_is_dropped() {
        let (mut reporter, _clock) = reporter();
        let sink = RecordingSink::always_failing(ReportError::Unauthorized);
        assert_eq!(
            reporter
                .submit(&sink, ProgressTrigger::Tick, &progress(10, false))
                .await,
            ReportOutcome::Dropped
        );
        assert_eq!(reporter.queued(), 0);
        assert!(!ReportError::Unauthorized.is_retryable());
        assert!(ReportError::Transient.is_retryable());
    }

    #[tokio::test]
    async fn flush_reports_pending_and_persists_failures() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let path = dir.path().join("pending-progress.json");
        let clock = Arc::new(ManualClock::new());
        let mut reporter =
            ProgressReporter::new(&UserPreferences::default(), clock).with_pending_path(&path);

        let failing = RecordingSink::always_failing(ReportError::Transient);
        reporter
            .submit(&failing, ProgressTrigger::Tick, &progress(30, false))
            .await;
        let outcome = reporter.flush(&failing).await;
        assert_eq!(outcome.sent, 0);
        assert_eq!(outcome.queued, 1);
        assert!(outcome.persisted);
        assert!(path.exists(), "仍然失败时必须落盘");

        // 下次启动：读回并补发成功，文件被删掉。
        let clock = Arc::new(ManualClock::new());
        let mut next =
            ProgressReporter::new(&UserPreferences::default(), clock).with_pending_path(&path);
        assert_eq!(next.load_pending().unwrap_or(0), 1);
        assert_eq!(next.queued(), 1);
        assert!(!path.exists(), "读回后立刻删除，避免重复补发");

        let sink = RecordingSink::new();
        let outcome = next.flush(&sink).await;
        assert_eq!(outcome.sent, 1);
        assert_eq!(outcome.queued, 0);
        assert!(!path.exists(), "补发完成后不应再留文件");
        assert_eq!(sink.reported()[0].position, Duration::from_secs(30));
        assert_eq!(sink.reported()[0].item_id.as_str(), "item-1");
    }

    #[tokio::test]
    async fn load_pending_is_noop_without_file_or_path() {
        let clock = Arc::new(ManualClock::new());
        let mut reporter = ProgressReporter::new(&UserPreferences::default(), clock.clone());
        assert_eq!(reporter.load_pending().unwrap_or(99), 0);
        assert!(reporter.pending_path().is_none());

        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let mut missing = ProgressReporter::new(&UserPreferences::default(), clock)
            .with_pending_path(dir.path().join("nope.json"));
        assert_eq!(missing.load_pending().unwrap_or(99), 0);
        assert_eq!(missing.queued(), 0);
    }

    #[tokio::test]
    async fn persist_writes_and_removes_file() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let path = dir.path().join("nested/pending-progress.json");
        let clock = Arc::new(ManualClock::new());
        let mut reporter =
            ProgressReporter::new(&UserPreferences::default(), clock).with_pending_path(&path);

        let failing = RecordingSink::always_failing(ReportError::Transient);
        reporter
            .submit(&failing, ProgressTrigger::Tick, &progress(1, false))
            .await;
        reporter
            .persist()
            .unwrap_or_else(|e| panic!("persist: {e}"));
        assert!(path.exists());
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(raw.contains("version"), "{raw}");
        assert!(raw.contains("item-1"), "{raw}");

        let sink = RecordingSink::new();
        let outcome = reporter.flush(&sink).await;
        assert_eq!(outcome.sent, 1);
        assert!(outcome.persisted);
        assert!(!path.exists(), "队列清空后删除文件");
    }

    #[test]
    fn backoff_is_capped() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(7), Duration::from_secs(60));
        assert_eq!(backoff(100), Duration::from_secs(60));
    }

    #[test]
    fn interval_comes_from_preferences_with_floor() {
        let clock = Arc::new(ManualClock::new());
        let prefs = UserPreferences {
            progress_report_interval: Duration::from_secs(2),
            ..UserPreferences::default()
        };
        let reporter = ProgressReporter::new(&prefs, clock);
        assert_eq!(reporter.interval(), Duration::from_secs(5));
    }
}
