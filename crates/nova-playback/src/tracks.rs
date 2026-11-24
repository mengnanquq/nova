// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 轨道枚举与选择。
//!
//! 两条规则：
//!
//! 1. 轨道**列表来自引擎真实枚举**（[`EngineEvent::TracksChanged`](crate::EngineEvent)），不是从
//!    服务器元信息猜的——服务器说有三条音轨、文件里只有两条的情况是真实存在的。
//! 2. 用户**本次会话**手动切过的轨道优先于用户偏好；偏好只在没有手动选择时生效。
//!    手动选择记在 [`TrackSelector`] 里而不是 [`Session`](crate::Session) 里，这样重连、重开
//!    （新建 Session）都还能沿用同一次播放会话的选择。

use nova_core::{MediaSource, MediaStream, StreamKind, SubtitleMode, TrackChoice, UserPreferences};

/// 字幕选择。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SubtitleSelection {
    /// 关闭字幕。
    #[default]
    Off,
    /// 选用引擎内该序号的字幕流。
    Track(u32),
    /// 外挂字幕文件（本地路径，由引擎加载）。
    External {
        /// 字幕文件路径。
        path: String,
    },
}

impl SubtitleSelection {
    /// 是否关闭字幕。
    pub fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }

    /// 内嵌字幕的流序号；关闭与外挂都返回 `None`。
    pub fn track_index(&self) -> Option<u32> {
        match self {
            Self::Track(index) => Some(*index),
            Self::Off | Self::External { .. } => None,
        }
    }

    /// 外挂字幕路径。
    pub fn external_path(&self) -> Option<&str> {
        match self {
            Self::External { path } => Some(path),
            Self::Off | Self::Track(_) => None,
        }
    }
}

impl From<TrackChoice> for SubtitleSelection {
    fn from(choice: TrackChoice) -> Self {
        match choice {
            TrackChoice::Off => Self::Off,
            TrackChoice::Stream(index) => Self::Track(index),
        }
    }
}

/// 引擎报上来的一组轨道。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackSet {
    streams: Vec<MediaStream>,
}

impl TrackSet {
    /// 由引擎枚举出的流列表构造。
    pub fn new(streams: Vec<MediaStream>) -> Self {
        Self { streams }
    }

    /// 由服务端资源构造（`TracksChanged` 还没到、或引擎不支持枚举时的兜底）。
    pub fn from_source(source: &MediaSource) -> Self {
        Self::new(source.streams.clone())
    }

    /// 全部流。
    pub fn streams(&self) -> &[MediaStream] {
        &self.streams
    }

    /// 全部音频流。
    pub fn audio(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Audio)
    }

    /// 全部字幕流。
    pub fn subtitles(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Subtitle)
    }

    /// 全部视频流。
    pub fn videos(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Video)
    }

    /// 按序号取音频流。
    pub fn audio_at(&self, index: u32) -> Option<&MediaStream> {
        self.audio().find(|stream| stream.index == index)
    }

    /// 按序号取字幕流。
    pub fn subtitle_at(&self, index: u32) -> Option<&MediaStream> {
        self.subtitles().find(|stream| stream.index == index)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// 轨道数量。
    pub fn len(&self) -> usize {
        self.streams.len()
    }
}

/// 引擎报 `TracksChanged` 之后要重新应用的选择。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackPlan {
    /// 要应用的音轨序号；`None` 表示不指定（交给引擎默认轨）。
    pub audio: Option<u32>,
    /// 要应用的字幕选择。
    pub subtitle: SubtitleSelection,
}

impl TrackPlan {
    /// 是否两项都不需要动。
    pub fn is_noop(&self) -> bool {
        self.audio.is_none() && self.subtitle.is_off()
    }
}

/// 本次播放会话的轨道选择器。
///
/// 手动选择存在这里、随会话生命周期存活；用户偏好只作为**没有手动选择时**的默认值。
#[derive(Clone, Debug, Default)]
pub struct TrackSelector {
    audio: Option<u32>,
    subtitle: Option<SubtitleSelection>,
}

impl TrackSelector {
    /// 新建（没有手动选择）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 切到另一个条目 / 新的播放会话时清空手动选择。
    pub fn clear(&mut self) {
        self.audio = None;
        self.subtitle = None;
    }

