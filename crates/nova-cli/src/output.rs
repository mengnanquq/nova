// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 命令输出：一份给人看的文本 + 一份给 `--json` 用的结构。
//!
//! 两条输出都从同一批数据生成，测试因此可以直接断言结构化结果，
//! 不必去解析自己刚打印的表格。

use nova_core::{ItemId, MediaItem, MediaKind, MediaSource, MediaStream, PlayMethod, StreamKind};
use nova_provider::Library;
use serde_json::{Value, json};

/// 一条命令的结果。
#[derive(Clone, Debug)]
pub struct CommandOutput {
    text: String,
    json: Value,
}

impl CommandOutput {
    /// 由文本与 JSON 构造。
    pub fn new(text: String, json: Value) -> Self {
        Self { text, json }
    }

    /// 按 `--json` 决定打印哪一种。
    pub fn render(&self, json: bool) -> String {
        if json {
            // `to_string_pretty` 只对 Value 失败于"含非字符串键"，这里不可能发生。
            let pretty =
                serde_json::to_string_pretty(&self.json).unwrap_or_else(|_| self.json.to_string());
            format!("{pretty}\n")
        } else {
            self.text.clone()
        }
    }

    /// 结构化结果（测试用）。
    pub fn json(&self) -> &Value {
        &self.json
    }

    /// 文本结果（测试用）。
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// 简单表格：列之间用两个空格分隔，按字符数补空格。
///
/// 刻意不处理东亚字符宽度：调用方给的中文列都放在最后一列，
/// 不影响可读性（引入 unicode-width 只为了对齐不值得）。
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let columns = headers.len();
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let mut push_row = |cells: Vec<String>| {
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                out.push_str("  ");
            }
            out.push_str(cell);
            if index + 1 < columns {
                let pad = widths[index].saturating_sub(cell.chars().count());
                out.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push('\n');
    };

    push_row(headers.iter().map(|header| (*header).to_owned()).collect());
    for row in rows {
        push_row(row.clone());
    }
    out
}

/// 秒数转 `h:mm:ss` / `m:ss`；`None` 显示 `-`。
pub fn duration_text(seconds: Option<u64>) -> String {
    let Some(total) = seconds else {
        return "-".to_owned();
    };
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// `Option<T>` 的显示：`None` 用 `-`。
pub fn or_dash(value: Option<impl std::fmt::Display>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "-".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// 命令输出的公共渲染
//
// 这些函数只做"把统一模型变成表格 / JSON"，不含任何 I/O 或网络逻辑，
// 因此单独放在这里：命令实现（`commands.rs`）保持只关心流程。
// ---------------------------------------------------------------------------

pub(crate) fn kind_text(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
        MediaKind::Season => "season",
        MediaKind::Episode => "episode",
    }
}

pub(crate) fn method_text(method: PlayMethod) -> &'static str {
    match method {
        PlayMethod::DirectPlay => "direct-play",
        PlayMethod::DirectStream => "direct-stream",
        PlayMethod::Transcode => "transcode",
    }
}

pub(crate) fn method_json(method: PlayMethod) -> &'static str {
    match method {
        PlayMethod::DirectPlay => "direct_play",
        PlayMethod::DirectStream => "direct_stream",
        PlayMethod::Transcode => "transcode",
    }
}

pub(crate) fn yes_no(value: bool) -> &'static str {
    if value { "有" } else { "无" }
}

pub(crate) fn or_empty_tag(tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("({tag})"),
        None => String::new(),
    }
}

pub(crate) fn item_row(item: &MediaItem) -> Vec<String> {
    vec![
        item.id.as_str().to_owned(),
        kind_text(item.kind).to_owned(),
        duration_text(item.runtime.map(|runtime| runtime.as_secs())),
        item.title.clone(),
    ]
}

