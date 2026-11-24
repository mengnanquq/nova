// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 引擎状态 → [`PlayerView`] 字段的搬运（纯函数，可单测）。
//!
//! 轨道列表**必须来自引擎枚举**（`EngineEvent::TracksChanged`），不是服务端元信息猜的；
//! 服务端说有三条音轨、文件里只有两条的情况是真实存在的。

use nova_core::{MediaStream, StreamKind};
use nova_playback::{EngineDiagnostics, SubtitleSelection, TrackSet};
use nova_ui::TrackItem;

/// 引擎枚举出的音轨列表。
pub(crate) fn audio_tracks(tracks: &TrackSet, selected: Option<u32>) -> Vec<TrackItem> {
    tracks
        .audio()
        .map(|stream| {
            TrackItem::new(stream.index, audio_label(stream))
                .with_detail(audio_detail(stream))
                .with_selected(selected == Some(stream.index))
        })
        .collect()
}

/// 引擎枚举出的字幕列表（"关闭"由界面上单独一项表示，不在这里）。
pub(crate) fn subtitle_tracks(tracks: &TrackSet, selected: &SubtitleSelection) -> Vec<TrackItem> {
    let selected = selected.track_index();
    tracks
        .subtitles()
        .map(|stream| {
            let mut item = TrackItem::new(stream.index, subtitle_label(stream))
                .with_detail(subtitle_detail(stream))
                .with_selected(selected == Some(stream.index));
            if is_bitmap_subtitle(stream) {
                item = item.with_bitmap_subtitle();
            }
            item
        })
        .collect()
}

/// 诊断行：`直连 · hwdec=vaapi · 1920x1080`。
pub(crate) fn engine_line(
    diagnostics: &EngineDiagnostics,
    video_size: Option<(u32, u32)>,
) -> String {
    let hwdec = diagnostics.active_hwdec.as_deref().unwrap_or("未知");
    match video_size {
        Some((width, height)) => format!("直连 · hwdec={hwdec} · {width}x{height}"),
        None => format!("直连 · hwdec={hwdec}"),
    }
}

fn audio_label(stream: &MediaStream) -> String {
    let language = language_label(stream);
    let codec = stream.codec.to_uppercase();
    match &stream.title {
        Some(title) if !title.trim().is_empty() => format!("{title} ({codec})"),
        _ => format!("{language} ({codec})"),
    }
}

fn audio_detail(stream: &MediaStream) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(audio) = &stream.audio {
        if let Some(channels) = audio.channels {
            parts.push(format!("{channels} 声道"));
        }
        if let Some(rate) = audio.sample_rate {
            parts.push(format!("{:.1} kHz", f64::from(rate) / 1000.0));
        }
        if let Some(profile) = &audio.profile
            && !profile.trim().is_empty()
        {
            parts.push(profile.clone());
        }
    }
    if stream.is_default {
        parts.push("默认".to_owned());
    }
    parts.join(" · ")
}

fn subtitle_label(stream: &MediaStream) -> String {
    let language = language_label(stream);
    let format = stream
        .subtitle
        .as_ref()
        .and_then(|subtitle| subtitle.format.clone())
        .unwrap_or_else(|| stream.codec.clone())
        .to_uppercase();
    match &stream.title {
        Some(title) if !title.trim().is_empty() => format!("{title} ({format})"),
        _ => format!("{language} ({format})"),
    }
}

fn subtitle_detail(stream: &MediaStream) -> String {
    let mut parts: Vec<String> = Vec::new();
    if stream.is_forced {
        parts.push("强制".to_owned());
    }
    if is_bitmap_subtitle(stream) {
        parts.push("引擎渲染，样式不可改".to_owned());
    }
    if stream
        .subtitle
        .as_ref()
        .is_some_and(|subtitle| subtitle.hearing_impaired)
    {
        parts.push("听障字幕".to_owned());
    }
    parts.join(" · ")
}

fn language_label(stream: &MediaStream) -> String {
    if stream.language.is_undetermined() {
        "未标注语言".to_owned()
    } else {
        stream.language.as_str().to_owned()
    }
}

/// 图形字幕：样式由引擎烧进画面，界面不能承诺可改字体。
fn is_bitmap_subtitle(stream: &MediaStream) -> bool {
    if stream.kind != StreamKind::Subtitle {
        return false;
    }
    let codec = stream.codec.to_ascii_lowercase();
    matches!(
        codec.as_str(),
        "hdmv_pgs_subtitle" | "pgs" | "dvd_subtitle" | "dvdsub" | "dvb_subtitle" | "xsub"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::{AudioDetails, Language, SubtitleDetails};

    fn audio(index: u32, language: &str, channels: u16) -> MediaStream {
        let mut stream = MediaStream::audio(
            index,
            "eac3",
            AudioDetails {
                channels: Some(channels),
                sample_rate: Some(48_000),
                bitrate: None,
                profile: None,
            },
            Language::new(language),
        );
        stream.title = Some("国语".to_owned());
        stream
    }

    fn subtitle(index: u32, codec: &str) -> MediaStream {
        let mut stream = MediaStream::subtitle(index, codec, Language::new("zh-Hans"));
        stream.subtitle = Some(SubtitleDetails {
            format: Some(codec.to_owned()),
            hearing_impaired: false,
        });
        stream
    }

    #[test]
    fn audio_tracks_mark_the_selected_one() {
        let set = TrackSet::new(vec![audio(1, "eng", 6), audio(2, "zh", 2)]);
        let items = audio_tracks(&set, Some(2));
        assert_eq!(items.len(), 2);
        assert!(!items[0].selected);
        assert!(items[1].selected);
        assert!(items[1].label.contains("国语"), "{}", items[1].label);
        assert!(items[1].detail.as_deref().unwrap_or("").contains("2 声道"));
    }

    #[test]
    fn bitmap_subtitles_are_flagged() {
        let set = TrackSet::new(vec![
            subtitle(3, "subrip"),
            subtitle(4, "hdmv_pgs_subtitle"),
        ]);
        let items = subtitle_tracks(&set, &SubtitleSelection::Track(4));
        assert!(!items[0].is_bitmap_subtitle);
        assert!(items[1].is_bitmap_subtitle);
        assert!(items[1].selected);
        assert!(!items[0].selected);
        assert!(
            items[1]
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("引擎渲染"),
            "{:?}",
            items[1].detail
        );
    }

    #[test]
    fn subtitle_off_selects_nothing() {
        let set = TrackSet::new(vec![subtitle(3, "subrip")]);
        let items = subtitle_tracks(&set, &SubtitleSelection::Off);
        assert!(items.iter().all(|item| !item.selected));
    }

    #[test]
    fn engine_line_reports_hwdec_and_size() {
        let diagnostics = EngineDiagnostics {
            requested_hwdec: Some("vaapi".into()),
            active_hwdec: Some("vaapi".into()),
            hw_decode_active: true,
            ..EngineDiagnostics::default()
        };
        assert_eq!(
            engine_line(&diagnostics, Some((1920, 1080))),
            "直连 · hwdec=vaapi · 1920x1080"
        );
        assert_eq!(engine_line(&diagnostics, None), "直连 · hwdec=vaapi");
        assert_eq!(
            engine_line(&EngineDiagnostics::default(), None),
            "直连 · hwdec=未知"
        );
    }
}
