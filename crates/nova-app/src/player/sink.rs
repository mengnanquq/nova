// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 进度上报的装配：`ProgressSink` 适配器 + 后台 worker。
//!
//! `ProgressReporter` 是 `&mut self` 的异步 API，而界面线程不能为一次 HTTP 请求卡住
//! （服务端不可达时 `reqwest` 可能拖到超时）。所以上报器整个搬到 tokio 任务里：
//! 界面线程只往无界 channel 里塞"该上报了"，退出时发一条 `Flush` 并**有界地**等结果。

use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
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

/// 同一个会话最多尝试几次 `Started`（含第一次）。
///
/// 起播注册失败不该葬送这个会话：后续每一次进度上报都可以顺手重试（不阻塞、不多等），
/// 但也不能没完没了——服务端真的不可用时，多打一个请求也只是多一个失败。
const STARTED_MAX_ATTEMPTS: u32 = 3;

/// "已发过 Started"的记录。
///
/// 会话代号（[`PlaybackProgress::session_seq`]）由 player 每次 `begin()` 递增，所以
/// "比见过的最新代号大"就是"从没见过的新会话"；比它小的都是队列里补发的旧会话——
/// 那些会话（或更新的会话）已经 `stop` 过，绝不能再补 `Started`，否则服务端会把
/// 已经结束的会话一直记成在播（复审 🟡）。
#[derive(Clone, Copy, Debug, Default)]
struct SessionMark {
    /// 处理过的最新代号（发过 `Started` 或 `stop` 过）。
    latest: u64,
    /// 已经 `stop` 过的最大代号（水位）：`<= closed` 的会话都已结束。
    ///
    /// 只靠 `latest` 拦不住"代号相等但已经 stop"的会话：那个会话的 `Started`
    /// 可能还有重试次数没用完（两次 transient 失败后退出），队列补发它的 `Exit`
    /// 时会给已结束的会话再发一次 `Started`，而之后再也没人 `stop` 它（复审 🟡）。
    closed: u64,
    /// 当前代号已尝试发 `Started` 的次数（包含失败）。
    attempts: u32,
    /// 当前代号的 `Started` 是否已经成功。
    started: bool,
}

impl SessionMark {
    /// 收到一个新代号时清空上一个会话的 `Started` 记录。
    fn observe(&mut self, seq: u64) {
        if seq > self.latest {
            self.latest = seq;
            self.attempts = 0;
            self.started = false;
        }
    }
}

/// 把 `MediaProvider::report_progress` 包成 [`ProgressSink`]。
pub(crate) struct ProviderSink {
    provider: Arc<dyn MediaProvider>,
    /// 最近见过的会话代号与本会话的 `Started` 进度。
    ///
    /// 不能是进程级的 `bool`：换片后是新会话，服务端要重新收到 `Started`
    /// 才会把接下来的进度算进会话（否则 B 的续播点永远不更新）。也不能只记最近
    /// 一条会话：队列补发的旧 `Exit` 跑到新会话之后，不能再把旧会话"开始"一次
    /// （以前只记最近一条 + `stop` 后清空，就会重新发 `Started` 且再也没人 `stop`）。
    session: Mutex<SessionMark>,
}

impl ProviderSink {
    /// 由 provider 构造。
    pub(crate) fn new(provider: Arc<dyn MediaProvider>) -> Self {
        Self {
            provider,
            session: Mutex::new(SessionMark::default()),
        }
    }

    /// 这一次要不要先发 `Started`；需要时顺手记下尝试次数。
    ///
    /// 规则：只对**从没见过的代号**发；已经 `stop` 过、比最新代号旧的会话（队列补发）
    /// 永远不发；代号 `0` 是旧版补发文件里的条目，那个会话早就结束了，也不发。
    fn begin_session(&self, progress: &PlaybackProgress) -> bool {
        let seq = progress.session_seq;
        if seq == 0 {
            return false;
        }
        let mut mark = self.mark();
        // 已结束的会话（哪怕 `Started` 的重试次数还没用完）绝不能再"开始"一次。
        if seq <= mark.closed {
            return false;
        }
        mark.observe(seq);
        if seq < mark.latest || mark.started || mark.attempts >= STARTED_MAX_ATTEMPTS {
            return false;
        }
        mark.attempts = mark.attempts.saturating_add(1);
        true
    }

