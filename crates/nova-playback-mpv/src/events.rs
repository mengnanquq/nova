// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! mpv 观察到的属性 → [`EngineEvent`] 的映射（纯函数，全部可单测）。
//!
//! 这一层刻意不碰 FFI：`ffi` 模块把事件体拷贝成 [`MpvEvent`]/[`RawTrack`] 之后，
//! 这里只做判定。这样「喂一个构造好的事件体，断言发出的 EngineEvent」不需要 GL、
//! 不需要 mpv、不需要显示会话。

use std::time::Duration;

use nova_core::{AudioDetails, Language, MediaStream, StreamKind, SubtitleDetails, VideoDetails};
use nova_playback::{EngineEvent, TrackSet};

use crate::ffi::{RawTrack, TrackKind};

/// 字幕正文字幕的默认帧率估计（mpv 没给 `demux-fps` 时用）。
const UNKNOWN_FRAMERATE: f32 = 0.0;

/// 把 `track-list` 转成 nova 的 [`TrackSet`]。
///
/// `index` 直接用 mpv 的 track id：`TracksChanged` 出去的序号必须能原样丢回
/// `select_audio`/`select_subtitle`（`sid=3`/`aid=1`），否则界面选了轨道也切不过去。
pub(crate) fn track_set(tracks: &[RawTrack]) -> TrackSet {
    let streams = tracks
        .iter()
        .map(|track| {
            let language = track
                .lang
                .as_deref()
                .map(Language::new)
                .unwrap_or_else(Language::undetermined);
            let mut stream = match track.kind {
                TrackKind::Video => MediaStream::video(
                    track.id,
                    track.codec.clone(),
                    VideoDetails {
                        width: track.width.unwrap_or(0),
                        height: track.height.unwrap_or(0),
                        frame_rate: track.frame_rate.filter(|fps| *fps > UNKNOWN_FRAMERATE),
                        bit_depth: None,
                        hdr: None,
                        interlaced: false,
                    },
                    language,
                ),
                TrackKind::Audio => MediaStream::audio(
                    track.id,
                    track.codec.clone(),
                    AudioDetails {
                        channels: track.channels,
                        sample_rate: track.sample_rate,
                        bitrate: None,
                        profile: None,
                    },
                    language,
                ),
                TrackKind::Subtitle | TrackKind::Other => {
                    let mut stream = MediaStream::subtitle(track.id, track.codec.clone(), language);
                    if let Some(details) = stream.subtitle.as_mut() {
                        *details = SubtitleDetails {
                            format: Some(track.codec.clone()).filter(|codec| !codec.is_empty()),
                            hearing_impaired: false,
                        };
                    }
                    stream
                }
            };
            stream.title = track.title.clone();
            stream.is_default = track.is_default;
            stream.is_forced = track.is_forced;
            stream.is_external = track.is_external;
            // `Other` 类型（mpv 将来新增的轨道类型）不当成字幕冒出来。
            if track.kind == TrackKind::Other {
                stream.kind = StreamKind::Subtitle;
            }
            stream
        })
        .collect();
    TrackSet::new(streams)
}

/// 缓冲判定的输入（全部来自观察到的属性）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BufferSignals {
    pub paused: bool,
    pub core_idle: bool,
    /// `cache-buffering-state`：0–100 的百分比；`None` 表示属性不可用。
    pub cache_percent: Option<f64>,
    pub eof: bool,
}

/// 现在是否处于「缓冲中」。
///
/// 判据（三条都要看，单看任何一条都会误报）：
///
/// - `cache-buffering-state < 99`：网络侧真的没喂饱（本地文件恒为 100）。
/// - `core-idle`：core 没有可播的数据（起播前、seek 后、断流都会置位）。
/// - 暂停 / EOF 时**永远不算缓冲**：暂停时 `core-idle` 也常常为真，把它们算成缓冲会让
///   界面在用户按暂停时转圈。
///
/// 起播阶段（还没有第一帧、`core-idle=true`）会先报 `Buffering(true)`，等首帧到了再
/// `Buffering(false)` —— 这正是 `Session` 期望的顺序（400 ms 转圈阈值由它自己过滤）。
pub(crate) fn is_buffering(signals: BufferSignals) -> bool {
    if signals.paused || signals.eof {
        return false;
    }
    if let Some(percent) = signals.cache_percent {
        if percent < 99.0 {
            return true;
        }
    }
    // cache 有 100% 也可能 core-idle（seek 之后、起播前、EOF 之后的重配），这时确实在等。
    signals.core_idle
}

