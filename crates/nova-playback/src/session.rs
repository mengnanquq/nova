// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 会话状态机（纯逻辑，无 I/O）。
//!
//! ```text
//! Idle → Opening → Playing ⇄ Paused
//!                     ↓
//!                  Buffering（回到进入前的 Playing / Paused）
//!                     ↓
//!                  Ended（位置固定为时长） / Failed（保留最后位置，可重试）
//! ```
//!
//! 两条硬规则：
//!
//! - **400 ms 转圈阈值**：[`Session::show_spinner`] 只在 `Opening` 持续满
//!   [`SPINNER_DELAY`] 后为真，避免黑底闪一下转圈。判断用的是注入的 [`Clock`]，
//!   所以测试推进 [`ManualClock`](crate::ManualClock) 就能精确验证边界。
//! - **15 秒卡死**：[`Session::tick`] 在"连续 [`STALL_TIMEOUT`] 没有任何引擎证据"
//!   时返回 [`PlaybackError::Stalled`] 并把状态置为 `Failed`。证据指位置变化、新帧、
//!   缓冲状态变化或丢帧报告——**本地插值不算**（[`Session::advance_position`] 不刷新计时器），
//!   否则进度条自己的估算会把真正的卡死掩盖掉。
//!
//! 缓冲期间不推进本地位置：缓冲中引擎给出的位置不写回，用户拖动 seek 例外（那是明确意图）。

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clock::Clock;
use crate::engine::{EngineDiagnostics, EngineEvent};
use crate::error::PlaybackError;
use crate::tracks::TrackSet;

/// `Opening` 持续多久之后才显示转圈。
pub const SPINNER_DELAY: Duration = Duration::from_millis(400);

/// 多久没有任何引擎证据就判定卡死（可重试）。
pub const STALL_TIMEOUT: Duration = Duration::from_secs(15);

/// 会话状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// 没有播放。
    Idle,
    /// 已发起打开、尚未出画。
    Opening,
    /// 播放中。
    Playing,
    /// 已暂停。
    Paused,
    /// 缓冲中（保留最后一帧）。
    Buffering,
    /// 播放到结尾。
    Ended,
    /// 失败（保留最后位置以便重试）。
    Failed,
}

impl SessionState {
    /// 是否处于"应该在动"的状态（卡死判定只覆盖这些）。
    pub fn is_active(self) -> bool {
        matches!(self, Self::Opening | Self::Playing | Self::Buffering)
    }

    /// 是否已经不会再自己前进（需要用户操作）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Idle | Self::Ended | Self::Failed)
    }

    /// 本地位置是否应该推进（只有真正播放时）。
    pub fn advances_position(self) -> bool {
        matches!(self, Self::Playing)
    }

    /// 界面是否需要显示转圈（`Opening` 由 400 ms 阈值再过滤）。
    pub fn needs_spinner(self) -> bool {
        matches!(self, Self::Opening | Self::Buffering)
    }
}

/// 一次播放会话的全部本地状态。
///
/// 不持有 `item_id` / `source_id`：打包 [`nova_core::PlaybackProgress`] 是调用方的事。
pub struct Session {
    clock: Arc<dyn Clock>,
    state: SessionState,
    position: Duration,
    duration: Option<Duration>,
    opening_since: Option<Instant>,
    last_activity: Option<Instant>,
    buffering_returns_to: SessionState,
    tracks: TrackSet,
    diagnostics: EngineDiagnostics,
    error: Option<PlaybackError>,
    dropped_frames: u64,
}

