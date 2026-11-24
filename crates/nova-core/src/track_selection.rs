// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 按用户偏好挑选音轨与字幕。
//!
//! 这是纯函数逻辑：输入是资源里的流列表 + 用户偏好，输出要选哪条流。
//! 放在 core 里是因为它最容易出错——错配字幕（日语片配了中文字幕、听障字幕混进正常字幕）
//! 是媒体中心最常见的用户抱怨，必须有测试覆盖。

use crate::language::Language;
use crate::prefs::{SubtitleMode, UserPreferences};
use crate::stream::{MediaStream, StreamKind};

/// 字幕选择结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackChoice {
    /// 关闭字幕。
    Off,
    /// 选用该序号的流。
    Stream(u32),
}

impl TrackChoice {
    /// 是否关闭。
    pub fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }

    /// 选中的流序号。
    pub fn index(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Stream(index) => Some(index),
        }
    }
}

/// 挑选音轨：按 `audio_languages` 的顺序找语言匹配的流，同语言内（若用户偏好环绕声）优先
/// 环绕声，其次服务端标记的默认轨，外部音轨降权。
///
/// 环绕声排在默认轨之前是有意的：Emby / Jellyfin 的 `default` 标记基本等价于"文件里的第一条"，
/// 而用户勾了"优先环绕声"就是明确表达了他想要 5.1 而不是同语言的立体声。
pub fn select_audio<'a>(
    streams: &'a [MediaStream],
    prefs: &UserPreferences,
) -> Option<&'a MediaStream> {
    let candidates: Vec<&MediaStream> = streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
        .collect();
    if candidates.is_empty() {
        return None;
    }

    for preferred in &prefs.audio_languages {
        let matched: Vec<&MediaStream> = candidates
            .iter()
            .copied()
            .filter(|stream| stream.language.matches(preferred))
            .collect();
        if let Some(best) = pick_audio(&matched, prefs) {
            return Some(best);
        }
    }

    pick_audio(&candidates, prefs)
}

/// 挑选字幕。语言匹配失败时宁可返回 [`TrackChoice::Off`]，也不去猜一条语言不符的字幕。
pub fn select_subtitle(
    streams: &[MediaStream],
    prefs: &UserPreferences,
    selected_audio: Option<&MediaStream>,
) -> TrackChoice {
    if prefs.subtitle_mode == SubtitleMode::Never {
        return TrackChoice::Off;
    }

    let subtitles: Vec<&MediaStream> = streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Subtitle)
        .collect();
    if subtitles.is_empty() {
        return TrackChoice::Off;
    }

    // 只要外语音轨时开字幕：音轨本身就是偏好语言，就不开。
    if prefs.subtitle_mode == SubtitleMode::ForeignAudioOnly {
        let audio_is_preferred = selected_audio
            .is_some_and(|audio| is_preferred_language(&audio.language, &prefs.audio_languages));
        if audio_is_preferred {
            return TrackChoice::Off;
        }
    }

    let allowed: Vec<&MediaStream> = subtitles
        .iter()
        .copied()
        .filter(|stream| prefs.allow_hearing_impaired || !is_hearing_impaired(stream))
        .collect();

    let (pool, include_forced): (Vec<&MediaStream>, bool) = match prefs.subtitle_mode {
        SubtitleMode::Forced => (
            allowed.iter().copied().filter(|s| s.is_forced).collect(),
            false,
        ),
        _ => (
            allowed.iter().copied().filter(|s| !s.is_forced).collect(),
            true,
        ),
    };

    if let Some(found) = match_by_language(&pool, &prefs.subtitle_languages) {
        return TrackChoice::Stream(found.index);
    }

    // 正常编排下强制字幕是嵌在视频里的"外语对白"轨，只有在没有普通字幕时才退而求其次。
    if include_forced {
        let forced: Vec<&MediaStream> = allowed.iter().copied().filter(|s| s.is_forced).collect();
        if let Some(found) = match_by_language(&forced, &prefs.subtitle_languages) {
            return TrackChoice::Stream(found.index);
        }
    }

    TrackChoice::Off
}

fn pick_audio<'a>(
    candidates: &[&'a MediaStream],
    prefs: &UserPreferences,
) -> Option<&'a MediaStream> {
    candidates
        .iter()
        .copied()
        .max_by_key(|stream| audio_score(stream, prefs))
}

fn audio_score(stream: &MediaStream, prefs: &UserPreferences) -> i32 {
    let mut score = 0;
    if stream.is_default {
        score += 2;
    }
    if prefs.prefer_surround_audio
        && stream
            .audio
            .as_ref()
            .is_some_and(|details| details.is_surround())
    {
        score += 4;
    }
    // 外部音轨更容易失步或延迟，只在没有内嵌轨时才用。
    if stream.is_external {
        score -= 8;
    }
    score
}