/// mpv 的秒数属性 → `Duration`（负数、NaN、inf 一律丢弃）。
pub(crate) fn seconds_to_duration(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(seconds))
}

/// 观察到的属性 → 要发给上层的 [`EngineEvent`]。
///
/// 返回值是 `Option`：`pause`/`core-idle`/`cache-buffering-state` 这类属性本身不产生
/// 事件，只是改变 [`BufferSignals`]；只有「缓冲状态翻转」才会发出 `Buffering`。
pub(crate) fn event_for(property: ObservedProperty) -> Option<EngineEvent> {
    match property {
        ObservedProperty::TimePos(seconds) => {
            seconds_to_duration(seconds).map(EngineEvent::PositionChanged)
        }
        ObservedProperty::Buffering(buffering) => Some(EngineEvent::Buffering(buffering)),
        ObservedProperty::Tracks(tracks) => Some(EngineEvent::TracksChanged(track_set(&tracks))),
        ObservedProperty::Eof(true) => Some(EngineEvent::Ended),
        ObservedProperty::Eof(false) => None,
        ObservedProperty::DroppedFrames(count) => Some(EngineEvent::DroppedFrames(count)),
        // pause 在 nova-playback 的 EngineEvent 里没有对应项：暂停是**上层**的状态
        // （Session::pause 已经记录了），引擎只需要把它用于缓冲判定与 watchdog。
        ObservedProperty::Pause(_) | ObservedProperty::CoreIdle(_) | ObservedProperty::Cache(_) => {
            None
        }
    }
}