impl Session {
    /// 新建一个 `Idle` 会话。
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state: SessionState::Idle,
            position: Duration::ZERO,
            duration: None,
            opening_since: None,
            last_activity: None,
            buffering_returns_to: SessionState::Playing,
            tracks: TrackSet::default(),
            diagnostics: EngineDiagnostics::default(),
            error: None,
            dropped_frames: 0,
        }
    }

    /// 当前状态。
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// 当前位置。
    pub fn position(&self) -> Duration {
        self.position
    }

    /// 总时长（未知时为 `None`）。
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// 剩余时长。
    pub fn remaining(&self) -> Option<Duration> {
        self.duration
            .map(|total| total.saturating_sub(self.position))
    }

    /// 当前轨道集（引擎枚举结果）。
    pub fn tracks(&self) -> &TrackSet {
        &self.tracks
    }

    /// 引擎诊断。
    pub fn diagnostics(&self) -> &EngineDiagnostics {
        &self.diagnostics
    }

    /// 最近一次失败原因。
    pub fn error(&self) -> Option<&PlaybackError> {
        self.error.as_ref()
    }

    /// 累计丢帧数。
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    /// 引擎上报诊断（open 之后由调用方填）。
    pub fn set_diagnostics(&mut self, diagnostics: EngineDiagnostics) {
        self.diagnostics = diagnostics;
    }

    /// `Opening` 已经持续了多久。
    pub fn opening_elapsed(&self) -> Option<Duration> {
        let since = self.opening_since?;
        Some(self.clock.now().saturating_duration_since(since))
    }

    /// 是否显示起播转圈（`Opening` 满 400 ms 才为真）。
    pub fn show_spinner(&self) -> bool {
        self.state == SessionState::Opening
            && self
                .opening_elapsed()
                .is_some_and(|elapsed| elapsed >= SPINNER_DELAY)
    }

    /// 发起一次打开（`Idle` / `Ended` / `Failed` / 重连都可以调）。
    ///
    /// 位置会按 `duration` 截断；轨道集清空等待引擎重新枚举。
    pub fn begin(&mut self, resume_at: Duration, duration: Option<Duration>) {
        let now = self.clock.now();
        self.duration = duration;
        self.position = clamp_position(resume_at, duration);
        self.state = SessionState::Opening;
        self.opening_since = Some(now);
        self.last_activity = Some(now);
        self.buffering_returns_to = SessionState::Playing;
        self.tracks = TrackSet::default();
        self.error = None;
        self.dropped_frames = 0;
    }

    /// 重试：回到 `Opening`，**不清进度**。返回应当续播的位置。
    pub fn retry(&mut self) -> Duration {
        let position = self.position;
        let duration = self.duration;
        self.begin(position, duration);
        position
    }

    /// 回到 `Idle`（退出播放页 / 开始新会话之前的复位）。
    ///
    /// 位置与时长也要清掉：它们属于**上一部片子**。只清状态会留下
    /// “上一个会话播到哪里”的读数，错误页会把它当成当前条目的进度显示（体检 R3-3）。
    pub fn reset(&mut self) {
        self.state = SessionState::Idle;
        self.position = Duration::ZERO;
        self.duration = None;
        self.opening_since = None;
        self.last_activity = None;
        self.buffering_returns_to = SessionState::Playing;
        self.tracks = TrackSet::default();
        self.error = None;
        self.dropped_frames = 0;
    }

    /// 用户请求播放。返回 `true` 表示需要在引擎上执行 `play`。
    pub fn play(&mut self) -> bool {
        match self.state {
            SessionState::Paused => {
                self.state = SessionState::Playing;
                self.touch();
                true
            }
            SessionState::Buffering => {
                self.buffering_returns_to = SessionState::Playing;
                true
            }
            SessionState::Playing => false,
            _ => false,
        }
    }

    /// 用户请求暂停。返回 `true` 表示需要在引擎上执行 `pause`。
    pub fn pause(&mut self) -> bool {
        match self.state {
            SessionState::Playing => {
                self.state = SessionState::Paused;
                true
            }
            SessionState::Buffering => {
                self.buffering_returns_to = SessionState::Paused;
                true
            }
            _ => false,
        }
    }

    /// 播放 / 暂停切换。`Some(true)` = 该 `play`，`Some(false)` = 该 `pause`，`None` = 不可操作。
    pub fn toggle_pause(&mut self) -> Option<bool> {
        match self.state {
            SessionState::Playing => self.pause().then_some(false),
            SessionState::Paused => self.play().then_some(true),
            SessionState::Buffering => {
                // 缓冲时按一下等于"暂停"，再按一下等于"缓冲完继续"。
                let pause = self.buffering_returns_to != SessionState::Paused;
                self.buffering_returns_to = if pause {
                    SessionState::Paused
                } else {
                    SessionState::Playing
                };
                Some(!pause)
            }
            _ => None,
        }
    }

    /// 用户请求 seek。位置立即生效（乐观），返回 `true` 表示需要引擎 seek。
    ///
    /// 缓冲期间的 seek 也必须生效：用户明确要求跳转，不是"本地推进"。
    pub fn seek(&mut self, position: Duration) -> bool {
        if !matches!(
            self.state,
            SessionState::Playing | SessionState::Paused | SessionState::Buffering
        ) {
            return false;
        }
        self.position = clamp_position(position, self.duration);
        self.touch();
        true
    }

    /// 本地推进位置（给进度条做平滑插值用）。
    ///
    /// 只在 `Playing` 生效：缓冲与暂停都不推进。**不会**刷新卡死计时器——
    /// 本地插值不是引擎仍然活着的证据。
    pub fn advance_position(&mut self, delta: Duration) {
        if !self.state.advances_position() {
            return;
        }
        self.position = clamp_position(self.position.saturating_add(delta), self.duration);
    }

    /// 应用一个引擎事件。
    pub fn handle(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::FirstFrame => {
                if matches!(self.state, SessionState::Opening | SessionState::Buffering) {
                    self.state = SessionState::Playing;
                }
                // 离开 Opening 之后就不再需要转圈计时。
                self.opening_since = None;
                self.touch();
            }
            EngineEvent::FramePresented => self.touch(),
            EngineEvent::PositionChanged(position) => {
                // 只有"还在播"的状态才接受位置：缓冲期按规范不推进，Ended 后固定为时长，
                // Failed 要保留失败瞬间的位置供重试。位置报告本身仍算引擎活着的证据。
                if matches!(
                    self.state,
                    SessionState::Opening | SessionState::Playing | SessionState::Paused
                ) {
                    self.position = clamp_position(position, self.duration);
                }
                self.touch();
            }
            EngineEvent::Buffering(buffering) => {
                if buffering {
                    if matches!(
                        self.state,
                        SessionState::Opening | SessionState::Playing | SessionState::Paused
                    ) {
                        self.buffering_returns_to = if self.state == SessionState::Paused {
                            SessionState::Paused
                        } else {
                            SessionState::Playing
                        };
                        self.state = SessionState::Buffering;
                    }
                } else if self.state == SessionState::Buffering {
                    self.state = self.buffering_returns_to;
                }
                self.touch();
            }
            EngineEvent::TracksChanged(tracks) => self.tracks = tracks,
            EngineEvent::Ended => {
                self.state = SessionState::Ended;
                if let Some(total) = self.duration {
                    self.position = total;
                }
            }
            EngineEvent::Error(error) => self.fail(error),
            EngineEvent::DroppedFrames(count) => {
                self.dropped_frames = count;
                self.touch();
            }
        }
    }

    /// 标记失败（保留位置）。
    pub fn fail(&mut self, error: PlaybackError) {
        self.error = Some(error);
        self.state = SessionState::Failed;
    }

    /// 驱动卡死判定。
    ///
    /// 应由界面 / 播放线程的定时器调用（例如每 500 ms 一次）。返回 `Some(Stalled)`
    /// 表示这一拍刚刚判定卡死并已切到 `Failed`。
    pub fn tick(&mut self) -> Option<PlaybackError> {
        if !self.state.is_active() {
            return None;
        }
        let last = self.last_activity?;
        if self.clock.now().saturating_duration_since(last) >= STALL_TIMEOUT {
            let error = PlaybackError::Stalled;
            self.fail(error.clone());
            return Some(error);
        }
        None
    }

    /// 距离上次引擎证据过了多久（诊断/测试用）。
    pub fn idle_for(&self) -> Option<Duration> {
        let last = self.last_activity?;
        Some(self.clock.now().saturating_duration_since(last))
    }

    fn touch(&mut self) {
        self.last_activity = Some(self.clock.now());
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("position", &self.position)
            .field("duration", &self.duration)
            .field("tracks", &self.tracks.len())
            .field("error", &self.error)
            .finish()
    }
}

