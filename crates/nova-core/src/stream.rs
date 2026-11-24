// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体资源与流：一个可播放的文件（版本）及其内部的视频 / 音频 / 字幕流。

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::ids::{ItemId, SourceId};
use crate::language::Language;

/// 流类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    /// 视频流。
    Video,
    /// 音频流。
    Audio,
    /// 字幕流（内嵌或外挂）。
    Subtitle,
}

/// HDR 格式。未知格式保留原始字符串，便于诊断时看到服务端到底报了什么。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrFormat {
    /// HDR10（静态元数据）。
    Hdr10,
    /// HDR10+（动态元数据）。
    Hdr10Plus,
    /// 杜比视界。
    DolbyVision,
    /// HLG。
    Hlg,
    /// 其他 / 未识别格式。
    Other(String),
}

/// 视频流细节。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoDetails {
    /// 像素宽度。
    pub width: u32,
    /// 像素高度。
    pub height: u32,
    /// 帧率。
    pub frame_rate: Option<f32>,
    /// 位深（8 / 10 / 12）。
    pub bit_depth: Option<u8>,
    /// HDR 格式。
    pub hdr: Option<HdrFormat>,
    /// 是否隔行扫描。
    pub interlaced: bool,
}

impl VideoDetails {
    /// 是否 4K 及以上。
    pub fn is_uhd(&self) -> bool {
        self.width >= 3840 || self.height >= 2160
    }

    /// 像素数，用于在同一条目的多个版本里挑"最合适"的一个。
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// 音频流细节。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioDetails {
    /// 声道数（2 / 6 / 8）。
    pub channels: Option<u16>,
    /// 采样率（Hz）。
    pub sample_rate: Option<u32>,
    /// 码率（bps）。
    pub bitrate: Option<u64>,
    /// 编码 profile（如 `Atmos` / `TrueHD`）。
    pub profile: Option<String>,
}

impl AudioDetails {
    /// 是否为环绕声（多于双声道）。
    pub fn is_surround(&self) -> bool {
        self.channels.is_some_and(|channels| channels > 2)
    }
}

/// 字幕流细节。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubtitleDetails {
    /// 字幕格式（`srt` / `ass` / `pgs` …）。
    pub format: Option<String>,
    /// 是否为听障字幕（含音效描述）。
    pub hearing_impaired: bool,
}

/// 一条媒体流。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaStream {
    /// 在服务器 / 文件内的流序号，播放时用它选择轨道。
    pub index: u32,
    /// 流类型。
    pub kind: StreamKind,
    /// 编码名（`hevc` / `aac` / `subrip`）。
    pub codec: String,
    /// 语言。
    pub language: Language,
    /// 显示名（"国语 5.1"、"简体中文"）。
    pub title: Option<String>,
    /// 服务端标记的默认轨。
    pub is_default: bool,
    /// 强制字幕轨（只翻译外语对白）。
    pub is_forced: bool,
    /// 外挂字幕 / 外部音轨。
    pub is_external: bool,
    /// 外挂轨的直链地址；内嵌轨为 `None`。
    pub url: Option<String>,
    /// 视频细节（`kind == Video` 时有值）。
    pub video: Option<VideoDetails>,
    /// 音频细节（`kind == Audio` 时有值）。
    pub audio: Option<AudioDetails>,
    /// 字幕细节（`kind == Subtitle` 时有值）。
    pub subtitle: Option<SubtitleDetails>,
}

impl MediaStream {
    /// 构造视频流。
    pub fn video(
        index: u32,
        codec: impl Into<String>,
        details: VideoDetails,
        language: Language,
    ) -> Self {
        Self {
            index,
            kind: StreamKind::Video,
            codec: codec.into(),
            language,
            title: None,
            is_default: index == 0,
            is_forced: false,
            is_external: false,
            url: None,
            video: Some(details),
            audio: None,
            subtitle: None,
        }
    }

    /// 构造音频流。
    pub fn audio(
        index: u32,
        codec: impl Into<String>,
        details: AudioDetails,
        language: Language,
    ) -> Self {
        Self {
            index,
            kind: StreamKind::Audio,
            codec: codec.into(),
            language,
            title: None,
            is_default: false,
            is_forced: false,
            is_external: false,
            url: None,
            video: None,
            audio: Some(details),
            subtitle: None,
        }
    }

    /// 构造字幕流。
    pub fn subtitle(index: u32, codec: impl Into<String>, language: Language) -> Self {
        Self {
            index,
            kind: StreamKind::Subtitle,
            codec: codec.into(),
            language,
            title: None,
            is_default: false,
            is_forced: false,
            is_external: false,
            url: None,
            video: None,
            audio: None,
            subtitle: Some(SubtitleDetails {
                format: None,
                hearing_impaired: false,
            }),
        }
    }

    /// 是否带图形字幕（PGS / VobSub），这类字幕不能简单转成文本，需要渲染到画面。
    pub fn is_bitmap_subtitle(&self) -> bool {
        if self.kind != StreamKind::Subtitle {
            return false;
        }
        let hint = self
            .subtitle
            .as_ref()
            .and_then(|details| details.format.as_deref())
            .unwrap_or(&self.codec);
        let hint = hint.to_ascii_lowercase();
        hint.contains("pgs")
            || hint.contains("sup")
            || hint.contains("vobsub")
            || hint.contains("dvdsub")
            || hint.contains("dvb")
    }
}

