// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 引擎接缝：`PlaybackEngine` trait、打开请求、事件与诊断。
//!
//! 这一层只用 [`futures::Stream`] 描述事件，不假设引擎是库、子进程还是远端；
//! libmpv 实现把 mpv 的事件循环翻译成 [`EngineEvent`]，伪造实现（[`FakeEngine`](crate::FakeEngine)）
//! 用于测试。

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;

use crate::error::PlaybackError;
use crate::tracks::{SubtitleSelection, TrackSet};

/// 打开媒体的请求。
#[derive(Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// 播放直链。**含令牌**，不要写日志；需要记录时用 `{:?}`（已脱敏）。
    pub url: String,
    /// 从这里起播（续播位置，已经过 [`resume_position`](crate::resume_position) 处理）。
    pub resume_at: Duration,
    /// 要选的音轨序号；`None` 交给引擎默认轨。
    pub audio: Option<u32>,
    /// 要选的字幕。
    pub subtitle: SubtitleSelection,
    /// 额外 HTTP 头（认证、`User-Agent` 等）。值里可能有令牌，`Debug` 只打名字。
    pub headers: Vec<(String, String)>,
}

impl OpenRequest {
    /// 只带地址的最小请求（从 0 开始、引擎默认轨道、无额外头）。
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            resume_at: Duration::ZERO,
            audio: None,
            subtitle: SubtitleSelection::Off,
            headers: Vec::new(),
        }
    }

    /// 设置续播位置。
    pub fn with_resume_at(mut self, resume_at: Duration) -> Self {
        self.resume_at = resume_at;
        self
    }

    /// 设置音轨。
    pub fn with_audio(mut self, audio: Option<u32>) -> Self {
        self.audio = audio;
        self
    }

    /// 设置字幕。
    pub fn with_subtitle(mut self, subtitle: SubtitleSelection) -> Self {
        self.subtitle = subtitle;
        self
    }

    /// 追加一个 HTTP 头。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

impl fmt::Debug for OpenRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("OpenRequest")
            .field("url", &format_args!("{}", redact_url(&self.url)))
            .field("resume_at", &self.resume_at)
            .field("audio", &self.audio)
            .field("subtitle", &self.subtitle)
            .field("headers", &format_args!("{headers:?} <值已脱敏>"))
            .finish()
    }
}

/// 引擎事件。
#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    /// 首帧已就绪（起播成功的信号）。
    FirstFrame,
    /// 又有一帧上屏了。用来证明"引擎还活着"，也是帧 watchdog 的汇报点。
    FramePresented,
    /// 播放位置变化（引擎的权威位置）。
    PositionChanged(Duration),
    /// 进入 / 离开缓冲。
    Buffering(bool),
    /// 轨道集变化（首次枚举完成，或 seek / 重连后重新枚举）。
    TracksChanged(TrackSet),
    /// 播放到结尾。
    Ended,
    /// 引擎侧错误（已经分类好）。
    Error(PlaybackError),
    /// 累计丢帧数。
    DroppedFrames(u64),
}

/// 引擎自述的诊断信息。
///
/// 用来做两件事：断言（硬解到底有没有生效）、界面提示（"硬件解码未生效"）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineDiagnostics {
    /// 请求的硬解模式（`vaapi` / `videotoolbox` / `d3d11va` / `auto` / `no`）。
    pub requested_hwdec: Option<String>,
    /// 实际生效的硬解模式（mpv 的 `hwdec-current`）。
    pub active_hwdec: Option<String>,
    /// 是否真的走了硬解。
    pub hw_decode_active: bool,
    /// 当前视频解码器名（`hevc` / `h264` …）。
    pub video_decoder: Option<String>,
    /// 渲染后端（`opengl` / `gpu` / 自绘 FBO …）。
    pub render_backend: Option<String>,
}

