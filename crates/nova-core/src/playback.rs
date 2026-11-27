// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放会话与进度。

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::ids::{ItemId, SourceId};

/// 播放状态机。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    /// 没有播放。
    Idle,
    /// 已发起、尚未出画。
    Loading,
    /// 缓冲中。
    Buffering,
    /// 播放中。
    Playing,
    /// 已暂停。
    Paused,
    /// 已停止（进度已提交给服务端）。
    Stopped,
    /// 失败（错误信息由调用方另行上报，不放在这里以免污染序列化数据）。
    Failed,
}

impl PlaybackStatus {
    /// 是否处于"时间在推进"的状态。
    pub fn is_active(self) -> bool {
        matches!(self, Self::Playing | Self::Buffering)
    }

    /// 是否可以安全地推进本地进度。
    pub fn can_advance_position(self) -> bool {
        self == Self::Playing
    }
}

/// 一次播放会话。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaybackSession {
    /// 播放的条目。
    pub item_id: ItemId,
    /// 播放的资源。
    pub source_id: SourceId,
    /// 当前状态。
    pub status: PlaybackStatus,
    /// 当前位置。
    pub position: Duration,
    /// 总时长（未知时为 `None`）。
    pub duration: Option<Duration>,
    /// 当前选中的音频流序号。
    pub audio_stream: Option<u32>,
    /// 当前选中的字幕流序号；`None` 表示关闭字幕。
    pub subtitle_stream: Option<u32>,
    /// 播放设备名（"客厅 Apple TV"，用于向服务端汇报会话）。
    pub device: Option<String>,
}

impl PlaybackSession {
    /// 起播。
    pub fn begin(
        item_id: ItemId,
        source_id: SourceId,
        position: Duration,
        duration: Option<Duration>,
    ) -> Self {
        Self {
            item_id,
            source_id,
            status: PlaybackStatus::Loading,
            position,
            duration,
            audio_stream: None,
            subtitle_stream: None,
            device: None,
        }
    }

    /// 剩余时长。
    pub fn remaining(&self) -> Option<Duration> {
        self.duration
            .map(|total| total.saturating_sub(self.position))
    }

    /// 观看进度比例 `0.0..=1.0`。
    pub fn fraction(&self) -> Option<f32> {
        let total = self.duration?.as_secs_f32();
        if total <= 0.0 {
            return None;
        }
        Some((self.position.as_secs_f32() / total).clamp(0.0, 1.0))
    }

    /// 是否已到达"视为看完"的阈值。
    ///
    /// 用"剩余时长"而非比例判断：片尾字幕长达数分钟，按比例 95% 会在字幕中途就标记看完。
    pub fn is_finished(&self, remaining_threshold: Duration) -> bool {
        self.remaining()
            .is_some_and(|remaining| remaining <= remaining_threshold)
    }

    /// 是否可以提交进度（只有真正播放过才提交，避免把 0 秒进度写回服务端）。
    pub fn should_report_progress(&self, min_position: Duration) -> bool {
        self.status != PlaybackStatus::Idle
            && self.status != PlaybackStatus::Failed
            && self.position >= min_position
    }
}

/// 播放方式。服务端需要它来区分"直接播放文件""直接流"与"转码"，
/// 也用它统计哪些设备在被迫转码。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayMethod {
    /// 直接播放原始文件。
    #[default]
    DirectPlay,
    /// 直接播放服务端重封装过的流（容器变了、码流没变）。
    DirectStream,
    /// 服务端转码。
    Transcode,
}

/// 要向服务端提交的播放进度。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackProgress {
    /// 会话代号：player 每次起播（`begin`）递增，用来区分"同一部片重播"与"同一次会话"。
    ///
    /// `0` 表示**不知道**（旧版补发文件里的条目）：上报器不会把它和新会话混为一谈，
    /// sink 也不会为它补发 `Started`——那个会话早就结束了。
    ///
    /// 带 `#[serde(default)]`：新增字段之前落盘的待发文件仍然能读回。
    #[serde(default)]
    pub session_seq: u64,
    /// 条目。
    pub item_id: ItemId,
    /// 资源。
    pub source_id: SourceId,
    /// 位置。
    pub position: Duration,
    /// 总时长。
    pub duration: Option<Duration>,
    /// 是否已看完。
    pub played: bool,
    /// 是否处于暂停。
    pub paused: bool,
    /// 播放方式。
    pub method: PlayMethod,
}

