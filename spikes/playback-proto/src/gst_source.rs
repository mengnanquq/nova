// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! GStreamer 后端的 `FrameSource` 实现 + 解码器探测。
//!
//! 管线形状：
//!
//! ```text
//! uridecodebin3 ─┬─ video/x-raw ─> queue ─> videoconvert ─> capsfilter(NV12) ─> appsink
//!                └─ audio/x-raw   （不接，decodebin3 会忽略未选中的流）
//! ```
//!
//! 选择 `uridecodebin3` 而不是 `filesrc ! decodebin3` 的原因：本地路径和 HTTP URL
//! 共用一条代码路径，且它内部就是 `decodebin3`，`deep-element-added` 照样能观测到
//! 实际插入的解码元素。代价是多了 URI 处理层，测量"解码段"时会包含 demuxer/parser。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;

use crate::convert::{self, I420View, Nv12View};
use crate::{
    AudioStreamInfo, DecoderKind, DecoderReport, Discovery, ElementReport, Error, FrameSource,
    PluginReport, Result, SourceDescription, VideoFrame, VideoStreamInfo,
};

/// 交给 Rust 的像素格式。NV12 是软解（avdec_*）与多数硬解转换后最常见的 4:2:0 布局。
pub const APPSINK_CAPS: &str = "video/x-raw,format=NV12";

/// 硬解工厂名前缀白名单（`klass` 里没有 `Hardware` 时的兜底判断）。
const HW_FACTORY_PREFIXES: &[&str] = &[
    "va", "vah", "nvdec", "nvh", "vtdec", "vth", "d3d11", "d3d12", "mfh", "msdk", "qsv", "amf",
    "vulkan",
];

/// 软解工厂名前缀白名单。
const SW_FACTORY_PREFIXES: &[&str] = &[
    "avdec_",
    "vp8dec",
    "vp9dec",
    "vpxdec",
    "openh264dec",
    "theora",
    "mpeg2dec",
    "mpeg4videodec",
    "a52dec",
    "faad",
    "opusdec",
    "vorbisdec",
    "flacdec",
    "mpegaudiodec",
    "dvdec",
    "jpegdec",
    "pngdec",
    "gifdec",
    "speexdec",
];

/// 已知的硬解插件（plugin）名；缺失时打印出来方便跨平台排查。
const HW_PLUGIN_NAMES: &[&str] = &[
    "va",
    "nvcodec",
    "applemedia",
    "d3d11",
    "d3d12",
    "mediafoundation",
    "msdk",
    "qsv",
    "amfcodec",
    "vulkan",
];

/// 已知的硬解元素（factory）名；缺失时打印出来。
const HW_FACTORY_NAMES: &[&str] = &[
    "vah264dec",
    "vah265dec",
    "vah266dec",
    "vajpegdec",
    "vavp9dec",
    "vaav1dec",
    "nvdec",
    "nvh264dec",
    "nvh265dec",
    "nvav1dec",
    "vtdec",
    "vth264dec",
    "vthevcdec",
    "d3d11h264dec",
    "d3d11h265dec",
    "d3d11vp9dec",
    "d3d11av1dec",
    "d3d12h264dec",
    "d3d12h265dec",
    "mfh264dec",
    "mfh265dec",
    "mfvp9dec",
    "msdkh264dec",
    "msdkh265dec",
    "qsvh264dec",
    "qsvh265dec",
    "amfh264dec",
    "amfh265dec",
    "vulkanh264dec",
    "vulkanh265dec",
];

/// 初始化 GStreamer（可重复调用）。
pub fn init() -> Result<()> {
    gst::init().map_err(|err| Error::Gst(format!("gst::init 失败: {err}")))
}

/// 把命令行参数转成 GStreamer 能吃的 URI：本地路径转 `file://`，URL 原样返回。
pub fn to_uri(arg: &str) -> Result<String> {
    if arg.contains("://") {
        return Ok(arg.to_string());
    }
    let path = std::fs::canonicalize(arg)?;
    gst::glib::filename_to_uri(&path, None)
        .map(|uri| uri.to_string())
        .map_err(|err| Error::Gst(format!("路径转 URI 失败: {err}")))
}