/// 一个可播放的资源（同一部电影的 1080p / 4K 版本各算一个资源）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaSource {
    /// 资源标识。
    pub id: SourceId,
    /// 所属条目。
    pub item_id: ItemId,
    /// 版本名（"BluRay 2160p"）。
    pub name: Option<String>,
    /// 容器格式（`mkv` / `mp4`）。
    pub container: Option<String>,
    /// 文件大小（字节）。
    pub size: Option<u64>,
    /// 总码率（bps）。
    pub bitrate: Option<u64>,
    /// 该资源自身的时长。
    ///
    /// 列表接口通常不返回时长，只有 `PlaybackInfo` / 详情接口才有 `MediaSources[].RunTimeTicks`；
    /// 所以时长必须能挂在资源上，不能只依赖 `MediaItem::runtime`。
    #[serde(default)]
    pub runtime: Option<Duration>,
    /// 全部流。
    pub streams: Vec<MediaStream>,
}

impl MediaSource {
    /// 第一条视频流。
    pub fn video_stream(&self) -> Option<&MediaStream> {
        self.streams
            .iter()
            .find(|stream| stream.kind == StreamKind::Video)
    }

    /// 全部音频流。
    pub fn audio_streams(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Audio)
    }

    /// 全部字幕流。
    pub fn subtitle_streams(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Subtitle)
    }

    /// 是否存在图形字幕——影响播放引擎选型（文本字幕可由引擎渲染，图形字幕必须走视频叠加）。
    pub fn has_bitmap_subtitles(&self) -> bool {
        self.subtitle_streams()
            .any(|stream| stream.is_bitmap_subtitle())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(index: u32, width: u32, height: u32) -> MediaStream {
        MediaStream::video(
            index,
            "hevc",
            VideoDetails {
                width,
                height,
                frame_rate: Some(23.976),
                bit_depth: Some(10),
                hdr: Some(HdrFormat::Hdr10),
                interlaced: false,
            },
            Language::undetermined(),
        )
    }

    #[test]
    fn video_helpers() {
        let uhd = video(0, 3840, 2160);
        let details = uhd.video.as_ref().unwrap();
        assert!(details.is_uhd());
        assert_eq!(details.pixels(), 3840 * 2160);

        let hd = video(0, 1920, 1080);
        assert!(!hd.video.as_ref().unwrap().is_uhd());
    }

    #[test]
    fn surround_detection() {
        let stereo = MediaStream::audio(
            1,
            "aac",
            AudioDetails {
                channels: Some(2),
                sample_rate: None,
                bitrate: None,
                profile: None,
            },
            Language::english(),
        );
        assert!(!stereo.audio.as_ref().unwrap().is_surround());

        let mut surround = stereo.clone();
        surround.audio = Some(AudioDetails {
            channels: Some(6),
            sample_rate: None,
            bitrate: None,
            profile: None,
        });
        assert!(surround.audio.as_ref().unwrap().is_surround());

        // 声道数未知时不臆测环绕声。
        let unknown = MediaStream::audio(
            2,
            "dts",
            AudioDetails {
                channels: None,
                sample_rate: None,
                bitrate: None,
                profile: None,
            },
            Language::english(),
        );
        assert!(!unknown.audio.as_ref().unwrap().is_surround());
    }

    #[test]
    fn bitmap_subtitle_detection() {
        let pgs = MediaStream::subtitle(3, "pgs", Language::new("zh"));
        assert!(pgs.is_bitmap_subtitle());

        let mut sup = MediaStream::subtitle(4, "subrip", Language::new("en"));
        sup.subtitle = Some(SubtitleDetails {
            format: Some("PGS".into()),
            hearing_impaired: false,
        });
        assert!(sup.is_bitmap_subtitle());

        let srt = MediaStream::subtitle(5, "subrip", Language::new("en"));
        assert!(!srt.is_bitmap_subtitle());

        // 音频流即使编码名里带 pgs 也不是字幕。
        let mut audio = MediaStream::audio(
            6,
            "aac",
            AudioDetails {
                channels: Some(2),
                sample_rate: None,
                bitrate: None,
                profile: None,
            },
            Language::english(),
        );
        audio.codec = "pgs".into();
        assert!(!audio.is_bitmap_subtitle());
    }

    #[test]
    fn source_stream_filters() {
        let source = MediaSource {
            id: SourceId::new("s1"),
            item_id: ItemId::new("i1"),
            name: None,
            container: Some("mkv".into()),
            size: None,
            bitrate: None,
            runtime: None,
            streams: vec![
                video(0, 1920, 1080),
                MediaStream::audio(
                    1,
                    "eac3",
                    AudioDetails {
                        channels: Some(6),
                        sample_rate: Some(48_000),
                        bitrate: None,
                        profile: None,
                    },
                    Language::english(),
                ),
                MediaStream::subtitle(2, "pgs", Language::new("zh")),
                MediaStream::subtitle(3, "subrip", Language::new("en")),
            ],
        };

        assert_eq!(source.video_stream().unwrap().index, 0);
        assert_eq!(source.audio_streams().count(), 1);
        assert_eq!(source.subtitle_streams().count(), 2);
        assert!(source.has_bitmap_subtitles());

        // 没有任何图形字幕时应返回 false。
        let mut text_only = source.clone();
        text_only.streams.retain(|stream| stream.codec != "pgs");
        assert!(!text_only.has_bitmap_subtitles());
    }
}