fn clamp_position(position: Duration, duration: Option<Duration>) -> Duration {
    match duration {
        Some(total) => position.min(total),
        None => position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{ManualClock, SystemClock};
    use crate::tracks::TrackSet;
    use nova_core::{ItemId, PlaybackProgress, SourceId};

    fn session() -> (Session, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new());
        (Session::new(clock.clone()), clock)
    }

    fn playing(duration_secs: u64) -> (Session, Arc<ManualClock>) {
        let (mut session, clock) = session();
        session.begin(Duration::ZERO, Some(Duration::from_secs(duration_secs)));
        session.handle(EngineEvent::FirstFrame);
        (session, clock)
    }

    #[test]
    fn full_state_machine_path() {
        let (mut session, clock) = session();
        assert_eq!(session.state(), SessionState::Idle);
        assert!(!session.show_spinner());

        session.begin(Duration::ZERO, Some(Duration::from_secs(120)));
        assert_eq!(session.state(), SessionState::Opening);

        session.handle(EngineEvent::FirstFrame);
        assert_eq!(session.state(), SessionState::Playing);

        assert!(session.pause());
        assert_eq!(session.state(), SessionState::Paused);
        assert!(session.play());
        assert_eq!(session.state(), SessionState::Playing);

        session.handle(EngineEvent::Buffering(true));
        assert_eq!(session.state(), SessionState::Buffering);
        session.handle(EngineEvent::Buffering(false));
        assert_eq!(session.state(), SessionState::Playing);

        clock.advance(Duration::from_secs(30));
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(30)));
        assert_eq!(session.position(), Duration::from_secs(30));

        session.handle(EngineEvent::Ended);
        assert_eq!(session.state(), SessionState::Ended);
        assert_eq!(session.position(), Duration::from_secs(120));
        assert!(session.state().is_terminal());

        session.reset();
        assert_eq!(session.state(), SessionState::Idle);
    }

    /// 退出播放页 / 切到新条目时 `reset()` 必须把位置与时长一起清掉。
    ///
    /// 体检 R3-3：`reset()` 只清状态与轨道，保留了上一部片子的 position / duration，
    /// 于是下一次起播在“本地缓存里没有这个条目”这种**不碰引擎**的早退路径上，
    /// 错误页显示的是上一部片子的进度（`8:37 / 1:31:23`）。
    #[test]
    fn reset_returns_to_idle_without_the_previous_position_or_duration() {
        let (mut session, _clock) = playing(600);
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(90)));
        assert_eq!(session.position(), Duration::from_secs(90));

        session.reset();

        assert_eq!(session.state(), SessionState::Idle);
        assert_eq!(
            session.position(),
            Duration::ZERO,
            "Idle 会话不该保留上一部片子的播放位置"
        );
        assert_eq!(
            session.duration(),
            None,
            "Idle 会话不该保留上一部片子的总时长"
        );
    }

    #[test]
    fn spinner_appears_only_after_400ms() {
        let (mut session, clock) = session();
        session.begin(Duration::ZERO, None);
        clock.advance(Duration::from_millis(399));
        assert!(!session.show_spinner(), "400ms 之前不许显示转圈");
        clock.advance(Duration::from_millis(1));
        assert!(session.show_spinner(), "满 400ms 才显示");
        // 出画之后永远不显示转圈。
        session.handle(EngineEvent::FirstFrame);
        assert!(!session.show_spinner());
        assert_eq!(session.opening_elapsed(), None);
    }

    #[test]
    fn spinner_not_shown_when_opening_is_fast() {
        let (mut session, clock) = session();
        session.begin(Duration::ZERO, None);
        clock.advance(Duration::from_millis(120));
        session.handle(EngineEvent::FirstFrame);
        assert!(!session.show_spinner());
        assert_eq!(session.state(), SessionState::Playing);
    }

    #[test]
    fn buffering_does_not_advance_local_position() {
        let (mut session, _clock) = playing(600);
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(10)));
        assert_eq!(session.position(), Duration::from_secs(10));

        session.handle(EngineEvent::Buffering(true));
        assert_eq!(session.state(), SessionState::Buffering);

        session.advance_position(Duration::from_secs(5));
        assert_eq!(session.position(), Duration::from_secs(10), "缓冲期不推进");

        session.handle(EngineEvent::PositionChanged(Duration::from_secs(12)));
        assert_eq!(
            session.position(),
            Duration::from_secs(10),
            "缓冲期引擎给的位置也不写回"
        );

        session.handle(EngineEvent::Buffering(false));
        assert_eq!(session.state(), SessionState::Playing);
        session.advance_position(Duration::from_secs(5));
        assert_eq!(session.position(), Duration::from_secs(15));
    }

    #[test]
    fn buffering_returns_to_paused_when_paused_first() {
        let (mut session, _clock) = playing(600);
        assert!(session.pause());
        session.handle(EngineEvent::Buffering(true));
        assert_eq!(session.state(), SessionState::Buffering);
        session.handle(EngineEvent::Buffering(false));
        assert_eq!(session.state(), SessionState::Paused);
    }

    #[test]
    fn paused_position_never_advances() {
        let (mut session, _clock) = playing(600);
        assert!(session.pause());
        session.advance_position(Duration::from_secs(5));
        assert_eq!(session.position(), Duration::ZERO);
    }

    #[test]
    fn stall_after_15s_without_engine_evidence() {
        let (mut session, clock) = playing(600);
        clock.advance(Duration::from_secs(14));
        assert_eq!(session.tick(), None);
        assert_eq!(session.state(), SessionState::Playing);

        clock.advance(Duration::from_secs(1));
        assert_eq!(session.tick(), Some(PlaybackError::Stalled));
        assert!(PlaybackError::Stalled.is_retryable());
        assert_eq!(session.state(), SessionState::Failed);
        assert_eq!(session.error(), Some(&PlaybackError::Stalled));

        // 已经 Failed 之后不再重复报。
        clock.advance(Duration::from_secs(60));
        assert_eq!(session.tick(), None);
    }

    #[test]
    fn local_interpolation_does_not_reset_the_stall_timer() {
        let (mut session, clock) = playing(600);
        for _ in 0..20 {
            clock.advance(Duration::from_secs(1));
            session.advance_position(Duration::from_secs(1));
            session.tick();
        }
        assert_eq!(
            session.state(),
            SessionState::Failed,
            "进度条自己走不能掩盖卡死"
        );
        assert_eq!(session.position(), Duration::from_secs(15));
    }

    #[test]
    fn engine_evidence_resets_the_stall_timer() {
        let (mut session, clock) = playing(600);
        clock.advance(Duration::from_secs(14));
        session.handle(EngineEvent::FramePresented);
        clock.advance(Duration::from_secs(14));
        assert_eq!(session.tick(), None);
        clock.advance(Duration::from_secs(1));
        assert_eq!(session.tick(), Some(PlaybackError::Stalled));

        // 位置变化同样是证据。
        let (mut session, clock) = playing(600);
        clock.advance(Duration::from_secs(14));
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(14)));
        clock.advance(Duration::from_secs(14));
        assert_eq!(session.tick(), None);
    }

    #[test]
    fn paused_and_idle_sessions_never_stall() {
        let (mut session, clock) = playing(600);
        assert!(session.pause());
        clock.advance(Duration::from_secs(120));
        assert_eq!(session.tick(), None);
        assert_eq!(session.state(), SessionState::Paused);

        let clock = Arc::new(ManualClock::new());
        let mut idle = Session::new(clock.clone());
        clock.advance(Duration::from_secs(120));
        assert_eq!(idle.tick(), None);
        assert_eq!(idle.state(), SessionState::Idle);
    }

    #[test]
    fn opening_also_stalls() {
        let (mut session, clock) = session();
        session.begin(Duration::ZERO, None);
        clock.advance(STALL_TIMEOUT);
        assert_eq!(session.tick(), Some(PlaybackError::Stalled));
        assert_eq!(session.state(), SessionState::Failed);
    }

    #[test]
    fn ended_locks_position_to_duration() {
        let (mut session, _clock) = playing(7200);
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(7100)));
        session.handle(EngineEvent::Ended);
        assert_eq!(session.position(), Duration::from_secs(7200));
        assert_eq!(session.remaining(), Some(Duration::ZERO));
        // Ended 之后位置固定为时长，迟到的位置报告不会把它改小。
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(7100)));
        assert_eq!(session.position(), Duration::from_secs(7200));
        assert_eq!(session.state(), SessionState::Ended);
    }

    #[test]
    fn failed_keeps_position_for_retry() {
        let (mut session, _clock) = playing(600);
        session.handle(EngineEvent::PositionChanged(Duration::from_secs(42)));
        session.handle(EngineEvent::Error(PlaybackError::Network));
        assert_eq!(session.state(), SessionState::Failed);
        assert_eq!(session.error(), Some(&PlaybackError::Network));
        assert_eq!(session.position(), Duration::from_secs(42));

        let resume = session.retry();
        assert_eq!(resume, Duration::from_secs(42));
        assert_eq!(session.state(), SessionState::Opening);
        assert_eq!(session.position(), Duration::from_secs(42));
        assert_eq!(session.error(), None);
        assert!(!session.show_spinner());
    }

    #[test]
    fn seek_clamps_and_requires_playable_state() {
        let (mut session, _clock) = session();
        assert!(!session.seek(Duration::from_secs(10)), "Idle 不接受 seek");

        session.begin(Duration::ZERO, Some(Duration::from_secs(600)));
        assert!(
            !session.seek(Duration::from_secs(10)),
            "Opening 期间不接受 seek"
        );

        session.handle(EngineEvent::FirstFrame);
        assert!(session.seek(Duration::from_secs(5000)));
        assert_eq!(
            session.position(),
            Duration::from_secs(600),
            "seek 会被时长截断"
        );

        assert!(session.seek(Duration::from_secs(90)));
        assert_eq!(session.position(), Duration::from_secs(90));
    }

    #[test]
    fn toggle_pause_round_trip() {
        let (mut session, _clock) = playing(600);
        assert_eq!(session.toggle_pause(), Some(false));
        assert_eq!(session.state(), SessionState::Paused);
        assert_eq!(session.toggle_pause(), Some(true));
        assert_eq!(session.state(), SessionState::Playing);

        let mut idle = Session::new(Arc::new(ManualClock::new()));
        assert_eq!(idle.toggle_pause(), None);
    }

    #[test]
    fn begin_clamps_resume_and_resets_tracks() {
        let (mut session, _clock) = playing(600);
        session.handle(EngineEvent::TracksChanged(TrackSet::default()));
        session.handle(EngineEvent::DroppedFrames(12));
        assert_eq!(session.dropped_frames(), 12);

        session.begin(Duration::from_secs(900), Some(Duration::from_secs(600)));
        assert_eq!(session.position(), Duration::from_secs(600));
        assert_eq!(session.dropped_frames(), 0);
        assert_eq!(session.state(), SessionState::Opening);
    }

    #[test]
    fn system_clock_session_builds_progress_without_sleep() {
        let mut session = Session::new(Arc::new(SystemClock::new()));
        session.begin(Duration::from_secs(60), Some(Duration::from_secs(600)));
        session.handle(EngineEvent::FirstFrame);
        let progress = PlaybackProgress {
            item_id: ItemId::new("i"),
            source_id: SourceId::new("s"),
            position: session.position(),
            duration: session.duration(),
            played: false,
            paused: false,
            method: nova_core::PlayMethod::DirectPlay,
        };
        assert_eq!(progress.position, Duration::from_secs(60));
        assert_eq!(session.state(), SessionState::Playing);
    }
}
