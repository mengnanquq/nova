// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 第一阶段播放引擎原型的最小库 API。
//!
//! 设计目标：把"帧从哪来、每帧成本多少、Slint 那边怎么接"三件事做成可测的库，
//! 而不是做成播放器。GStreamer 是当前唯一实现；`FrameSource` 预留了给
//! mpv / libVLC 插入的位置。

use std::fmt;

pub mod bench;
pub mod convert;
pub mod dump;
pub mod gst_source;

#[cfg(feature = "slint")]
pub mod slint_bridge;

/// 一帧解码后的图像，RGB 分量在内存里连续排列。
#[derive(Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// 每行字节数，可能 > width*4（对齐填充）。
    pub stride: usize,
    /// RGBA8，长度恒为 `stride * height`。
    pub data: Vec<u8>,
    /// 显示时间戳（纳秒）。
    pub pts_ns: Option<u64>,
}

impl VideoFrame {
    /// RGBA8：每像素 4 字节。
    pub const BYTES_PER_PIXEL: usize = 4;

    /// 整个缓冲区的字节数。
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    /// 取第 `row` 行（不含行尾 padding）的 RGBA 切片。
    ///
    /// 越界返回 `None`，方便调用方自己判错。
    pub fn row(&self, row: u32) -> Option<&[u8]> {
        if row >= self.height {
            return None;
        }
        let start = row as usize * self.stride;
        let end = start + self.width as usize * Self::BYTES_PER_PIXEL;
        self.data.get(start..end)
    }
}

impl fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("data_len", &self.data.len())
            .field("pts_ns", &self.pts_ns)
            .finish()
    }
}

/// 帧来源抽象：GStreamer 现在是唯一实现，mpv/libVLC 将来可插进来。
pub trait FrameSource {
    /// 取下一帧；`Ok(None)` 表示流正常结束（EOS）。
    fn next_frame(&mut self) -> std::result::Result<Option<VideoFrame>, Error>;

    /// 运行时实际选择的解码器、是否硬解、协商后的 caps。
    ///
    /// 必须如实反映运行时状态，不能写死预期值。
    fn description(&self) -> SourceDescription;
}

/// 解码元素属于硬解还是软解。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderKind {
    /// 由 GPU / 专用硬件解码器完成。
    Hardware,
    /// 由 CPU 软件解码器完成。
    Software,
    /// 名字与 klass 都不在白名单里，无法判定。
    Unknown,
}

impl DecoderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DecoderKind::Hardware => "hardware",
            DecoderKind::Software => "software",
            DecoderKind::Unknown => "unknown",
        }
    }
}

impl fmt::Display for DecoderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 运行时实际插入的 GStreamer 元素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementReport {
    /// 元素实例名（`gst_element_get_name()`）。
    pub name: String,
    /// 工厂名（决定是哪个插件、哪种实现）。
    pub factory: String,
    /// `klass` 属性，如 `Codec/Decoder/Video`。
    pub klass: String,
    /// 注册表里的 rank；找不到工厂时为 `None`。
    pub rank: Option<i32>,
    /// 是否被判定为解码器。
    pub is_decoder: bool,
    pub kind: DecoderKind,
}

/// `gst-discoverer` 风格的补充信息（容器 / 码率 / 可 seek）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub container_caps: Vec<String>,
    pub video_bitrate: Option<u32>,
    pub video_max_bitrate: Option<u32>,
    pub audio_bitrate: Option<u32>,
    pub seekable: bool,
    pub live: bool,
}

impl Discovery {
    pub fn render(&self) -> String {
        format!(
            "容器 caps={}，seekable={}，live={}，视频 max-bitrate={}，音频 bitrate={}",
            join_or_none(&self.container_caps),
            if self.seekable { "是" } else { "否" },
            if self.live { "是" } else { "否" },
            option_text(self.video_max_bitrate.map(|value| value.to_string())),
            option_text(self.audio_bitrate.map(|value| value.to_string())),
        )
    }
}

/// 硬解相关插件在本机的安装情况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReport {
    pub name: String,
    /// 插件已安装时的版本号。
    pub version: Option<String>,
    /// 插件暴露的元素数量；插件在但为 0 通常意味着依赖库缺失。
    pub features: usize,
}

impl PluginReport {
    pub fn present(&self) -> bool {
        self.version.is_some()
    }
}