fn match_by_language<'a>(
    candidates: &[&'a MediaStream],
    languages: &[Language],
) -> Option<&'a MediaStream> {
    for language in languages {
        let matched: Vec<&MediaStream> = candidates
            .iter()
            .copied()
            .filter(|stream| stream.language.matches(language))
            .collect();
        if let Some(best) = matched
            .iter()
            .copied()
            .max_by_key(|stream| subtitle_score(stream))
        {
            return Some(best);
        }
    }

    // 语言未标注的流：只有在完全没有语言信息可用时才作为兜底，
    // 且仅当它被服务端标为默认轨（此时服务端的意图比我们的猜测更可靠）。
    candidates
        .iter()
        .copied()
        .find(|stream| stream.language.is_undetermined() && stream.is_default)
}

fn subtitle_score(stream: &MediaStream) -> i32 {
    let mut score = 0;
    if stream.is_default {
        score += 2;
    }
    // 外挂字幕通常是用户自己调好的，质量往往好于内嵌轨。
    if stream.is_external {
        score += 1;
    }
    if !is_hearing_impaired(stream) {
        score += 1;
    }
    score
}

fn is_hearing_impaired(stream: &MediaStream) -> bool {
    stream
        .subtitle
        .as_ref()
        .is_some_and(|details| details.hearing_impaired)
}