/// 一条媒体流的单行描述。
pub(crate) fn stream_line(stream: &MediaStream) -> String {
    let mut parts = vec![
        format!(
            "{} #{}",
            match stream.kind {
                StreamKind::Video => "视频",
                StreamKind::Audio => "音频",
                StreamKind::Subtitle => "字幕",
            },
            stream.index
        ),
        or_dash(Some(stream.codec.as_str())),
    ];
    if let Some(video) = &stream.video {
        parts.push(format!("{}x{}", video.width, video.height));
        if let Some(depth) = video.bit_depth {
            parts.push(format!("{depth}bit"));
        }
        if let Some(hdr) = &video.hdr {
            parts.push(format!("{hdr:?}"));
        }
    }
    if let Some(audio) = &stream.audio {
        if let Some(channels) = audio.channels {
            parts.push(format!("{channels}ch"));
        }
        if let Some(profile) = &audio.profile {
            parts.push(profile.clone());
        }
    }
    if let Some(subtitle) = &stream.subtitle {
        if let Some(format) = &subtitle.format {
            parts.push(format.clone());
        }
        if subtitle.hearing_impaired {
            parts.push("听障".to_owned());
        }
    }
    if !stream.language.is_undetermined() {
        parts.push(stream.language.to_string());
    }
    if let Some(title) = &stream.title {
        parts.push(title.clone());
    }
    parts.join(" ")
}

pub(crate) fn library_json(library: &Library) -> Value {
    json!({
        "id": library.id.as_str(),
        "name": library.name,
        "kind": kind_text(library.kind),
        "collection_type": library.collection_type,
        "item_count": library.item_count,
    })
}

pub(crate) fn item_json(item: &MediaItem) -> Value {
    json!({
        "id": item.id.as_str(),
        "title": item.title,
        "sort_title": item.sort_title,
        "kind": kind_text(item.kind),
        "year": item.year,
        "runtime_secs": item.runtime.map(|runtime| runtime.as_secs()),
        "child_count": item.child_count,
        "parent_id": item.parent_id.as_ref().map(ItemId::as_str),
        "played": item.user_data.played,
        "resume_secs": item.user_data.resume_position.as_secs(),
        "play_count": item.user_data.play_count,
        "genres": item.genres,
        "has_poster": item.images.poster.is_some(),
        "poster_tag": item.images.poster_tag,
        "has_backdrop": item.images.backdrop.is_some(),
        "backdrop_tag": item.images.backdrop_tag,
    })
}

pub(crate) fn source_json(source: &MediaSource) -> Value {
    json!({
        "id": source.id.as_str(),
        "name": source.name,
        "container": source.container,
        "size": source.size,
        "bitrate": source.bitrate,
        "runtime_secs": source.runtime.map(|runtime| runtime.as_secs()),
        "has_video": source.video_stream().is_some(),
        "streams": source.streams.iter().map(stream_json).collect::<Vec<_>>(),
    })
}