    /// `Started` 已经发出去（服务端收下了）。
    fn note_started(&self, seq: u64) {
        let mut mark = self.mark();
        if mark.latest == seq {
            mark.started = true;
        }
    }

    /// `Started` 失败且重试没意义（认证失效 / 被拒）：这个会话不再试。
    fn give_up_started(&self, seq: u64) {
        let mut mark = self.mark();
        if mark.latest == seq {
            mark.attempts = STARTED_MAX_ATTEMPTS;
        }
    }

    /// 会话结束：记下这个代号，之后（晚到的补发项）不再为它发 `Started`。
    ///
    /// `closed` 是水位：`stop` 之后即使队列补发的是同一个代号、`Started` 的重试次数
    /// 也还没用完，也不会再发（代号的相等拦不住，得有水位）。
    fn end_session(&self, progress: &PlaybackProgress) {
        let seq = progress.session_seq;
        let mut mark = self.mark();
        mark.observe(seq);
        mark.closed = mark.closed.max(seq);
    }

    fn mark(&self) -> MutexGuard<'_, SessionMark> {
        match self.session.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[async_trait]
impl ProgressSink for ProviderSink {
    async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
        if self.begin_session(progress) {
            // 起播注册失败不阻塞进度：下一次 Progress 会再试（本会话最多
            // `STARTED_MAX_ATTEMPTS` 次），成功前进度照常上报。
            match self.provider.report_started(progress).await {
                Ok(()) => self.note_started(progress.session_seq),
                Err(error) => {
                    tracing::debug!(%error, "起播上报失败（下一次进度上报时再试）");
                    if !is_transient(&error) {
                        self.give_up_started(progress.session_seq);
                    }
                }
            }
        }
        match self.provider.report_progress(progress).await {
            Ok(()) => Ok(()),
            // 本机没有令牌：请求根本没发出去，按“被拒”处理（不重试、也不算登录失效）。
            Err(ProviderError::NoToken) => Err(ReportError::Rejected),
            Err(ProviderError::Unauthorized) => Err(ReportError::Unauthorized),
            Err(error) if is_transient(&error) => Err(ReportError::Transient),
            Err(_) => Err(ReportError::Rejected),
        }
    }