/// 某个解码器的输入/输出 caps。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderReport {
    pub element: ElementReport,
    /// 解码器 sink pad 的 caps（通常是压缩码流 caps）。
    pub in_caps: Option<String>,
    /// 解码器 src pad 的 caps（解码后原始 caps）。
    pub out_caps: Option<String>,
}

impl DecoderReport {
    pub fn hardware(&self) -> Option<bool> {
        match self.element.kind {
            DecoderKind::Hardware => Some(true),
            DecoderKind::Software => Some(false),
            DecoderKind::Unknown => None,
        }
    }
}

/// 解码后的视频流信息（来自 pad 协商结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    /// 如 `NV12`、`I420`。
    pub format: String,
    /// 如 `30/1`。
    pub framerate: String,
    /// 解码器 src pad 的完整 caps。
    pub caps: String,
}

/// 解码后的音频流信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStreamInfo {
    pub channels: Option<i32>,
    pub rate: Option<i32>,
    pub format: Option<String>,
    pub caps: String,
}

/// `FrameSource::description()` 的返回值：运行时事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescription {
    /// 后端名，目前恒为 `gstreamer`。
    pub backend: &'static str,
    pub gst_version: String,
    pub uri: String,
    /// 容器 caps（demuxer sink pad 协商结果）。
    pub container_caps: Vec<String>,
    pub duration_ns: Option<u64>,
    pub video: Option<VideoStreamInfo>,
    pub audio: Option<AudioStreamInfo>,
    pub video_decoder: Option<DecoderReport>,
    pub audio_decoder: Option<DecoderReport>,
    /// 所有被 `deep-element-added` 观察到的元素（含非解码器）。
    pub elements: Vec<ElementReport>,
    /// appsink 实际协商到的 caps（帧交给 Rust 时的格式）。
    pub sink_caps: Option<String>,
    /// `Some(true)` = 实测走了硬解；`Some(false)` = 实测走软解。
    pub hardware_decode: Option<bool>,
    /// 硬解相关插件（含"插件在但 0 个元素"的情况）。
    pub hw_plugins: Vec<PluginReport>,
    /// discoverer 的补充信息（拿不到时为 `None`）。
    pub discovery: Option<Discovery>,
    /// 本机注册表里缺失的硬解元素名（跨平台排查用）。
    pub missing_hw_factories: Vec<String>,
    /// 运行时由 `missing-plugin` 消息报告的缺失组件。
    pub runtime_missing_plugins: Vec<String>,
    /// 注册表里 `klass` 含 `Hardware` 的解码元素（本机已装的硬解能力）。
    pub installed_hw_decoders: Vec<ElementReport>,
    /// `GST_PLUGIN_FEATURE_RANK` 环境变量（原样）。
    pub rank_env: Option<String>,
}

impl SourceDescription {
    /// 所有被判定为解码器的元素。
    pub fn decoders(&self) -> impl Iterator<Item = &ElementReport> {
        self.elements.iter().filter(|e| e.is_decoder)
    }
}