impl PlaybackProgress {
    /// 由会话生成待提交进度。位置会按 `duration` 截断，避免服务端收到越界值。
    pub fn from_session(session: &PlaybackSession, remaining_threshold: Duration) -> Self {
        let position = match session.duration {
            Some(total) => session.position.min(total),
            None => session.position,
        };
        Self {
            session_seq: 0,
            item_id: session.item_id.clone(),
            source_id: session.source_id.clone(),
            position,
            duration: session.duration,
            played: session.is_finished(remaining_threshold),
            paused: session.status == PlaybackStatus::Paused,
            method: PlayMethod::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(position: u64, duration: Option<u64>) -> PlaybackSession {
        let mut session = PlaybackSession::begin(
            ItemId::new("i"),
            SourceId::new("s"),
            Duration::from_secs(position),
            duration.map(Duration::from_secs),
        );
        session.status = PlaybackStatus::Playing;
        session
    }

    #[test]
    fn status_predicates() {
        assert!(PlaybackStatus::Playing.is_active());
        assert!(PlaybackStatus::Buffering.is_active());
        assert!(!PlaybackStatus::Paused.is_active());
        // 缓冲中时间戳不应推进。
        assert!(!PlaybackStatus::Buffering.can_advance_position());
        assert!(PlaybackStatus::Playing.can_advance_position());
    }

    #[test]
    fn remaining_and_fraction() {
        let s = session(30, Some(120));
        assert_eq!(s.remaining(), Some(Duration::from_secs(90)));
        assert_eq!(s.fraction(), Some(0.25));

        let no_duration = session(30, None);
        assert_eq!(no_duration.remaining(), None);
        assert_eq!(no_duration.fraction(), None);

        let over = session(500, Some(120));
        assert_eq!(over.remaining(), Some(Duration::ZERO));
        assert_eq!(over.fraction(), Some(1.0));
    }

    #[test]
    fn finished_uses_remaining_time_not_ratio() {
        // 2 小时片长，剩 4 分钟：比例上是 96.6%，但仍在片尾字幕里，不算看完。
        let in_credits = session(6960, Some(7200));
        assert!(!in_credits.is_finished(Duration::from_secs(60)));
        assert!(in_credits.is_finished(Duration::from_secs(300)));

        let finished = session(7195, Some(7200));
        assert!(finished.is_finished(Duration::from_secs(60)));
    }

    #[test]
    fn progress_reporting_thresholds() {
        let mut idle = session(0, Some(120));
        idle.status = PlaybackStatus::Idle;
        assert!(!idle.should_report_progress(Duration::from_secs(10)));

        let mut failed = session(60, Some(120));
        failed.status = PlaybackStatus::Failed;
        assert!(!failed.should_report_progress(Duration::from_secs(10)));

        assert!(!session(5, Some(120)).should_report_progress(Duration::from_secs(10)));
        assert!(session(60, Some(120)).should_report_progress(Duration::from_secs(10)));
    }

    #[test]
    fn progress_carries_pause_state() {
        let mut s = session(60, Some(120));
        s.status = PlaybackStatus::Paused;
        // 剩 60s、阈值 30s：暂停中且未看完。
        let progress = PlaybackProgress::from_session(&s, Duration::from_secs(30));
        assert!(progress.paused);
        assert!(!progress.played);
    }

    #[test]
    fn progress_clamps_position_to_duration() {
        let over = session(500, Some(120));
        let progress = PlaybackProgress::from_session(&over, Duration::from_secs(60));
        assert_eq!(progress.position, Duration::from_secs(120));
        assert!(progress.played);
        assert!(!progress.paused);
        assert_eq!(progress.method, PlayMethod::DirectPlay);
        assert_eq!(progress.item_id.as_str(), "i");
    }
}