/// 按 `klass` 与名字白名单判定硬解/软解。
///
/// 规则（按优先级）：
/// 1. `klass` 含 `Hardware` → 硬解（GStreamer 官方硬解元素的标注方式）；
/// 2. 工厂名命中硬解前缀 → 硬解；
/// 3. 工厂名命中软解前缀 → 软解；
/// 4. 其余解码器 → `Unknown`（不猜）。
pub fn classify_decoder(factory: &str, klass: &str) -> DecoderKind {
    if !klass.contains("Decoder") {
        return DecoderKind::Unknown;
    }
    if klass.contains("Hardware") {
        return DecoderKind::Hardware;
    }
    if HW_FACTORY_PREFIXES
        .iter()
        .any(|prefix| factory.starts_with(prefix))
    {
        return DecoderKind::Hardware;
    }
    if SW_FACTORY_PREFIXES
        .iter()
        .any(|prefix| factory.starts_with(prefix))
    {
        return DecoderKind::Software;
    }
    DecoderKind::Unknown
}

fn rank_of(factory: &gst::ElementFactory) -> Option<i32> {
    use gst::glib::translate::IntoGlib;
    Some(factory.rank().into_glib())
}

fn element_report(element: &gst::Element) -> ElementReport {
    let factory = element.factory();
    let factory_name = factory
        .as_ref()
        .map(|f| f.name().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let klass = factory
        .as_ref()
        .and_then(|f| f.metadata("klass"))
        .map(|s| s.to_string())
        .unwrap_or_default();
    let rank = factory.as_ref().and_then(rank_of);
    let is_decoder = klass.contains("Decoder");
    let kind = classify_decoder(&factory_name, &klass);
    ElementReport {
        name: element.name().to_string(),
        factory: factory_name,
        klass,
        rank,
        is_decoder,
        kind,
    }
}

fn factory_report(factory: &gst::ElementFactory) -> ElementReport {
    let factory_name = factory.name().to_string();
    let klass = factory
        .metadata("klass")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let is_decoder = klass.contains("Decoder");
    ElementReport {
        name: factory_name.clone(),
        factory: factory_name.clone(),
        kind: classify_decoder(&factory_name, &klass),
        klass,
        rank: rank_of(factory),
        is_decoder,
    }
}

/// 注册表里所有 `klass` 含 `Hardware` 的解码元素——即本机装了的硬解能力。
pub fn installed_hw_decoders() -> Vec<ElementReport> {
    let registry = gst::Registry::get();
    let mut out: Vec<ElementReport> = registry
        .features(gst::ElementFactory::static_type())
        .iter()
        .filter_map(|feature| feature.clone().downcast::<gst::ElementFactory>().ok())
        .map(|factory| factory_report(&factory))
        .filter(|report| report.is_decoder && report.kind == DecoderKind::Hardware)
        .collect();
    out.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.factory.cmp(&b.factory)));
    out
}

/// 硬解插件安装情况 + 缺失的硬解元素名（只查白名单，不做全库推断）。
pub fn hw_components() -> (Vec<PluginReport>, Vec<String>) {
    let registry = gst::Registry::get();
    let plugins = HW_PLUGIN_NAMES
        .iter()
        .map(|name| PluginReport {
            name: name.to_string(),
            version: registry
                .find_plugin(name)
                .map(|plugin| plugin.version().to_string()),
            features: registry.features_by_plugin(name).len(),
        })
        .collect();
    let factories = HW_FACTORY_NAMES
        .iter()
        .filter(|name| {
            registry
                .find_feature(name, gst::ElementFactory::static_type())
                .is_none()
        })
        .map(|name| name.to_string())
        .collect();
    (plugins, factories)
}

/// 把第一个有 caps 的 pad 的 caps 格式化出来。
fn pads_caps(pads: &[gst::Pad]) -> Option<gst::Caps> {
    pads.iter()
        .filter_map(|pad| pad.current_caps())
        .find(|caps| !caps.is_any() && !caps.is_empty())
}

fn caps_string(caps: &gst::Caps) -> String {
    caps.to_string()
}

/// 容器 caps：demuxer 的 sink pad 往往不缓存 caps（caps 事件来自上游），
/// 所以先看 sink pad，再看它 peer 的 src pad。
fn container_caps_of(element: &gst::Element) -> Option<String> {
    for pad in element.sink_pads() {
        if let Some(caps) = pad.current_caps().filter(|caps| !caps.is_any()) {
            return Some(caps_string(&caps));
        }
        if let Some(peer) = pad.peer()
            && let Some(caps) = peer.current_caps().filter(|caps| !caps.is_any())
        {
            return Some(caps_string(&caps));
        }
        let queried = pad.query_caps(None);
        if queried.size() == 1 {
            return Some(caps_string(&queried));
        }
    }
    None
}