impl SourceDescription {
    /// 人读的探测报告（`probe` 子命令直接打印这段）。
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "后端            : {} {}",
            self.backend, self.gst_version
        );
        let _ = writeln!(out, "URI             : {}", self.uri);
        let container_caps = if self.container_caps.is_empty() {
            self.discovery
                .as_ref()
                .map(|discovery| discovery.container_caps.clone())
                .unwrap_or_default()
        } else {
            self.container_caps.clone()
        };
        let _ = writeln!(out, "容器 caps       : {}", join_or_none(&container_caps));
        match self.duration_ns {
            Some(ns) => {
                let _ = writeln!(out, "时长            : {:.3} s ({ns} ns)", ns as f64 / 1e9);
            }
            None => {
                let _ = writeln!(out, "时长            : 未知");
            }
        }
        match &self.video {
            Some(video) => {
                let _ = writeln!(
                    out,
                    "视频流          : {}x{} {} @ {}",
                    video.width, video.height, video.format, video.framerate
                );
            }
            None => {
                let _ = writeln!(out, "视频流          : 未知（没有视频解码器？）");
            }
        }
        match &self.audio {
            Some(audio) => {
                let _ = writeln!(
                    out,
                    "音频流          : {} ch, {} Hz, {}",
                    option_text(audio.channels.map(|c| c.to_string())),
                    option_text(audio.rate.map(|r| r.to_string())),
                    option_text(audio.format.clone())
                );
            }
            None => {
                let _ = writeln!(out, "音频流          : 无");
            }
        }
        let _ = writeln!(
            out,
            "视频解码器      : {}",
            render_decoder(self.video_decoder.as_ref())
        );
        let _ = writeln!(
            out,
            "音频解码器      : {}",
            render_decoder(self.audio_decoder.as_ref())
        );
        if let Some(video_decoder) = &self.video_decoder {
            let _ = writeln!(
                out,
                "  解码器输入 caps: {}",
                option_text(video_decoder.in_caps.clone())
            );
            let _ = writeln!(
                out,
                "  解码器输出 caps: {}",
                option_text(video_decoder.out_caps.clone())
            );
        }
        let _ = writeln!(
            out,
            "appsink caps    : {}",
            option_text(self.sink_caps.clone())
        );
        let _ = writeln!(
            out,
            "硬解判定        : {}",
            match self.hardware_decode {
                Some(true) => "是（实测走硬解）",
                Some(false) => "否（实测走软解）",
                None => "无法判定（解码器不在白名单里）",
            }
        );
        let _ = writeln!(out, "元素链          :");
        for (index, element) in self.elements.iter().enumerate() {
            let _ = writeln!(
                out,
                "  [{index:02}] {:<20} factory={:<18} kind={:<8} klass={} rank={}",
                element.name,
                element.factory,
                element.kind.as_str(),
                element.klass,
                option_text(element.rank.map(|rank| rank.to_string()))
            );
        }
        if self.installed_hw_decoders.is_empty() {
            let _ = writeln!(out, "已装硬解元素    : （无）");
        } else {
            let names: Vec<String> = self
                .installed_hw_decoders
                .iter()
                .map(|element| {
                    format!(
                        "{} (rank {})",
                        element.factory,
                        option_text(element.rank.map(|rank| rank.to_string()))
                    )
                })
                .collect();
            let _ = writeln!(out, "已装硬解元素    : {}", names.join(", "));
        }
        if let Some(discovery) = &self.discovery {
            let _ = writeln!(out, "discoverer      : {}", discovery.render());
        }
        let _ = writeln!(out, "硬解插件        :");
        for plugin in &self.hw_plugins {
            if plugin.present() {
                let _ = writeln!(
                    out,
                    "  {:<18} 已安装 v{}，{} 个元素{}",
                    plugin.name,
                    option_text(plugin.version.clone()),
                    plugin.features,
                    if plugin.features == 0 {
                        "（插件在但元素没注册，通常是依赖库缺失）"
                    } else {
                        ""
                    }
                );
            } else {
                let _ = writeln!(out, "  {:<18} 缺失", plugin.name);
            }
        }
        let _ = writeln!(
            out,
            "缺失硬解元素    : {}",
            join_or_none(&self.missing_hw_factories)
        );
        let _ = writeln!(
            out,
            "运行时缺失组件  : {}",
            join_or_none(&self.runtime_missing_plugins)
        );
        let _ = writeln!(
            out,
            "GST_PLUGIN_FEATURE_RANK: {}",
            self.rank_env
                .clone()
                .unwrap_or_else(|| "（未设置）".to_string())
        );
        out
    }
}

fn render_decoder(report: Option<&DecoderReport>) -> String {
    match report {
        Some(report) => format!(
            "{} [{}] klass={} rank={}",
            report.element.factory,
            report.element.kind.as_str(),
            report.element.klass,
            option_text(report.element.rank.map(|rank| rank.to_string()))
        ),
        None => "（未观测到）".to_string(),
    }
}

fn option_text(value: Option<String>) -> String {
    value.unwrap_or_else(|| "（无）".to_string())
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "（无）".to_string()
    } else {
        items.join(", ")
    }
}

/// 原型统一错误类型。原型不做错误恢复，信息够排查就行。
#[derive(Debug)]
pub enum Error {
    /// GStreamer 侧错误（构造失败、总线错误、超时等）。
    Gst(String),
    /// 文件/目录 IO。
    Io(std::io::Error),
    /// caps/像素格式不支持或数据长度不符。
    Format(String),
    /// PNG 编码。
    Png(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Gst(msg) => write!(f, "gstreamer: {msg}"),
            Error::Io(err) => write!(f, "io: {err}"),
            Error::Format(msg) => write!(f, "format: {msg}"),
            Error::Png(msg) => write!(f, "png: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