fn is_preferred_language(language: &Language, preferred: &[Language]) -> bool {
    preferred
        .iter()
        .any(|candidate| language.matches(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{AudioDetails, SubtitleDetails};

    fn chinese() -> Language {
        Language::new("zh-Hans")
    }

    fn audio(index: u32, language: Language, channels: u16, is_default: bool) -> MediaStream {
        let mut stream = MediaStream::audio(
            index,
            "eac3",
            AudioDetails {
                channels: Some(channels),
                sample_rate: Some(48_000),
                bitrate: None,
                profile: None,
            },
            language,
        );
        stream.is_default = is_default;
        stream
    }

    fn subtitle(index: u32, language: Language, forced: bool) -> MediaStream {
        let mut stream = MediaStream::subtitle(index, "subrip", language);
        stream.is_forced = forced;
        stream
    }

    fn hearing_impaired(mut stream: MediaStream) -> MediaStream {
        if let Some(details) = stream.subtitle.as_mut() {
            details.hearing_impaired = true;
        } else {
            stream.subtitle = Some(SubtitleDetails {
                format: Some("subrip".into()),
                hearing_impaired: true,
            });
        }
        stream
    }

    #[test]
    fn audio_prefers_configured_language() {
        let streams = vec![
            audio(1, Language::english(), 6, true),
            audio(2, Language::new("zh"), 2, false),
        ];
        let prefs = UserPreferences::default();
        let picked = select_audio(&streams, &prefs).unwrap();
        assert_eq!(picked.index, 2);
    }

    #[test]
    fn audio_prefers_surround_within_same_language() {
        let streams = vec![
            audio(1, Language::new("zh"), 2, true),
            audio(2, Language::new("zh-Hant"), 6, false),
        ];
        let prefs = UserPreferences::default();
        assert_eq!(select_audio(&streams, &prefs).unwrap().index, 2);

        let stereo_only = UserPreferences {
            prefer_surround_audio: false,
            ..UserPreferences::default()
        };
        // 关掉环绕优先后，同语言内默认轨胜出。
        assert_eq!(select_audio(&streams, &stereo_only).unwrap().index, 1);
    }

    #[test]
    fn audio_falls_back_to_default_then_first() {
        let no_match = vec![
            audio(1, Language::new("ja"), 2, false),
            audio(2, Language::new("ko"), 6, true),
        ];
        let prefs = UserPreferences::default();
        assert_eq!(select_audio(&no_match, &prefs).unwrap().index, 2);

        let none_default = vec![
            audio(1, Language::new("ja"), 6, false),
            audio(2, Language::new("ko"), 2, false),
        ];
        assert_eq!(select_audio(&none_default, &prefs).unwrap().index, 1);

        assert!(select_audio(&[], &prefs).is_none());
        // 只有字幕时不应误选。
        let subs_only = vec![subtitle(3, chinese(), false)];
        assert!(select_audio(&subs_only, &prefs).is_none());
    }

    #[test]
    fn audio_deprioritises_external_tracks() {
        let mut external = audio(2, Language::new("zh"), 6, false);
        external.is_external = true;
        let streams = vec![audio(1, Language::new("zh"), 2, false), external];
        let prefs = UserPreferences::default();
        assert_eq!(select_audio(&streams, &prefs).unwrap().index, 1);
    }

    #[test]
    fn subtitle_always_picks_preferred_language() {
        let streams = vec![
            subtitle(3, Language::english(), false),
            subtitle(4, chinese(), false),
        ];
        let prefs = UserPreferences::default();
        assert_eq!(
            select_subtitle(&streams, &prefs, None),
            TrackChoice::Stream(4)
        );
    }

    #[test]
    fn subtitle_never_matches_wrong_language() {
        let streams = vec![subtitle(3, Language::new("ja"), false)];
        let prefs = UserPreferences::default();
        // 日语字幕不是用户要的语言，宁可不显示。
        assert_eq!(select_subtitle(&streams, &prefs, None), TrackChoice::Off);
    }

    #[test]
    fn subtitle_prefers_non_forced_over_forced() {
        let streams = vec![subtitle(3, chinese(), true), subtitle(4, chinese(), false)];
        let prefs = UserPreferences::default();
        assert_eq!(
            select_subtitle(&streams, &prefs, None),
            TrackChoice::Stream(4)
        );
    }

    #[test]
    fn forced_mode_only_takes_forced() {
        let streams = vec![
            subtitle(3, chinese(), false),
            subtitle(4, Language::english(), true),
        ];
        let prefs = UserPreferences {
            subtitle_mode: SubtitleMode::Forced,
            ..UserPreferences::default()
        };
        assert_eq!(
            select_subtitle(&streams, &prefs, None),
            TrackChoice::Stream(4)
        );

        let no_forced = vec![subtitle(3, chinese(), false)];
        assert_eq!(select_subtitle(&no_forced, &prefs, None), TrackChoice::Off);
    }

    #[test]
    fn always_mode_falls_back_to_forced_when_only_forced_exists() {
        let streams = vec![subtitle(3, chinese(), true)];
        let prefs = UserPreferences::default();
        assert_eq!(
            select_subtitle(&streams, &prefs, None),
            TrackChoice::Stream(3)
        );
    }

    #[test]
    fn foreign_audio_only_respects_selected_audio() {
        let streams = vec![subtitle(3, chinese(), false)];
        let prefs = UserPreferences {
            subtitle_mode: SubtitleMode::ForeignAudioOnly,
            ..UserPreferences::default()
        };

        let chinese_audio = audio(1, Language::new("zh"), 2, true);
        assert_eq!(
            select_subtitle(&streams, &prefs, Some(&chinese_audio)),
            TrackChoice::Off
        );

        let japanese_audio = audio(1, Language::new("ja"), 2, true);
        assert_eq!(
            select_subtitle(&streams, &prefs, Some(&japanese_audio)),
            TrackChoice::Stream(3)
        );

        // 音轨未知时不假设它是偏好语言。
        assert_eq!(
            select_subtitle(&streams, &prefs, None),
            TrackChoice::Stream(3)
        );
    }

    #[test]
    fn never_mode_returns_off() {
        let streams = vec![subtitle(3, chinese(), false)];
        let prefs = UserPreferences {
            subtitle_mode: SubtitleMode::Never,
            ..UserPreferences::default()
        };
        assert_eq!(select_subtitle(&streams, &prefs, None), TrackChoice::Off);
    }

    #[test]
    fn hearing_impaired_is_filtered_by_default() {
        let streams = vec![
            hearing_impaired(subtitle(3, chinese(), false)),
            subtitle(4, Language::english(), false),
        ];
        let prefs = UserPreferences::default();
        assert_eq!(
            select_subtitle(&streams, &prefs, None),
            TrackChoice::Stream(4)
        );

        let allow = UserPreferences {
            allow_hearing_impaired: true,
            ..UserPreferences::default()
        };
        // 中文优先，即便它是听障字幕。
        assert_eq!(
            select_subtitle(&streams, &allow, None),
            TrackChoice::Stream(3)
        );
    }

    #[test]
    fn undetermined_language_only_used_when_default() {
        let undetermined = vec![subtitle(3, Language::undetermined(), false)];
        let prefs = UserPreferences::default();
        assert_eq!(
            select_subtitle(&undetermined, &prefs, None),
            TrackChoice::Off
        );

        let mut default_undetermined = subtitle(3, Language::undetermined(), false);
        default_undetermined.is_default = true;
        assert_eq!(
            select_subtitle(&[default_undetermined], &prefs, None),
            TrackChoice::Stream(3)
        );
    }

    #[test]
    fn no_subtitles_returns_off() {
        let prefs = UserPreferences::default();
        assert_eq!(select_subtitle(&[], &prefs, None), TrackChoice::Off);
        let audio_only = vec![audio(1, Language::new("zh"), 2, true)];
        assert_eq!(select_subtitle(&audio_only, &prefs, None), TrackChoice::Off);
    }

    #[test]
    fn track_choice_helpers() {
        assert!(TrackChoice::Off.is_off());
        assert_eq!(TrackChoice::Off.index(), None);
        assert_eq!(TrackChoice::Stream(7).index(), Some(7));
        assert!(!TrackChoice::Stream(7).is_off());
    }
}