fn structure_field<T: for<'a> gst::glib::value::FromValue<'a> + 'static>(
    caps: &gst::Caps,
    field: &str,
) -> Option<T> {
    caps.structure(0)?.get::<T>(field).ok()
}

fn video_stream_info(caps: &gst::Caps) -> VideoStreamInfo {
    let structure = caps.structure(0);
    let width = structure_field::<i32>(caps, "width").unwrap_or(0).max(0) as u32;
    let height = structure_field::<i32>(caps, "height").unwrap_or(0).max(0) as u32;
    let format = structure_field::<String>(caps, "format").unwrap_or_default();
    let framerate = structure
        .and_then(|s| s.get::<gst::Fraction>("framerate").ok())
        .map(|fps| format!("{}/{}", fps.0.numer(), fps.0.denom()))
        .unwrap_or_default();
    VideoStreamInfo {
        width,
        height,
        format,
        framerate,
        caps: caps_string(caps),
    }
}

fn audio_stream_info(caps: &gst::Caps) -> AudioStreamInfo {
    AudioStreamInfo {
        channels: structure_field::<i32>(caps, "channels"),
        rate: structure_field::<i32>(caps, "rate"),
        format: structure_field::<String>(caps, "format"),
        caps: caps_string(caps),
    }
}

fn bus_error(err: &gst::message::Error) -> Error {
    let source = err
        .src()
        .map(|element| element.name().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    let debug = err.debug().unwrap_or_default();
    Error::Gst(format!(
        "{}（元素 {source}，debug: {debug}）",
        err.error().message()
    ))
}

/// 构造管线。返回的元素容器用 `Arc<Mutex<_>>` 是因为 `deep-element-added`
/// 由 decodebin3 的内部线程发出，回调必须 `Send + Sync`。
struct PipelineParts {
    pipeline: gst::Pipeline,
    appsink: gst_app::AppSink,
    elements: Arc<Mutex<Vec<gst::Element>>>,
    video_pad_caps: Arc<Mutex<Vec<String>>>,
    runtime_missing: Arc<Mutex<Vec<String>>>,
}

fn make_element(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|err| Error::Gst(format!("创建元素 {factory} 失败: {err}")))
}

fn build_pipeline(uri: &str) -> Result<PipelineParts> {
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin3")
        .name("source")
        .property("uri", uri)
        .build()
        .map_err(|err| Error::Gst(format!("创建 uridecodebin3 失败: {err}")))?;
    let queue = make_element("queue")?;
    let convert = make_element("videoconvert")?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "NV12")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|err| Error::Gst(format!("创建 capsfilter 失败: {err}")))?;
    let appsink = gst_app::AppSink::builder()
        .name("sink")
        .caps(&caps)
        .sync(false)
        .max_buffers(4)
        .enable_last_sample(false)
        .build();

    pipeline
        .add_many([
            &src,
            &queue,
            &convert,
            &capsfilter,
            appsink.upcast_ref::<gst::Element>(),
        ])
        .map_err(|err| Error::Gst(format!("加入管线失败: {err}")))?;
    gst::Element::link_many([
        &queue,
        &convert,
        &capsfilter,
        appsink.upcast_ref::<gst::Element>(),
    ])
    .map_err(|err| Error::Gst(format!("链接转换链失败: {err}")))?;

    let elements = Arc::new(Mutex::new(Vec::new()));
    let elements_for_signal = Arc::clone(&elements);
    pipeline.connect_deep_element_added(move |_, _bin, element| {
        if let Ok(mut list) = elements_for_signal.lock() {
            list.push(element.clone());
        }
    });

    let video_pad_caps = Arc::new(Mutex::new(Vec::new()));
    let video_pad_caps_for_signal = Arc::clone(&video_pad_caps);
    let sink_element = queue.clone();
    src.connect_pad_added(move |_, pad| {
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let is_video = caps
            .structure(0)
            .map(|structure| structure.name().starts_with("video/x-raw"))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        if let Ok(mut list) = video_pad_caps_for_signal.lock() {
            list.push(caps_string(&caps));
        }
        if let Some(sink_pad) = sink_element.static_pad("sink") {
            let _ = pad.link(&sink_pad);
        }
    });

    Ok(PipelineParts {
        pipeline,
        appsink,
        elements,
        video_pad_caps,
        runtime_missing: Arc::new(Mutex::new(Vec::new())),
    })
}