impl EngineDiagnostics {
    /// 硬解是否被静默降级了。
    ///
    /// ADR-0001 实测：不传 DRM render node 时 mpv 会报 `Could not create a VA display.`
    /// 然后退成 `hwdec-current=no`，用户完全无感。所以这里必须显式比对。
    pub fn is_hwdec_degraded(&self) -> bool {
        match (&self.requested_hwdec, &self.active_hwdec) {
            (Some(requested), Some(active)) => !hwdec_is_acceptable(requested, active),
            // 读不到实际值意味着"还没到那一步"，不是降级。
            //
            // 这里原本把 `(Some, None)` 当成降级，结果首帧之前 `hwdec-current` 还没被 mpv 写入，
            // 于是每台机器都会先闪一条"硬件解码未生效"的假警报（实测于 Arc B580：
            // 警告文案说"实际未知"，同一帧之后 `hwdec-current=vaapi`）。
            // 判定必须在拿到真实值之后做，调用方应在首帧事件之后重新取一次诊断。
            (_, None) => false,
            (None, _) => false,
        }
    }

    /// 需要展示给用户的提示；`None` 表示一切正常。
    pub fn hwdec_notice(&self) -> Option<String> {
        if !self.is_hwdec_degraded() {
            return None;
        }
        let requested = self.requested_hwdec.as_deref().unwrap_or("自动");
        let active = self.active_hwdec.as_deref().unwrap_or("未知");
        Some(format!(
            "硬件解码未生效（请求 {requested}，实际 {active}），正在使用软件解码，4K 片源可能卡顿"
        ))
    }
}

/// 请求的硬解模式与 mpv 实际选中的模式是否算"成功"。
///
/// `auto*` 是让 mpv 自己挑，只要最终没落回 `no` 就算成功（`auto` 在 Arc B580 上会选中 `vaapi`）；
/// 显式请求某种硬解时要求精确匹配，`vaapi` 与 `vaapi-copy` 不算同一种（后者要走 GPU→CPU 回读，
/// 4K 下实测比软解还慢，正是 ADR-0001 记录的坑）。
pub fn hwdec_is_acceptable(requested: &str, active: &str) -> bool {
    match requested {
        "auto" | "auto-safe" | "auto-copy" => active != "no",
        other => other == active,
    }
}

/// 播放引擎。
///
/// 实现方要求：
///
/// - 所有命令都是**控制面**，不阻塞到画面出现；首帧通过 [`EngineEvent::FirstFrame`] 汇报。
/// - [`events`](PlaybackEngine::events) 返回的流在引擎生命周期内一直有效，不因 open/close 失效。
/// - 位置必须来自引擎自己的播放时钟（不是调用方的估算），否则缓冲期的位置会漂。
/// - 关闭时要把渲染上下文先销毁再释放句柄（见 ADR-0001 的"复用前必须加固"）。
#[async_trait]
pub trait PlaybackEngine: Send {
    /// 打开媒体并开始解码。返回的 `Ok` 只表示"已接受请求"，首帧由事件流给出。
    async fn open(&mut self, request: OpenRequest) -> Result<(), PlaybackError>;

    /// 播放。
    async fn play(&mut self) -> Result<(), PlaybackError>;

    /// 暂停。
    async fn pause(&mut self) -> Result<(), PlaybackError>;

    /// 跳转。
    async fn seek(&mut self, position: Duration) -> Result<(), PlaybackError>;

    /// 设置音量（`0.0..=1.0`）。
    async fn set_volume(&mut self, volume: f32) -> Result<(), PlaybackError>;

    /// 静音开关。
    async fn set_muted(&mut self, muted: bool) -> Result<(), PlaybackError>;

    /// 选择音轨。
    async fn select_audio(&mut self, index: u32) -> Result<(), PlaybackError>;

    /// 选择字幕（关闭 / 内嵌序号 / 外挂文件）。
    async fn select_subtitle(&mut self, selection: SubtitleSelection) -> Result<(), PlaybackError>;

    /// 字幕延迟（正数表示字幕更晚出现）。
    async fn set_subtitle_delay(&mut self, delay: Duration) -> Result<(), PlaybackError>;

    /// 字幕延迟，**允许负值**（负数表示字幕提前出现，即 mpv 的 `sub-delay` 为负）。
    ///
    /// [`Self::set_subtitle_delay`] 的签名是 `Duration`（只能非负），而实际调字幕
    /// 对齐时用户经常需要把字幕往前挪，所以单独给一个 `f64` 秒的入口。
    ///
    /// 默认实现把非负值转成 `Duration` 转交、负值直接忽略（不认识负偏移的引擎
    /// 至少不会因为"往前挪"这个动作报错）。
    async fn set_subtitle_delay_seconds(&mut self, seconds: f64) -> Result<(), PlaybackError> {
        if seconds.is_finite() && seconds >= 0.0 {
            self.set_subtitle_delay(Duration::from_secs_f64(seconds))
                .await
        } else {
            Ok(())
        }
    }