    /// 结束会话：服务端只在收到这条时持久化续播点。
    ///
    /// 顺带记下这个代号：之后晚到的补发项不会再为它发 `Started`；同一部片重播是
    /// 一个新代号，会正常重新发 `Started`。
    async fn stop(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
        self.end_session(progress);
        match self.provider.report_stopped(progress).await {
            Ok(()) => Ok(()),
            // 同上：本机没有令牌不是服务端拒了这次上报。
            Err(ProviderError::NoToken) => Err(ReportError::Rejected),
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
    /// 测试：调用过几次**阻塞式**补发（`flush()`）。
    ///
    /// 换片路径绝不能碰到它：那里面有 `block_on` + 1500ms 上限，跑在界面线程上。
    #[cfg(test)]
    flushes: Arc<AtomicUsize>,
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
                #[cfg(test)]
                flushes: Arc::new(AtomicUsize::new(0)),
            };
        };
        Self::with_sink(local, runtime, Arc::new(ProviderSink::new(provider)))
    }

    /// 用测试注入的 sink 起 worker（与 [`Progress::start`] 同一条路径）。
    ///
    /// 换片收尾（`Exit` 进度 + `stop`）必须能被观测到，而 `ProviderSink` 要真机上
    /// 的 provider 才有意义，所以把 sink 变成可注入的。
    #[cfg(test)]
    pub(crate) fn for_test(
        local: &Arc<Local>,
        runtime: &Handle,
        sink: Arc<dyn ProgressSink>,
    ) -> Self {
        Self::with_sink(local, runtime, sink)
    }

    fn with_sink(local: &Arc<Local>, runtime: &Handle, sink: Arc<dyn ProgressSink>) -> Self {
        let prefs: UserPreferences = local.preferences.clone();
        let pending_path = local.data_dir.join("pending-progress.json");
        let (commands, mut receiver) = unbounded_channel();
        runtime.spawn(async move {
            let mut reporter = ProgressReporter::new(&prefs, Arc::new(SystemClock))
                .with_pending_path(pending_path);
            match reporter.load_pending() {
                Ok(0) => {}
                Ok(loaded) => {
                    tracing::info!(loaded, "发现上次没提交的播放进度，尝试补发");
                    let outcome = reporter.flush(sink.as_ref()).await;
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
                        let outcome = reporter.submit(sink.as_ref(), trigger, &progress).await;
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
                        let outcome = reporter.flush(sink.as_ref()).await;
                        // 接收端可能已经不等了（超时），忽略发送失败。
                        let _ = reply.send(outcome);
                    }
                    Command::Shutdown => {
                        let outcome = reporter.flush(sink.as_ref()).await;
                        tracing::debug!(?outcome, "退出前补发播放进度");
                        break;
                    }
                }
            }
        });
        Self {
            commands: Some(commands),
            runtime: runtime.clone(),
            #[cfg(test)]
            flushes: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 提交一次进度（非阻塞）。返回是否**真的排进了**上报队列：`false` 表示
    /// 上报通道已经关掉（进程正在退出），这次提交不会到达服务端。
    pub(crate) fn submit(&self, trigger: ProgressTrigger, progress: PlaybackProgress) -> bool {
        let Some(commands) = &self.commands else {
            return false;
        };
        commands.send(Command::Submit(trigger, progress)).is_ok()
    }

    /// 退出前尽力补发；最多等 [`FLUSH_TIMEOUT`]。
    ///
    /// **只给离开播放页 / 进程退出用**：它 `block_on` 在界面线程上。换片
    /// （[`Player::begin`](crate::player::Player)）只 `submit`，不等补发。
    pub(crate) fn flush(&self) -> Option<FlushOutcome> {
        #[cfg(test)]
        self.flushes.fetch_add(1, Ordering::SeqCst);
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

    /// 测试：`flush()` 被调用过几次。换片路径必须是 0。
    #[cfg(test)]
    pub(crate) fn flush_count(&self) -> usize {
        self.flushes.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::{ItemId, MediaItem, MediaSource, ServerInfo, ServerKind, SourceId};
    use nova_provider::{ImageRequest, ItemQuery, Library, Page, StreamTarget};

    #[test]
    fn client_errors_are_not_retried() {
        assert!(is_transient(&ProviderError::Network("timeout".into())));
        assert!(is_transient(&ProviderError::Http { status: 503 }));
        assert!(is_transient(&ProviderError::Http { status: 429 }));
        assert!(!is_transient(&ProviderError::Http { status: 400 }));
        assert!(!is_transient(&ProviderError::Unauthorized));
        assert!(!is_transient(&ProviderError::NotFound));
    }

    /// 只记录三个上报端点，其余方法测试用不到。
    #[derive(Default)]
    struct FakeProvider {
        started: Mutex<Vec<String>>,
        stopped: Mutex<Vec<String>>,
        progress: Mutex<Vec<String>>,
        /// 接下来几次 `report_started` 要失败（模拟 transient）。
        start_failures: Mutex<u32>,
        /// `report_started` 一共被调用了几次（含失败）。
        start_attempts: Mutex<u32>,
    }

    impl FakeProvider {
        /// 前 `times` 次起播注册失败（`Network`，transient）。
        fn failing_starts(times: u32) -> Self {
            let provider = Self::default();
            match provider.start_failures.lock() {
                Ok(mut guard) => *guard = times,
                Err(poisoned) => *poisoned.into_inner() = times,
            }
            provider
        }

        fn start_attempts(&self) -> u32 {
            match self.start_attempts.lock() {
                Ok(guard) => *guard,
                Err(poisoned) => *poisoned.into_inner(),
            }
        }

        fn calls(calls: &Mutex<Vec<String>>) -> Vec<String> {
            match calls.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }

        fn started(&self) -> Vec<String> {
            Self::calls(&self.started)
        }

        fn stopped(&self) -> Vec<String> {
            Self::calls(&self.stopped)
        }

        fn progress_count(&self) -> usize {
            Self::calls(&self.progress).len()
        }

        fn recorded(calls: &Mutex<Vec<String>>, progress: &PlaybackProgress) {
            match calls.lock() {
                Ok(mut guard) => guard.push(progress.item_id.to_string()),
                Err(poisoned) => poisoned.into_inner().push(progress.item_id.to_string()),
            }
        }
    }

    #[async_trait]
    impl MediaProvider for FakeProvider {
        fn kind(&self) -> ServerKind {
            ServerKind::Jellyfin
        }

        fn base_url(&self) -> &str {
            "http://127.0.0.1:1"
        }

        fn server_info(&self) -> ServerInfo {
            unreachable!("测试只关心上报")
        }

        async fn public_info_as_server_info(&self) -> Result<ServerInfo, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn public_info(&self) -> Result<nova_provider::PublicServerInfo, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn authenticate(
            &self,
            _user: &str,
            _password: &nova_secrets::SecretString,
        ) -> Result<ServerInfo, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn libraries(&self) -> Result<Vec<Library>, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn items(&self, _query: ItemQuery) -> Result<Page<MediaItem>, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn item(&self, _id: &ItemId) -> Result<MediaItem, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn children(&self, _id: &ItemId) -> Result<Vec<MediaItem>, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn sources(&self, _id: &ItemId) -> Result<Vec<MediaSource>, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn stream_target(
            &self,
            _item: &ItemId,
            _source: &SourceId,
        ) -> Result<StreamTarget, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn report_started(&self, progress: &PlaybackProgress) -> Result<(), ProviderError> {
            match self.start_attempts.lock() {
                Ok(mut attempts) => *attempts += 1,
                Err(poisoned) => *poisoned.into_inner() += 1,
            }
            let mut failures = match self.start_failures.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *failures > 0 {
                *failures -= 1;
                return Err(ProviderError::Network("timeout".into()));
            }
            drop(failures);
            Self::recorded(&self.started, progress);
            Ok(())
        }

        async fn report_stopped(&self, progress: &PlaybackProgress) -> Result<(), ProviderError> {
            Self::recorded(&self.stopped, progress);
            Ok(())
        }

        async fn report_progress(&self, progress: &PlaybackProgress) -> Result<(), ProviderError> {
            Self::recorded(&self.progress, progress);
            Ok(())
        }

        async fn image(&self, _request: ImageRequest) -> Result<Vec<u8>, ProviderError> {
            unreachable!("测试只关心上报")
        }

        async fn logout(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn progress_at(item: &str, seq: u64, secs: u64) -> PlaybackProgress {
        PlaybackProgress {
            session_seq: seq,
            item_id: ItemId::new(item),
            source_id: SourceId::new(format!("source-{item}")),
            position: Duration::from_secs(secs),
            duration: Some(Duration::from_secs(600)),
            played: false,
            paused: false,
            method: nova_core::PlayMethod::DirectPlay,
        }
    }

    /// 旧版补发文件里的进度：没有会话代号（`0`）。
    fn legacy_progress(item: &str, secs: u64) -> PlaybackProgress {
        progress_at(item, 0, secs)
    }

    async fn report(sink: &ProviderSink, item: &str, seq: u64, secs: u64) {
        sink.report(&progress_at(item, seq, secs))
            .await
            .unwrap_or_else(|error| panic!("上报失败：{error:?}"));
    }

    async fn stop(sink: &ProviderSink, item: &str, seq: u64, secs: u64) {
        sink.stop(&progress_at(item, seq, secs))
            .await
            .unwrap_or_else(|error| panic!("结束会话失败：{error:?}"));
    }

    /// 复审 🟡4：`started` 曾经是进程级的 `AtomicBool`，换片后 B 永远不会再发
    /// `Started`（`Sessions/Playing`），服务端于是不把 B 的进度算进会话。
    ///
    /// 现在按会话代号记录（`0` 除外）。
    #[tokio::test]
    async fn every_new_session_reports_started() {
        let provider = Arc::new(FakeProvider::default());
        let sink = ProviderSink::new(Arc::clone(&provider) as Arc<dyn MediaProvider>);

        // A 的会话（代号 1）：第一条上报先发 Started，后续不重复。
        report(&sink, "a", 1, 10).await;
        assert_eq!(provider.started(), ["a"], "A 的第一条上报必须是 Started");
        report(&sink, "a", 1, 20).await;
        assert_eq!(provider.started(), ["a"], "同一会话不重复发 Started");

        // A 退出：`stop` 之后会话结束。
        report(&sink, "a", 1, 30).await;
        stop(&sink, "a", 1, 30).await;
        assert_eq!(provider.stopped(), ["a"]);

        // 换片 B（代号 2）：第一条上报仍然要是 Started。
        report(&sink, "b", 2, 1).await;
        assert_eq!(
            provider.started(),
            ["a", "b"],
            "换片后新会话必须重新发 Started"
        );

        // 同一部片重播（stop 之后、新代号）也算新会话。
        stop(&sink, "b", 2, 1).await;
        report(&sink, "b", 3, 2).await;
        assert_eq!(provider.started(), ["a", "b", "b"]);
    }

    /// 复审 🟡2：drain 补发一个已经 `stop` 过的会话的 `Exit` 时，不能再发 `Started`——
    /// 否则那个会话（服务端刚收到 `stop`）又被记成在播，而之后再也没人 `stop` 它。
    #[tokio::test]
    async fn drained_exit_does_not_restart_a_stopped_session() {
        let provider = Arc::new(FakeProvider::default());
        let sink = ProviderSink::new(Arc::clone(&provider) as Arc<dyn MediaProvider>);

        // A 播放中（代号 1）：Started 已发。
        report(&sink, "a", 1, 30).await;
        assert_eq!(provider.started(), ["a"]);

        // A 退出：`stop` 先到（失败的那条 `Exit` 留在队列里，稍后补发）。
        stop(&sink, "a", 1, 30).await;
        assert_eq!(provider.stopped(), ["a"]);

        // 补发旧 `Exit`：进度照发，但绝不能再发 Started。
        report(&sink, "a", 1, 31).await;
        assert_eq!(
            provider.started(),
            ["a"],
            "已经 stop 的会话不得再发 Started"
        );

        // 重播（代号 2）仍然是新会话：正常重新发 Started。
        report(&sink, "a", 2, 0).await;
        assert_eq!(provider.started(), ["a", "a"]);
    }

    /// 💡：Started 遇到 transient 失败时，在下一次 report 里重试（不阻塞），
    /// 但同一个会话最多试 `STARTED_MAX_ATTEMPTS` 次。
    #[tokio::test]
    async fn transient_started_failure_is_retried_on_the_next_report() {
        let provider = Arc::new(FakeProvider::failing_starts(1));
        let sink = ProviderSink::new(Arc::clone(&provider) as Arc<dyn MediaProvider>);

        // 第一次：Started 失败，进度照常上报。
        report(&sink, "a", 1, 10).await;
        assert!(provider.started().is_empty(), "失败那次不算已发");
        assert_eq!(provider.progress_count(), 1);

        // 下一次 report 顺手重试：成功，只补发一次。
        report(&sink, "a", 1, 20).await;
        assert_eq!(provider.started(), ["a"], "transient 失败要重试并成功");
        report(&sink, "a", 1, 30).await;
        assert_eq!(provider.started(), ["a"], "成功之后不再重复");
    }

    /// 起播注册一直失败时，本会话最多试 `STARTED_MAX_ATTEMPTS` 次，不会变成每次都发。
    #[tokio::test]
    async fn started_retries_are_capped_per_session() {
        let provider = Arc::new(FakeProvider::failing_starts(u32::MAX));
        let sink = ProviderSink::new(Arc::clone(&provider) as Arc<dyn MediaProvider>);

        for secs in 0..10 {
            report(&sink, "a", 1, secs).await;
        }
        assert_eq!(provider.start_attempts(), STARTED_MAX_ATTEMPTS);
        assert!(provider.started().is_empty());
    }

    /// 复审 🟡：会话 N 的 `Started` 两次 transient 失败（`attempts = 2`，还没到上限），
    /// `Exit` 的进度也失败进了队列；`stop(N)` 成功之后补发那条 `Exit`——按代号比对
    /// 拦不住（相等、且重试次数没满），于是给一个已经 stop 的会话又发了一次 `Started`，
    /// 之后再也没人 `stop` 它，服务端一直把它记成在播。
    ///
    /// 修法：`stop` 记下 `closed` 水位，`begin_session` 见到 `seq <= closed` 直接拒绝。
    #[tokio::test]
    async fn stopped_session_is_never_started_again_with_attempts_left() {
        let provider = Arc::new(FakeProvider::failing_starts(2));
        let sink = ProviderSink::new(Arc::clone(&provider) as Arc<dyn MediaProvider>);

        // 会话 1 的 Started 连着失败两次（attempts = 2 < STARTED_MAX_ATTEMPTS）。
        report(&sink, "a", 1, 10).await;
        report(&sink, "a", 1, 20).await;
        assert_eq!(provider.start_attempts(), 2);
        assert!(provider.started().is_empty());

        // 退出：Exit 的进度进了队列，stop 成功——这个会话结束了。
        stop(&sink, "a", 1, 30).await;
        assert_eq!(provider.stopped(), ["a"]);

        // 队列补发那条 Exit：进度照发，但绝不能再给已 stop 的会话发 Started
        // （哪怕重试次数还没用完——失败的那两次没让 attempts 到上限）。
        report(&sink, "a", 1, 30).await;
        assert_eq!(
            provider.start_attempts(),
            2,
            "已经 stop 的会话不得再尝试 Started"
        );
        assert!(provider.started().is_empty());
        assert_eq!(provider.progress_count(), 3, "进度本身照常补发");

        // 水位只压住"已结束的代号"：重播是新代号，Started 照发。
        report(&sink, "a", 2, 0).await;
        assert_eq!(provider.started(), ["a"], "重播的新会话仍然要发 Started");
    }

    /// 落盘兼容：旧版补发文件里的条目没有会话代号（`0`），那个会话早就结束了，
    /// 补发它时绝不能再发 Started。
    #[tokio::test]
    async fn legacy_progress_never_starts_a_session() {
        let provider = Arc::new(FakeProvider::default());
        let sink = ProviderSink::new(Arc::clone(&provider) as Arc<dyn MediaProvider>);

        sink.report(&legacy_progress("a", 30))
            .await
            .unwrap_or_else(|error| panic!("上报失败：{error:?}"));
        assert!(provider.started().is_empty(), "没有代号就不补 Started");
        assert_eq!(provider.progress_count(), 1);
    }
}