/// 跑到 PAUSED（preroll），等 `ASYNC_DONE`；同时收集总线上的错误与缺失插件。
fn preroll(parts: &PipelineParts) -> Result<()> {
    let pipeline = &parts.pipeline;
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|err| Error::Gst(format!("切到 PAUSED 失败: {err}")))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| Error::Gst("管线没有 bus".into()))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match bus.timed_pop(gst::ClockTime::from_mseconds(200)) {
            Some(message) => match message.view() {
                gst::MessageView::Error(err) => return Err(bus_error(err)),
                gst::MessageView::Eos(..) => {
                    return Err(Error::Gst(
                        "preroll 阶段就收到 EOS（流里没有可解码的数据？）".into(),
                    ));
                }
                gst::MessageView::Element(element) => {
                    if let Some(structure) = element.structure()
                        && structure.name() == "missing-plugin"
                    {
                        let name = structure
                            .get::<String>("name")
                            .unwrap_or_else(|_| "<unknown>".to_string());
                        if let Ok(mut list) = parts.runtime_missing.lock() {
                            list.push(name);
                        }
                    }
                }
                gst::MessageView::AsyncDone(..) => return Ok(()),
                _ => {}
            },
            None => {
                if Instant::now() >= deadline {
                    return Err(Error::Gst("preroll 超时（20s）".into()));
                }
            }
        }
    }
}

/// 用 `GstDiscoverer` 补齐容器 caps / 码率信息；失败时返回 `None`，不影响主流程。
fn discover(uri: &str) -> Option<Discovery> {
    use gstreamer_pbutils::prelude::*;
    let discoverer = gstreamer_pbutils::Discoverer::new(gst::ClockTime::from_seconds(10)).ok()?;
    let info = discoverer.discover_uri(uri).ok()?;

    // 顶层 stream 就是容器（`gst-discoverer` 的 container #0）；
    // `container_streams()` 在部分版本里不含顶层，所以两条路都试。
    let mut container_caps = Vec::new();
    if let Some(top) = info.stream_info()
        && top.is::<gstreamer_pbutils::DiscovererContainerInfo>()
        && let Some(caps) = top.caps()
    {
        container_caps.push(caps_string(&caps));
    }
    if container_caps.is_empty() {
        container_caps = info
            .container_streams()
            .iter()
            .filter_map(|stream| {
                stream
                    .upcast_ref::<gstreamer_pbutils::DiscovererStreamInfo>()
                    .caps()
            })
            .map(|caps| caps_string(&caps))
            .collect();
    }

    let video = info.video_streams();
    let audio = info.audio_streams();
    Some(Discovery {
        container_caps,
        video_bitrate: video.first().map(|stream| stream.bitrate()),
        video_max_bitrate: video.first().map(|stream| stream.max_bitrate()),
        audio_bitrate: audio.first().map(|stream| stream.bitrate()),
        seekable: info.is_seekable(),
        live: info.is_live(),
    })
}