    /// 记录用户手动选择的音轨。
    pub fn select_audio(&mut self, index: u32) {
        self.audio = Some(index);
    }

    /// 记录用户手动选择的字幕。
    pub fn select_subtitle(&mut self, selection: SubtitleSelection) {
        self.subtitle = Some(selection);
    }

    /// 取消音轨的手动选择（回到偏好）。
    pub fn clear_audio(&mut self) {
        self.audio = None;
    }

    /// 取消字幕的手动选择（回到偏好）。
    pub fn clear_subtitle(&mut self) {
        self.subtitle = None;
    }

    /// 当前手动指定的音轨。
    pub fn manual_audio(&self) -> Option<u32> {
        self.audio
    }

    /// 当前手动指定的字幕。
    pub fn manual_subtitle(&self) -> Option<&SubtitleSelection> {
        self.subtitle.as_ref()
    }

    /// 解析要应用的音轨：手动选择优先（若它在新轨道集里仍然存在），否则按偏好。
    pub fn resolve_audio(&self, tracks: &TrackSet, prefs: &UserPreferences) -> Option<u32> {
        if let Some(index) = self.audio
            && tracks.audio_at(index).is_some()
        {
            return Some(index);
        }
        nova_core::select_audio(tracks.streams(), prefs).map(|stream| stream.index)
    }

    /// 解析要应用的字幕：手动选择优先（若它在新轨道集里仍然存在），否则按偏好。
    pub fn resolve_subtitle(
        &self,
        tracks: &TrackSet,
        prefs: &UserPreferences,
        selected_audio: Option<&MediaStream>,
    ) -> SubtitleSelection {
        if let Some(selection) = &self.subtitle {
            let still_valid = match selection {
                SubtitleSelection::Track(index) => tracks.subtitle_at(*index).is_some(),
                // 关闭与外挂不依赖轨道枚举。
                SubtitleSelection::Off | SubtitleSelection::External { .. } => true,
            };
            if still_valid {
                return selection.clone();
            }
        }
        // 用户明确要求"从不自动开字幕"时，偏好侧直接给 Off。
        if prefs.subtitle_mode == SubtitleMode::Never {
            return SubtitleSelection::Off;
        }
        SubtitleSelection::from(nova_core::select_subtitle(
            tracks.streams(),
            prefs,
            selected_audio,
        ))
    }