    /// 字幕底部安全区：把字幕往上抬 `y` 像素（mpv 的 `sub-margin-y`），`0` 表示归位。
    ///
    /// 用于控制层可见时避开传输行（`blend-subtitles=video` 时字幕烧在视频层，
    /// 界面上的控件会直接盖住它）。
    ///
    /// 默认空实现：不关心字幕排版的引擎（或测试替补）不必为了这个改代码。
    async fn set_subtitle_margin(&mut self, _y: i64) -> Result<(), PlaybackError> {
        Ok(())
    }

    /// 播放速度（`1.0` 为正常）。
    async fn set_rate(&mut self, rate: f32) -> Result<(), PlaybackError>;

    /// 关闭并释放资源。幂等。
    async fn close(&mut self) -> Result<(), PlaybackError>;

    /// 事件流。
    fn events(&mut self) -> &mut (dyn Stream<Item = EngineEvent> + Unpin + Send);

    /// 引擎自述：是否真的走了硬解、当前解码器、渲染后端。
    fn diagnostics(&self) -> EngineDiagnostics;
}

/// 把地址的查询串与片段替换成 `<已脱敏>`（播放直链的令牌就在查询串里）。
pub fn redact_url(url: &str) -> String {
    let head = url.split(['?', '#']).next().unwrap_or(url);
    if head.len() == url.len() {
        // 没有查询串：路径只含条目 / 资源 id，可以留作诊断线索。
        String::from(head)
    } else {
        format!("{head}?<已脱敏>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_request_debug_hides_url_and_header_values() {
        let request = OpenRequest::new("http://192.168.1.9:8096/videos/1/stream?api_key=SECRET")
            .with_resume_at(Duration::from_secs(90))
            .with_header("Authorization", "Bearer SECRET")
            .with_audio(Some(2))
            .with_subtitle(SubtitleSelection::Track(3));
        let debug = format!("{request:?}");
        assert!(!debug.contains("SECRET"), "{debug}");
        assert!(!debug.contains("api_key"), "{debug}");
        assert!(!debug.contains("Bearer"), "{debug}");
        // 主机名 + 路径本身不是秘密，留着有助于诊断。
        assert!(debug.contains("192.168.1.9"), "{debug}");
        assert!(debug.contains("Authorization"), "头部名字可以打：{debug}");
        assert!(debug.contains("90s"), "{debug}");
    }

    #[test]
    fn redact_url_keeps_host_and_drops_query() {
        assert_eq!(
            redact_url("http://h:8096/v/1/stream?api_key=x"),
            "http://h:8096/v/1/stream?<已脱敏>"
        );
        assert_eq!(redact_url("http://h/plain"), "http://h/plain");
    }

    #[test]
    fn hwdec_degradation_is_detected() {
        let ok = EngineDiagnostics {
            requested_hwdec: Some("vaapi".into()),
            active_hwdec: Some("vaapi".into()),
            hw_decode_active: true,
            ..EngineDiagnostics::default()
        };
        assert!(!ok.is_hwdec_degraded());
        assert_eq!(ok.hwdec_notice(), None);

        let silent_fallback = EngineDiagnostics {
            requested_hwdec: Some("vaapi".into()),
            active_hwdec: Some("no".into()),
            hw_decode_active: false,
            ..EngineDiagnostics::default()
        };
        assert!(silent_fallback.is_hwdec_degraded());
        let notice = silent_fallback.hwdec_notice().unwrap_or_default();
        assert!(notice.contains("硬件解码未生效"), "{notice}");
        assert!(notice.contains("vaapi"), "{notice}");

        // 读不到实际值 = "还没到那一步"，不是降级。旧实现把它当降级，
        // 结果首帧前每台机器都会先闪一条假警报（实测 Arc B580 上就是这样）。
        let unknown = EngineDiagnostics {
            requested_hwdec: Some("vaapi".into()),
            ..EngineDiagnostics::default()
        };
        assert!(!unknown.is_hwdec_degraded());

        let soft_by_choice = EngineDiagnostics {
            requested_hwdec: Some("no".into()),
            active_hwdec: None,
            ..EngineDiagnostics::default()
        };
        assert!(!soft_by_choice.is_hwdec_degraded());
    }
}