/// 快照当前运行时事实。
fn snapshot_description(parts: &PipelineParts, uri: &str) -> SourceDescription {
    let elements = parts
        .elements
        .lock()
        .map(|list| list.clone())
        .unwrap_or_default();
    let reports: Vec<ElementReport> = elements.iter().map(element_report).collect();

    let video_decoder = elements
        .iter()
        .zip(reports.iter())
        .find(|(_, report)| report.klass.contains("Decoder/Video"))
        .map(|(element, report)| DecoderReport {
            element: report.clone(),
            in_caps: pads_caps(&element.sink_pads()).map(|caps| caps_string(&caps)),
            out_caps: pads_caps(&element.src_pads()).map(|caps| caps_string(&caps)),
        });
    let audio_decoder = elements
        .iter()
        .zip(reports.iter())
        .find(|(_, report)| report.klass.contains("Decoder/Audio"))
        .map(|(element, report)| DecoderReport {
            element: report.clone(),
            in_caps: pads_caps(&element.sink_pads()).map(|caps| caps_string(&caps)),
            out_caps: pads_caps(&element.src_pads()).map(|caps| caps_string(&caps)),
        });

    // 优先用解码器 src pad 的 caps；某些硬解会输出 memory:XXXMemory，
    // 此时退回 uridecodebin3 视频 pad 上记录的 caps。
    let video_caps = video_decoder
        .as_ref()
        .and_then(|report| report.out_caps.as_deref())
        .and_then(|text| text.parse::<gst::Caps>().ok())
        .or_else(|| {
            parts
                .video_pad_caps
                .lock()
                .ok()
                .and_then(|caps| caps.first().and_then(|text| text.parse::<gst::Caps>().ok()))
        });
    let video = video_caps.as_ref().map(video_stream_info);
    let audio_caps = audio_decoder
        .as_ref()
        .and_then(|report| report.out_caps.as_deref())
        .and_then(|text| text.parse::<gst::Caps>().ok());
    let audio = audio_caps.as_ref().map(audio_stream_info);

    let mut container_caps: Vec<String> = elements
        .iter()
        .zip(reports.iter())
        .filter(|(_, report)| report.klass.contains("Demux"))
        .filter_map(|(element, _)| container_caps_of(element))
        .collect();
    if container_caps.is_empty() {
        // demuxer 的 sink pad 经常不缓存 caps（caps 事件只往下游走），
        // 退回到 typefind 的 src pad——那里是 typefind 识别出的容器 caps。
        container_caps = elements
            .iter()
            .zip(reports.iter())
            .filter(|(_, report)| report.factory == "typefind")
            .filter_map(|(element, _)| {
                pads_caps(&element.src_pads()).map(|caps| caps_string(&caps))
            })
            .collect();
    }

    let sink_caps = parts
        .appsink
        .static_pad("sink")
        .and_then(|pad| pad.current_caps())
        .map(|caps| caps_string(&caps));

    let duration_ns = parts
        .pipeline
        .query_duration::<gst::ClockTime>()
        .map(|duration| duration.nseconds());

    let (hw_plugins, missing_factories) = hw_components();
    let runtime_missing = parts
        .runtime_missing
        .lock()
        .map(|list| list.clone())
        .unwrap_or_default();

    SourceDescription {
        backend: "gstreamer",
        gst_version: gst::version_string().to_string(),
        uri: uri.to_string(),
        container_caps,
        duration_ns,
        video,
        audio,
        hardware_decode: video_decoder.as_ref().and_then(DecoderReport::hardware),
        video_decoder,
        audio_decoder,
        elements: reports,
        sink_caps,
        hw_plugins,
        discovery: discover(uri),
        missing_hw_factories: missing_factories,
        runtime_missing_plugins: runtime_missing,
        installed_hw_decoders: installed_hw_decoders(),
        rank_env: std::env::var("GST_PLUGIN_FEATURE_RANK").ok(),
    }
}

/// GStreamer 后端的帧来源。
pub struct GstSource {
    pipeline: gst::Pipeline,
    appsink: gst_app::AppSink,
    description: SourceDescription,
}

impl GstSource {
    /// 打开 URI（本地路径会先转成 `file://`），并跑到 PLAYING 状态。
    pub fn open(media: &str) -> Result<Self> {
        init()?;
        let uri = to_uri(media)?;
        let parts = build_pipeline(&uri)?;
        preroll(&parts)?;
        let description = snapshot_description(&parts, &uri);
        parts
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|err| Error::Gst(format!("切到 PLAYING 失败: {err}")))?;
        Ok(Self {
            pipeline: parts.pipeline,
            appsink: parts.appsink,
            description,
        })
    }

    /// 拉一个已解码的 sample（NV12）。`Ok(None)` = EOS。
    pub fn pull_decoded(&self) -> Result<Option<gst::Sample>> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(sample) = self
                .appsink
                .try_pull_sample(gst::ClockTime::from_mseconds(200))
            {
                return Ok(Some(sample));
            }
            if self.appsink.is_eos() {
                return Ok(None);
            }
            if let Some(error) = self.take_bus_error() {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(Error::Gst("拉取 sample 超时（20s）".into()));
            }
        }
    }

    pub fn appsink(&self) -> &gst_app::AppSink {
        &self.appsink
    }

    fn take_bus_error(&self) -> Option<Error> {
        let bus = self.pipeline.bus()?;
        while let Some(message) = bus.pop() {
            if let gst::MessageView::Error(err) = message.view() {
                return Some(bus_error(err));
            }
        }
        None
    }
}