    /// 引擎报 `TracksChanged` 之后要重新应用的计划。
    ///
    /// 引擎在某些情况下会重置轨道（seek、重连、重新 open），所以每次轨道集变化都要重放一遍，
    /// 否则用户刚切的字幕会悄悄变回去。
    pub fn reapply(&self, tracks: &TrackSet, prefs: &UserPreferences) -> TrackPlan {
        let audio = self.resolve_audio(tracks, prefs);
        let selected_audio = audio.and_then(|index| tracks.audio_at(index));
        let subtitle = self.resolve_subtitle(tracks, prefs, selected_audio);
        TrackPlan { audio, subtitle }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::{AudioDetails, Language};

    fn audio(index: u32, language: &str, channels: u16) -> MediaStream {
        MediaStream::audio(
            index,
            "eac3",
            AudioDetails {
                channels: Some(channels),
                sample_rate: Some(48_000),
                bitrate: None,
                profile: None,
            },
            Language::new(language),
        )
    }

    fn subtitle(index: u32, language: &str) -> MediaStream {
        MediaStream::subtitle(index, "subrip", Language::new(language))
    }

    fn tracks() -> TrackSet {
        TrackSet::new(vec![
            audio(1, "eng", 6),
            audio(2, "zh", 2),
            subtitle(3, "zh-Hans"),
            subtitle(4, "eng"),
        ])
    }

    #[test]
    fn preference_applies_when_no_manual_choice() {
        let prefs = UserPreferences::default();
        let selector = TrackSelector::new();
        let plan = selector.reapply(&tracks(), &prefs);
        assert_eq!(plan.audio, Some(2), "偏好语言 zh 应选中第 2 条音轨");
        assert_eq!(plan.subtitle, SubtitleSelection::Track(3));
    }

    #[test]
    fn manual_choice_beats_preference_and_survives_tracks_changed() {
        let prefs = UserPreferences::default();
        let mut selector = TrackSelector::new();
        selector.select_audio(1);
        selector.select_subtitle(SubtitleSelection::Track(4));

        // 重连：引擎重新枚举了一遍轨道。
        let plan = selector.reapply(&tracks(), &prefs);
        assert_eq!(plan.audio, Some(1));
        assert_eq!(plan.subtitle, SubtitleSelection::Track(4));
    }

    #[test]
    fn manual_off_beats_preference() {
        let prefs = UserPreferences::default();
        let mut selector = TrackSelector::new();
        selector.select_subtitle(SubtitleSelection::Off);
        assert_eq!(
            selector.resolve_subtitle(&tracks(), &prefs, None),
            SubtitleSelection::Off
        );
    }

    #[test]
    fn external_subtitle_is_kept_verbatim() {
        let prefs = UserPreferences::default();
        let mut selector = TrackSelector::new();
        selector.select_subtitle(SubtitleSelection::External {
            path: "/media/subs/movie.zh.srt".to_owned(),
        });
        let plan = selector.reapply(&tracks(), &prefs);
        assert_eq!(
            plan.subtitle,
            SubtitleSelection::External {
                path: "/media/subs/movie.zh.srt".to_owned()
            }
        );
        assert_eq!(
            plan.subtitle.external_path(),
            Some("/media/subs/movie.zh.srt")
        );
        assert_eq!(plan.subtitle.track_index(), None);
    }

    #[test]
    fn stale_manual_index_falls_back_to_preference() {
        let prefs = UserPreferences::default();
        let mut selector = TrackSelector::new();
        selector.select_audio(9);
        selector.select_subtitle(SubtitleSelection::Track(9));

        // 换了一个只有两条音轨、一条字幕的版本。
        let smaller = TrackSet::new(vec![
            audio(1, "eng", 6),
            audio(2, "zh", 2),
            subtitle(3, "zh-Hans"),
        ]);
        let plan = selector.reapply(&smaller, &prefs);
        assert_eq!(plan.audio, Some(2));
        assert_eq!(plan.subtitle, SubtitleSelection::Track(3));
        // 手动选择本身没被清掉：可能只是暂时回到旧版本。
        assert_eq!(selector.manual_audio(), Some(9));
        assert_eq!(
            selector.manual_subtitle(),
            Some(&SubtitleSelection::Track(9))
        );
    }

    #[test]
    fn subtitle_never_mode_gives_off() {
        let prefs = UserPreferences {
            subtitle_mode: SubtitleMode::Never,
            ..UserPreferences::default()
        };
        assert_eq!(
            TrackSelector::new().resolve_subtitle(&tracks(), &prefs, None),
            SubtitleSelection::Off
        );
    }

    #[test]
    fn track_set_helpers() {
        let set = tracks();
        assert_eq!(set.len(), 4);
        assert!(!set.is_empty());
        assert_eq!(set.audio().count(), 2);
        assert_eq!(set.subtitles().count(), 2);
        assert_eq!(set.videos().count(), 0);
        assert!(set.audio_at(2).is_some());
        assert!(set.audio_at(3).is_none(), "3 号是字幕不是音频");
        assert!(set.subtitle_at(4).is_some());
        assert!(TrackSet::default().is_empty());
    }

    #[test]
    fn subtitle_selection_accessors() {
        assert!(SubtitleSelection::Off.is_off());
        assert!(!SubtitleSelection::Track(1).is_off());
        assert_eq!(SubtitleSelection::Track(7).track_index(), Some(7));
        assert_eq!(SubtitleSelection::Off.track_index(), None);
        assert_eq!(
            SubtitleSelection::from(TrackChoice::Stream(5)),
            SubtitleSelection::Track(5)
        );
        assert_eq!(
            SubtitleSelection::from(TrackChoice::Off),
            SubtitleSelection::Off
        );
        assert!(
            TrackPlan {
                audio: None,
                subtitle: SubtitleSelection::Off
            }
            .is_noop()
        );
    }
}