pub(crate) fn stream_json(stream: &MediaStream) -> Value {
    let mut value = json!({
        "type": match stream.kind {
            StreamKind::Video => "video",
            StreamKind::Audio => "audio",
            StreamKind::Subtitle => "subtitle",
        },
        "index": stream.index,
        "codec": stream.codec,
        "language": stream.language.as_str(),
        "title": stream.title,
        "default": stream.is_default,
        "forced": stream.is_forced,
        "external": stream.is_external,
    });
    if let Some(video) = &stream.video {
        value["width"] = json!(video.width);
        value["height"] = json!(video.height);
        value["bit_depth"] = json!(video.bit_depth);
        value["frame_rate"] = json!(video.frame_rate);
        value["hdr"] = json!(video.hdr.as_ref().map(|hdr| format!("{hdr:?}")));
    }
    if let Some(audio) = &stream.audio {
        value["channels"] = json!(audio.channels);
        value["sample_rate"] = json!(audio.sample_rate);
        value["bitrate"] = json!(audio.bitrate);
        value["profile"] = json!(audio.profile);
    }
    if let Some(subtitle) = &stream.subtitle {
        value["format"] = json!(subtitle.format);
        value["hearing_impaired"] = json!(subtitle.hearing_impaired);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::{
        AudioDetails, ImageSet, ItemId, MediaItem, MediaKind, MediaStream, SourceId, UserData,
        VideoDetails,
    };
    use serde_json::json;
    use std::time::Duration;

    fn sample_item() -> MediaItem {
        MediaItem {
            id: ItemId::new("i1"),
            kind: MediaKind::Movie,
            title: "片段".to_owned(),
            sort_title: "片段".to_owned(),
            original_title: None,
            year: Some(2024),
            overview: None,
            genres: vec!["测试".to_owned()],
            runtime: Some(Duration::from_secs(3)),
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet {
                poster: Some("http://x/Items/i1/Images/Primary".to_owned()),
                poster_tag: Some("ptag".to_owned()),
                ..ImageSet::default()
            },
            user_data: UserData {
                played: false,
                resume_position: Duration::from_secs(1),
                play_count: 2,
                favorite: true,
            },
        }
    }

    fn sample_source() -> MediaSource {
        MediaSource {
            id: SourceId::new("s1"),
            item_id: ItemId::new("i1"),
            name: Some("1080p".to_owned()),
            container: Some("mp4".to_owned()),
            size: Some(1024),
            bitrate: Some(1_000_000),
            runtime: Some(Duration::from_secs(3)),
            streams: vec![
                MediaStream::video(
                    0,
                    "hevc",
                    VideoDetails {
                        width: 1920,
                        height: 1080,
                        frame_rate: Some(30.0),
                        bit_depth: Some(10),
                        hdr: None,
                        interlaced: false,
                    },
                    nova_core::Language::undetermined(),
                ),
                MediaStream::audio(
                    1,
                    "aac",
                    AudioDetails {
                        channels: Some(2),
                        sample_rate: Some(48_000),
                        bitrate: None,
                        profile: None,
                    },
                    nova_core::Language::new("zh"),
                ),
            ],
        }
    }

    #[test]
    fn render_switches_between_text_and_json() {
        let output = CommandOutput::new("hello\n".to_owned(), json!({"a": 1}));
        assert_eq!(output.render(false), "hello\n");
        let rendered = output.render(true);
        assert!(rendered.contains("\"a\": 1"), "{rendered}");
        assert!(rendered.ends_with('\n'));
        assert_eq!(output.text(), "hello\n");
        assert_eq!(output.json()["a"], 1);
    }

    #[test]
    fn table_pads_columns() {
        let text = table(
            &["ID", "名称"],
            &[
                vec!["a".to_owned(), "电影".to_owned()],
                vec!["bbbb".to_owned(), "剧集库".to_owned()],
            ],
        );
        assert_eq!(text, "ID    名称\na     电影\nbbbb  剧集库\n");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(duration_text(Some(5)), "0:05");
        assert_eq!(duration_text(Some(75)), "1:15");
        assert_eq!(duration_text(Some(3675)), "1:01:15");
        assert_eq!(duration_text(None), "-");
        assert_eq!(or_dash(Some(7)), "7");
        assert_eq!(or_dash(None::<u8>), "-");
    }

    #[test]
    fn item_json_exposes_cache_friendly_fields() {
        let value = item_json(&sample_item());
        assert_eq!(value["id"], "i1");
        assert_eq!(value["kind"], "movie");
        assert_eq!(value["runtime_secs"], 3);
        assert_eq!(value["resume_secs"], 1);
        assert_eq!(value["play_count"], 2);
        assert_eq!(value["has_poster"], true);
        assert_eq!(value["poster_tag"], "ptag");
        assert!(value.get("api_key").is_none());

        let row = item_row(&sample_item());
        assert_eq!(row[0], "i1");
        assert_eq!(row[1], "movie");
        assert_eq!(row[2], "0:03");
        assert_eq!(row[3], "片段");
    }

    #[test]
    fn source_and_stream_json_are_self_describing() {
        let source = sample_source();
        let value = source_json(&source);
        assert_eq!(value["runtime_secs"], 3);
        assert_eq!(value["has_video"], true);
        assert_eq!(value["streams"][0]["type"], "video");
        assert_eq!(value["streams"][0]["width"], 1920);
        assert_eq!(value["streams"][1]["type"], "audio");
        assert_eq!(value["streams"][0]["language"], "und");
        assert_eq!(value["streams"][1]["language"], "zh");

        let line = stream_line(&source.streams[0]);
        assert!(line.contains("视频 #0"), "{line}");
        assert!(line.contains("1920x1080"), "{line}");
        let audio = stream_line(&source.streams[1]);
        assert!(audio.contains("音频 #1"), "{audio}");
        assert!(audio.contains("2ch"), "{audio}");
    }

    #[test]
    fn progress_helpers_match_provider_expectations() {
        assert_eq!(method_text(PlayMethod::DirectStream), "direct-stream");
        assert_eq!(method_json(PlayMethod::DirectStream), "direct_stream");
        assert_eq!(method_json(PlayMethod::Transcode), "transcode");
        assert_eq!(kind_text(MediaKind::Episode), "episode");
        assert_eq!(yes_no(true), "有");
        assert_eq!(yes_no(false), "无");
        assert_eq!(or_empty_tag(Some("t")), "(t)");
        assert_eq!(or_empty_tag(None), "");
    }
}