impl Drop for GstSource {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl FrameSource for GstSource {
    fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        let Some(sample) = self.pull_decoded()? else {
            return Ok(None);
        };
        let caps = sample
            .caps()
            .ok_or_else(|| Error::Format("sample 没有 caps".into()))?;
        let info = gst_video::VideoInfo::from_caps(caps)
            .map_err(|err| Error::Format(format!("caps 无法解析为 VideoInfo: {err}")))?;
        let pts_ns = sample
            .buffer()
            .and_then(|buffer| buffer.pts())
            .map(|pts| pts.nseconds());
        let mut data = Vec::new();
        sample_to_rgba(&sample, &info, &mut data)?;
        Ok(Some(VideoFrame {
            width: info.width(),
            height: info.height(),
            stride: info.width() as usize * VideoFrame::BYTES_PER_PIXEL,
            data,
            pts_ns,
        }))
    }

    fn description(&self) -> SourceDescription {
        self.description.clone()
    }
}

/// sample（NV12 / I420 / RGBA）→ 紧凑 RGBA8，写进调用方给的缓冲区。
pub fn sample_to_rgba(
    sample: &gst::Sample,
    info: &gst_video::VideoInfo,
    out: &mut Vec<u8>,
) -> Result<()> {
    if !info.is_valid() {
        return Err(Error::Format("VideoInfo 无效（caps 不可用）".into()));
    }
    let buffer = sample
        .buffer_owned()
        .ok_or_else(|| Error::Format("sample 没有 buffer".into()))?;
    let frame = gst_video::VideoFrame::from_buffer_readable(buffer, info)
        .map_err(|_| Error::Gst("从 buffer 映射 VideoFrame 失败".into()))?;
    let width = info.width();
    let height = info.height();
    let strides: Vec<usize> = frame
        .plane_stride()
        .iter()
        .map(|stride| *stride as usize)
        .collect();
    match frame.format() {
        gst_video::VideoFormat::Nv12 => {
            let view = Nv12View {
                width,
                height,
                y: frame
                    .plane_data(0)
                    .map_err(|err| Error::Format(format!("取 Y 平面失败: {err}")))?,
                y_stride: strides[0],
                uv: frame
                    .plane_data(1)
                    .map_err(|err| Error::Format(format!("取 UV 平面失败: {err}")))?,
                uv_stride: strides[1],
            };
            convert::nv12_to_rgba(&view, out)
        }
        gst_video::VideoFormat::I420 => {
            let view = I420View {
                width,
                height,
                y: frame
                    .plane_data(0)
                    .map_err(|err| Error::Format(format!("取 Y 平面失败: {err}")))?,
                y_stride: strides[0],
                u: frame
                    .plane_data(1)
                    .map_err(|err| Error::Format(format!("取 U 平面失败: {err}")))?,
                u_stride: strides[1],
                v: frame
                    .plane_data(2)
                    .map_err(|err| Error::Format(format!("取 V 平面失败: {err}")))?,
                v_stride: strides[2],
            };
            convert::i420_to_rgba(&view, out)
        }
        gst_video::VideoFormat::Rgba => copy_packed_rgba(&frame, width, height, strides[0], out),
        other => Err(Error::Format(format!(
            "不支持的像素格式 {other}（期望 NV12 / I420 / RGBA）"
        ))),
    }
}

fn copy_packed_rgba(
    frame: &gst_video::VideoFrame<gst_video::video_frame::Readable>,
    width: u32,
    height: u32,
    stride: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let plane = frame
        .plane_data(0)
        .map_err(|err| Error::Format(format!("取 RGBA 平面失败: {err}")))?;
    let row_bytes = width as usize * VideoFrame::BYTES_PER_PIXEL;
    if stride < row_bytes {
        return Err(Error::Format(format!(
            "RGBA 行 stride={stride} 小于一行字节数 {row_bytes}"
        )));
    }
    out.clear();
    out.reserve(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * stride;
        out.extend_from_slice(&plane[start..start + row_bytes]);
    }
    Ok(())
}

/// 把一个 sample 的 pts 转成纳秒。
pub fn sample_pts_ns(sample: &gst::Sample) -> Option<u64> {
    sample
        .buffer()
        .and_then(|buffer| buffer.pts())
        .map(|pts| pts.nseconds())
}