/// 已经归一化的一条属性观察结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ObservedProperty {
    TimePos(f64),
    /// 缓冲状态翻转（由 [`is_buffering`] 算出来，不是 mpv 的属性）。
    Buffering(bool),
    Pause(bool),
    CoreIdle(bool),
    Cache(Option<f64>),
    Tracks(Vec<RawTrack>),
    Eof(bool),
    DroppedFrames(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: u32) -> RawTrack {
        RawTrack {
            id,
            kind: TrackKind::Video,
            codec: "hevc".into(),
            lang: Some("und".into()),
            title: None,
            is_default: true,
            is_forced: false,
            is_external: false,
            width: Some(3840),
            height: Some(2160),
            channels: None,
            sample_rate: None,
            frame_rate: Some(23.976),
        }
    }

    fn audio_track(id: u32, lang: &str, channels: u16) -> RawTrack {
        RawTrack {
            id,
            kind: TrackKind::Audio,
            codec: "eac3".into(),
            lang: Some(lang.into()),
            title: Some("国语 5.1".into()),
            is_default: false,
            is_forced: false,
            is_external: false,
            width: None,
            height: None,
            channels: Some(channels),
            sample_rate: Some(48_000),
            frame_rate: None,
        }
    }

    fn subtitle_track(id: u32, lang: &str, external: bool) -> RawTrack {
        RawTrack {
            id,
            kind: TrackKind::Subtitle,
            codec: "subrip".into(),
            lang: Some(lang.into()),
            title: None,
            is_default: false,
            is_forced: true,
            is_external: external,
            width: None,
            height: None,
            channels: None,
            sample_rate: None,
            frame_rate: None,
        }
    }

    #[test]
    fn track_list_maps_to_nova_streams_with_mpv_ids() {
        let set = track_set(&[
            video(1),
            audio_track(2, "eng", 6),
            audio_track(3, "zh", 2),
            subtitle_track(4, "zh-Hans", true),
        ]);
        assert_eq!(set.len(), 4);
        assert_eq!(set.videos().count(), 1);
        assert_eq!(set.audio().count(), 2);
        assert_eq!(set.subtitles().count(), 1);
        // 序号保持 mpv 的 id：界面回传的就是 sid/aid。
        assert!(set.audio_at(3).is_some());
        assert!(set.subtitle_at(4).is_some());
        let video = set.videos().next().expect("有视频轨");
        assert_eq!(video.codec, "hevc");
        assert_eq!(
            video
                .video
                .as_ref()
                .map(|details| (details.width, details.height)),
            Some((3840, 2160))
        );
        assert!(video.video.as_ref().and_then(|d| d.frame_rate).is_some());
        let audio = set.audio_at(2).expect("第 2 条音轨");
        assert_eq!(audio.audio.as_ref().and_then(|d| d.channels), Some(6));
        assert_eq!(audio.title.as_deref(), Some("国语 5.1"));
        let subtitle = set.subtitle_at(4).expect("第 4 条字幕");
        assert!(subtitle.is_forced);
        assert!(subtitle.is_external);
        assert_eq!(
            subtitle.subtitle.as_ref().and_then(|d| d.format.as_deref()),
            Some("subrip")
        );
    }

    #[test]
    fn unknown_language_and_missing_fps_do_not_panic() {
        let mut track = video(1);
        track.lang = None;
        track.frame_rate = Some(0.0);
        let set = track_set(&[track]);
        let video = set.videos().next().expect("有视频轨");
        assert_eq!(video.language, Language::undetermined());
        assert_eq!(video.video.as_ref().and_then(|d| d.frame_rate), None);
    }

    #[test]
    fn time_pos_maps_to_position_and_filters_garbage() {
        assert_eq!(
            event_for(ObservedProperty::TimePos(12.5)),
            Some(EngineEvent::PositionChanged(Duration::from_secs_f64(12.5)))
        );
        assert_eq!(event_for(ObservedProperty::TimePos(-1.0)), None);
        assert_eq!(event_for(ObservedProperty::TimePos(f64::NAN)), None);
        assert_eq!(event_for(ObservedProperty::TimePos(f64::INFINITY)), None);
    }

    #[test]
    fn pause_and_idle_do_not_emit_events_but_drive_buffering() {
        assert_eq!(event_for(ObservedProperty::Pause(true)), None);
        assert_eq!(event_for(ObservedProperty::CoreIdle(true)), None);
        assert_eq!(event_for(ObservedProperty::Cache(Some(30.0))), None);

        let playing = BufferSignals {
            paused: false,
            core_idle: false,
            cache_percent: Some(100.0),
            eof: false,
        };
        assert!(!is_buffering(playing));
        // cache 掉到 100 以下 → 缓冲。
        assert!(is_buffering(BufferSignals {
            cache_percent: Some(30.0),
            ..playing
        }));
        // core-idle（seek 后/断流）也算缓冲。
        assert!(is_buffering(BufferSignals {
            core_idle: true,
            ..playing
        }));
        // 暂停时 core-idle 常常为真，但绝不算缓冲（否则按暂停就转圈）。
        assert!(!is_buffering(BufferSignals {
            paused: true,
            core_idle: true,
            ..playing
        }));
        // EOF 之后不算缓冲。
        assert!(!is_buffering(BufferSignals {
            eof: true,
            core_idle: true,
            ..playing
        }));
        // 起播阶段（还没画面、core-idle）算缓冲：Session 的 400 ms 阈值会过滤掉闪烁。
        assert!(is_buffering(BufferSignals {
            core_idle: true,
            ..playing
        }));
        // 但没有 cache 属性也没有 core-idle（例如纯音频正常播放）就不算缓冲。
        assert!(!is_buffering(BufferSignals {
            cache_percent: None,
            ..playing
        }));
    }

    #[test]
    fn buffering_emits_only_on_transitions() {
        assert_eq!(
            event_for(ObservedProperty::Buffering(true)),
            Some(EngineEvent::Buffering(true))
        );
        assert_eq!(
            event_for(ObservedProperty::Buffering(false)),
            Some(EngineEvent::Buffering(false))
        );
        assert_eq!(
            event_for(ObservedProperty::Eof(true)),
            Some(EngineEvent::Ended)
        );
        assert_eq!(event_for(ObservedProperty::Eof(false)), None);
        assert_eq!(
            event_for(ObservedProperty::DroppedFrames(7)),
            Some(EngineEvent::DroppedFrames(7))
        );
        assert_eq!(
            event_for(ObservedProperty::Tracks(vec![video(1)])),
            Some(EngineEvent::TracksChanged(track_set(&[video(1)])))
        );
    }
}
