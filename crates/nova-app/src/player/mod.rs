// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放页引擎接入：`nova-playback`（会话 / 进度 / 轨道）+ `nova-playback-mpv`（libmpv）+
//! Slint 的 OpenGL 渲染回调。
//!
//! # 线程模型（本模块的核心约束）
//!
//! ```text
//! Slint 主线程（winit 事件循环）                    tokio 工作线程
//! ─────────────────────────────────                 ─────────────────────────────
//! RenderingSetup  → MpvEngine::new + attach_gl      provider.stream_target()
//! BeforeRendering → engine.render(0, w, h)          provider.report_progress()
//! AfterRendering  →（捕获模式）glReadPixels
//! RenderingTeardown → engine close + drop
//! 8ms Timer       → 事件泵 / 会话 tick / 进度上报 / request_redraw
//! ```
//!
//! - **所有 GL 调用都在 Slint 主线程的 notifier 里**：`mpv_render_context_*` 必须在持有
//!   上下文的线程上调用，所以 `MpvEngine` 只被主线程碰，从不进 tokio。
//! - 网络（取直链、上报进度）在 tokio 上；结果通过 [`Message`] 回到主线程。
//! - mpv 自己的事件循环在独立线程（`nova-playback-mpv` 的 worker），主线程用 8ms 定时器
//!   把事件流泵空，**不阻塞** UI。
//! - 帧 watchdog：`MpvEngine::needs_redraw()` 为真就 `request_redraw()`，
//!   这样 Wayland 锁屏 / 窗口被遮挡导致合成器不发帧回调时画面仍然继续。

mod capture;
pub(crate) mod gl;
mod open;
mod sink;
mod view;

use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use futures::Stream;
use nova_core::{ItemId, PlayMethod, PlaybackProgress, UserPreferences};
use nova_playback::{
    EngineDiagnostics, EngineEvent, OpenRequest, PlaybackEngine, PlaybackError, ProgressTrigger,
    Session, SessionState, SubtitleSelection, SystemClock, TrackPlan, TrackSelector, TrackSet,
    resume_position_for_item,
};
use nova_playback_mpv::{EngineOptions, GlFunctions, MpvEngine};
use nova_ui::{App, AppWindow, PlayerView};
use slint::{ComponentHandle, GraphicsAPI, RenderingState};
use tokio::runtime::Handle;

use crate::error::AppError;
use crate::local::Local;

pub(crate) use capture::Options as CaptureOptions;

/// tick 间隔：60 fps 的视频需要 ≤16ms 的驱动；8ms 让 watchdog 与事件泵足够及时。
pub(crate) const TICK_INTERVAL: Duration = Duration::from_millis(8);

/// 界面刷新间隔（进度条 / 时间文本）。重要的状态变化会立即刷新。
const VIEW_INTERVAL: Duration = Duration::from_millis(500);

/// 这么久没有任何 `AfterRendering` 就认定合成器不再给帧回调（锁屏 / 遮挡）。
const PRESENT_STALL: Duration = Duration::from_millis(400);

/// 兜底驱动的最小间隔（`take_snapshot()` 会把整帧读回 CPU，别跑太密）。
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(16);

/// 没有服务器时播放失败的用户可见文案。
///
/// 这段文字直接进播放页的 `error`（桌面界面），所以只说应用内的入口
/// （与 `status.rs` 的横幅同一套措辞），不让用户去终端敲命令。
const NO_SERVER_PLAYBACK: &str =
    "还没有连接服务器：先在应用内连接（Ctrl+N，或首页「连接服务器」）再播放";

/// `--play` 的启动参数。
#[derive(Clone, Debug)]
pub struct StartPlay {
    /// 条目 id。
    pub item_id: String,
    /// 指定资源（版本）；`None` 用第一个。
    pub source_id: Option<String>,
    /// 捕获模式（`--capture`）。
    pub capture: Option<CaptureOptions>,
}

/// 后台 → 主线程的播放消息。
pub(crate) enum Message {
    /// 取直链完成（或失败）。
    OpenReady {
        /// 用来丢弃过期的请求（用户已经换了条目）。
        generation: u64,
        /// `OpenRequest` 或失败原因。
        request: Result<Box<OpenRequest>, OpenFailure>,
    },
}

/// 取直链失败的原因。
///
/// 保留 [`nova_provider::ProviderError`] 而不是当场转成文案：界面文案会随产品调整，
/// 调用方 / 测试要按**错误码**（`NoToken` / `Unauthorized` / `NotFound` …）判定，
/// 匹配字符串等于把文案钉死在测试里。
#[derive(Debug)]
pub(crate) enum OpenFailure {
    /// provider 返回的错误（没有令牌 / 未授权 / 找不到 / 网络）。
    Provider(nova_provider::ProviderError),
}

impl OpenFailure {
    /// 给界面看的文案。
    fn text(&self) -> String {
        match self {
            Self::Provider(error) => player_error_text(error),
        }
    }
}

/// 后台 → 主线程的投递函数（`Send`：它只把消息塞进跨线程队列并唤醒事件循环）。
pub(crate) type Post = Arc<dyn Fn(Message) + Send + Sync>;

/// 与渲染 notifier 共享的 GL / 引擎状态。
///
/// 只在主线程访问（`Rc<RefCell<_>>`）；notifier 与定时器都用 `try_borrow_mut`。
pub(crate) struct Host {
    /// 捕获模式：音频输出用 `null`（不出声，也不让音频设备影响掉帧统计）。
    silent: bool,
    /// 播放页是否在前台。
    ///
    /// **必须**用它门控 `before_rendering`：mpv 是画进窗口 back buffer 的，
    /// 首页若也照常渲染，整窗会被 mpv 的空白画面盖掉——表现为"没有主页、没有搜索框"。
    player_active: bool,
    engine: Option<MpvEngine>,
    gl: Option<GlFunctions>,
    readback: Option<gl::Readback>,
    /// `RenderingSetup` 失败的原因（界面读一次就清掉）。
    setup_error: Option<String>,
    /// 渲染路径上产生的事件（错误等），交给 tick 统一处理。
    events: Vec<EngineEvent>,
    /// 当前交给 Slint 的视频纹理包装。
    frame_image: Option<slint::Image>,
    texture_serial: u64,
    /// 累计渲染进纹理的帧数。
    frames: u64,
    /// 上一次 `BeforeRendering` 的窗口物理尺寸。
    window: (u32, u32),
    /// 已经渲染过至少一帧（说明纹理可用）。
    drew: bool,
    /// 待执行的 open（UI 线程填，tick 消费）。
    pending_open: Option<OpenRequest>,
    /// 捕获：下一次 `AfterRendering` 做回读。
    capture_requested: bool,
    /// 捕获：回读结果。
    shot: Option<gl::Frame>,
    /// 捕获：回读失败原因。
    shot_error: Option<String>,
    /// 上一次 `AfterRendering` 的时刻（判断合成器还在不在给帧回调）。
    last_present: Option<Instant>,
    /// 测试：实际交给引擎的字幕上抬量（百分比）序列。
    #[cfg(test)]
    subtitle_margins: Vec<i64>,
    /// 测试：与引擎 / 上报打交道的动作序列（`"pause"` / `"open:<url>"`）。
    ///
    /// 换片必须**先**停旧引擎、**再**上报进度：顺序错了用户就会听到两路声音，
    /// 或者界面被阻塞式补发卡住。测试 sink（`TracingSink`）往同一个 log 里写
    /// `"report:<secs>"` / `"stop:<secs>"`，才能断言两者之间的先后。
    #[cfg(test)]
    trace: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Host {
    /// 新建（还没有引擎；`RenderingSetup` 里才建）。
    pub(crate) fn new(silent: bool) -> Self {
        Self {
            silent,
            player_active: false,
            engine: None,
            gl: None,
            readback: None,
            setup_error: None,
            events: Vec::new(),
            frame_image: None,
            texture_serial: 0,
            frames: 0,
            window: (0, 0),
            drew: false,
            pending_open: None,
            capture_requested: false,
            shot: None,
            shot_error: None,
            last_present: None,
            #[cfg(test)]
            subtitle_margins: Vec::new(),
            #[cfg(test)]
            trace: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// 播放页进场/退场（由界面路由调用）。退出时同时清掉帧纹理，
    /// 否则返回首页后仍会显示最后一帧视频。
    pub(crate) fn set_player_active(&mut self, active: bool) {
        if self.player_active == active {
            return;
        }
        self.player_active = active;
        if !active {
            self.frame_image = None;
            self.drew = false;
        }
    }

    /// `RenderingSetup`：GL 上下文刚建好（current）。
    ///
    /// 在这里建 mpv 引擎并接入 GL。`attach_gl` 会把 DRM render node 交给 mpv——
    /// 这是 `hwdec=vaapi` 零拷贝生效的前提（ADR-0001 坑 1）。
    fn setup(&mut self, api: &GraphicsAPI<'_>) {
        if self.engine.is_some() {
            return;
        }
        let gl = match GlFunctions::from_slint_api(api) {
            Ok(gl) => gl,
            Err(error) => {
                self.setup_error = Some(error.hint().to_owned());
                tracing::warn!(%error, "无法从 Slint 装载 GL 入口点");
                return;
            }
        };
        if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api {
            // 回读入口点单独解析（Slint 不提供 glReadPixels）。
            self.readback = gl::Readback::load(*get_proc_address);
        }
        let options = if self.silent {
            // 捕获模式：不出声（`ao=null`），并把 mpv 的日志开到 info，起播失败时能看清原因。
            EngineOptions::new()
                .audio_output("null")
                .log_level("info")
                // 吞吐测量：不阻塞到目标显示时间（否则每帧固定占一个显示间隔，
                // 兜底驱动路径 18ms/帧的渲染节奏会把 60fps 素材挤掉几帧）。
                .block_for_target_time(false)
        } else {
            EngineOptions::new()
        };
        let mut engine = match MpvEngine::new(options) {
            Ok(engine) => engine,
            Err(error) => {
                self.setup_error = Some(error.hint().to_owned());
                tracing::warn!(%error, "创建 mpv 引擎失败");
                return;
            }
        };
        if let Err(error) = engine.attach_gl(gl) {
            self.setup_error = Some(error.hint().to_owned());
            tracing::warn!(%error, "mpv 接入 GL 失败");
            return;
        }
        tracing::info!(hwdec = %engine.diagnostics().requested_hwdec.clone().unwrap_or_default(), "mpv 引擎已接入 GL");
        self.gl = Some(gl);
        self.engine = Some(engine);
    }

    /// `BeforeRendering`：把 mpv 的新帧渲染进它自己的纹理。
    ///
    /// 注意 `render` 会阻塞到目标显示时间（A/V 同步的正确做法），所以每帧最多占一个
    /// 显示间隔；没有新帧时它立刻返回。
    fn before_rendering(&mut self, window: (u32, u32)) {
        self.window = window;
        // 不在播放页就什么都不画：mpv 的渲染目标是窗口 back buffer，
        // 无条件渲染会把首页/详情页整屏覆盖成 mpv 的空白画面。
        if !self.player_active {
            return;
        }
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        if window.0 == 0 || window.1 == 0 {
            return;
        }
        match engine.render(0, window.0, window.1) {
            Ok(true) => {
                self.frames = self.frames.saturating_add(1);
                self.drew = true;
                tracing::trace!(frames = self.frames, "渲染了一帧");
            }
            Ok(false) => {}
            Err(error) => self.events.push(EngineEvent::Error(error)),
        }
    }

    /// `AfterRendering`：Slint 已经把整帧画进 back buffer。捕获模式在这里回读。
    fn after_rendering(&mut self) {
        self.last_present = Some(Instant::now());
        if !self.capture_requested || self.shot.is_some() {
            return;
        }
        self.capture_requested = false;
        let Some(readback) = self.readback else {
            self.shot_error =
                Some("当前渲染后端不提供 glReadPixels（需要 OpenGL 后端）".to_owned());
            return;
        };
        if self.window.0 == 0 || self.window.1 == 0 {
            self.shot_error = Some("窗口尺寸为 0，无法回读".to_owned());
            return;
        }
        match readback.read(self.window.0, self.window.1) {
            Ok(frame) => {
                tracing::info!(
                    width = frame.width,
                    height = frame.height,
                    mean = frame.stats.mean_luma,
                    variance = frame.stats.variance,
                    "回读窗口 back buffer"
                );
                self.shot = Some(frame);
            }
            Err(error) => self.shot_error = Some(error),
        }
    }

    /// `RenderingTeardown`：上下文仍然 current，先关引擎再放句柄。
    fn teardown(&mut self) {
        self.frame_image = None;
        self.readback = None;
        self.gl = None;
        // `MpvEngine::drop` 会先释放 render context（要求上下文 current），再销毁 core。
        self.engine = None;
    }

    /// 把引擎纹理包成 Slint 的 `Image`（纹理重建过才重做）。
    fn refresh_frame_image(&mut self) -> bool {
        let Some((serial, texture)) = self
            .engine
            .as_ref()
            .map(|engine| (engine.texture_serial(), engine.texture()))
        else {
            return false;
        };
        if serial == self.texture_serial && self.frame_image.is_some() {
            return false;
        }
        let Some((texture, width, height)) = texture else {
            return false;
        };
        match borrowed_texture(texture, width, height) {
            Some(image) => {
                self.frame_image = Some(image);
                self.texture_serial = serial;
                true
            }
            None => false,
        }
    }

    /// 当前帧图（给 `PlayerView` 用）。
    fn frame_image(&self) -> Option<slint::Image> {
        self.frame_image.clone()
    }

    /// 视频尺寸（`texture()` 里有，用于诊断行）。
    fn video_size(&self) -> Option<(u32, u32)> {
        let (_, width, height) = self.engine.as_ref()?.texture()?;
        Some((width, height))
    }

    fn diagnostics(&self) -> Option<EngineDiagnostics> {
        self.engine.as_ref().map(MpvEngine::diagnostics)
    }

    fn dropped_frames(&self) -> u64 {
        self.engine
            .as_ref()
            .map(MpvEngine::dropped_frames)
            .unwrap_or_default()
    }

    fn first_frame_latency(&self) -> Option<Duration> {
        self.engine.as_ref()?.first_frame_latency()
    }

    fn needs_redraw(&self) -> bool {
        self.engine
            .as_ref()
            .map(MpvEngine::needs_redraw)
            .unwrap_or(false)
    }

    /// 泵空引擎事件流（非阻塞：没有事件时立刻返回）。
    fn drain_events(&mut self) -> Vec<EngineEvent> {
        let Some(engine) = self.engine.as_mut() else {
            return std::mem::take(&mut self.events);
        };
        let mut drained = drain_stream(engine.events());
        drained.append(&mut self.events);
        drained
    }

    /// 测试注入：把一条引擎事件排进待处理队列。
    ///
    /// 真实路径上这些事件来自 `drain_stream(engine.events())`；测试里没有 mpv，
    /// 但 `pump_events` 对它们的处理（代次门、会话门）是一致的。
    #[cfg(test)]
    fn push_event(&mut self, event: EngineEvent) {
        self.events.push(event);
    }

    /// 如果引擎已接入并且有排队的 open，就现在发起。
    fn try_open(&mut self, runtime: &Handle) -> Option<Result<(), PlaybackError>> {
        let engine = self.engine.as_mut()?;
        let request = self.pending_open.take()?;
        tracing::info!(?request, "发起播放");
        Some(runtime.block_on(engine.open(request)))
    }

    fn enqueue_open(&mut self, request: OpenRequest) {
        #[cfg(test)]
        self.trace_push(format!("open:{}", request.url));
        self.pending_open = Some(request);
    }

    /// 测试：往共享 trace 里记一笔（与测试 sink 的 `report` / `stop` 同一条时间线）。
    #[cfg(test)]
    fn trace_push(&self, entry: String) {
        self.trace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry);
    }

    fn play(&mut self, runtime: &Handle) {
        #[cfg(test)]
        self.trace_push("play".to_owned());
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.play())
        {
            tracing::warn!(%error, "engine.play 失败");
        }
    }

    fn pause(&mut self, runtime: &Handle) {
        #[cfg(test)]
        self.trace_push("pause".to_owned());
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.pause())
        {
            tracing::warn!(%error, "engine.pause 失败");
        }
    }

    fn seek(&mut self, runtime: &Handle, position: Duration) {
        #[cfg(test)]
        self.trace_push(format!("seek:{}", position.as_secs()));
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.seek(position))
        {
            tracing::warn!(%error, "engine.seek 失败");
        }
    }

    fn set_volume(&mut self, runtime: &Handle, volume: f32) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.set_volume(volume))
        {
            tracing::warn!(%error, "engine.set_volume 失败");
        }
    }

    fn set_muted(&mut self, runtime: &Handle, muted: bool) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.set_muted(muted))
        {
            tracing::warn!(%error, "engine.set_muted 失败");
        }
    }

    /// 字幕延迟（秒，可为负）：`sub-delay`。
    fn set_subtitle_delay(&mut self, _runtime: &Handle, seconds: f64) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = engine.set_subtitle_delay_seconds(seconds)
        {
            tracing::warn!(%error, "engine.set_subtitle_delay_seconds 失败");
        }
    }

    /// 字幕安全区：把字幕抬到控制层上方。
    ///
    /// 传入的 `lift_percent` 是**屏幕高度的百分比**（0 = 原位）。窗口像素 → 百分比的
    /// 换算在 [`Player::set_subtitle_margin`] 里做（那里才知道窗口高度）；
    /// mpv 那边只有百分比属性（`sub-pos`）真的能抬动 ASS 字幕。
    fn set_subtitle_margin(&mut self, runtime: &Handle, lift_percent: i64) {
        #[cfg(test)]
        self.subtitle_margins.push(lift_percent);
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.set_subtitle_margin(lift_percent))
        {
            tracing::warn!(%error, "engine.set_subtitle_margin 失败");
        }
    }

    fn set_rate(&mut self, runtime: &Handle, rate: f32) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.set_rate(rate))
        {
            tracing::warn!(%error, "engine.set_rate 失败");
        }
    }

    fn select_audio(&mut self, runtime: &Handle, index: u32) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.select_audio(index))
        {
            tracing::warn!(%error, "engine.select_audio 失败");
        }
    }

    fn select_subtitle(&mut self, runtime: &Handle, selection: SubtitleSelection) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.select_subtitle(selection))
        {
            tracing::warn!(%error, "engine.select_subtitle 失败");
        }
    }

    /// 请求一次回读（下一次 `AfterRendering` 执行）。
    pub(crate) fn request_capture(&mut self) {
        if self.capture_requested || self.shot.is_some() {
            return;
        }
        self.capture_requested = true;
    }

    pub(crate) fn take_shot(&mut self) -> Option<(gl::Frame, Option<String>)> {
        let error = self.shot_error.take();
        match self.shot.take() {
            Some(frame) => Some((frame, error)),
            None => error.map(|error| {
                (
                    gl::Frame {
                        width: 0,
                        height: 0,
                        rgba: Vec::new(),
                        stats: gl::PixelStats::default(),
                    },
                    Some(error),
                )
            }),
        }
    }

    /// 合成器已经不再给帧回调（Wayland 锁屏 / 窗口被遮挡时的实测行为）。
    ///
    /// `docs`+`spikes/slint-mpv-gl/FINDINGS.md` §5.5：会话锁屏后 winit 收不到
    /// `wl_surface.frame`，`request_redraw()` 变成空操作，Slint 只渲染过一帧。
    fn presenter_stalled(&self) -> bool {
        self.last_present
            .map(|at| at.elapsed() >= PRESENT_STALL)
            .unwrap_or(false)
    }

    /// 是否有回读在路上（请求了还没拿到 / 已拿到但还没被取走）。
    fn capture_waiting(&self) -> bool {
        self.capture_requested || self.shot.is_some()
    }

    /// 合成器帧回调的最近时刻（快照兜底驱动期间要被还原，否则自己把自己骗过去）。
    fn present_marker(&self) -> Option<Instant> {
        self.last_present
    }

    fn restore_present(&mut self, marker: Option<Instant>) {
        self.last_present = marker;
    }

    fn take_setup_error(&mut self) -> Option<String> {
        self.setup_error.take()
    }
}

/// 把 `BorrowedOpenGLTextureBuilder` 的纹理包成 `slint::Image`。
///
/// Slint 的这个构造函数在库内部是 `unsafe`：它要求纹理由**当前 GL 上下文**创建，
/// 并且生命周期覆盖 Slint 使用它的整段时间。两个条件都由本模块保证：
///
/// 1. 纹理来自同一个 `RenderingSetup` 里 `attach_gl` 的上下文；
/// 2. 纹理由 `MpvEngine` 持有，引擎只在 `RenderingTeardown` 里释放（那时 Slint 已经
///    不再合成任何东西了）。
#[allow(unsafe_code)]
fn borrowed_texture(texture: u32, width: u32, height: u32) -> Option<slint::Image> {
    let texture = std::num::NonZeroU32::new(texture)?;
    let size = i_slint_core::graphics::IntSize::new(width, height);
    // SAFETY: 见函数文档——纹理与当前上下文同源，且引擎的存活期覆盖所有使用点。
    Some(unsafe {
        slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(texture, size)
            // 实测结论（同一个 FBO 用两条路径交叉验证）：
            // - 回读路径（`player/gl.rs` 里把行序反转后写 PNG）得到的是**正立**画面；
            // - 交给 Slint 时声明 `BottomLeft` 的快照是**上下颠倒**的（字幕倒着）。
            // 说明 mpv 在 `FLIP_Y = 0` 下写进 FBO 的纹理第 0 行就是画面顶部，
            // 且 `BorrowedOpenGLTextureOrigin` 描述的是"图像数据的原点在哪个角"，
            // 传 `BottomLeft` 会让 Slint 再翻一次。故必须用 `TopLeft`。
            .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
            .build()
    })
}

/// 把引擎事件流泵空（`poll_next` + noop waker：不注册、不 await）。
fn drain_stream(stream: &mut (dyn Stream<Item = EngineEvent> + Unpin + Send)) -> Vec<EngineEvent> {
    let waker: &Waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut pinned = Pin::new(stream);
    let mut events = Vec::new();
    while let Poll::Ready(Some(event)) = pinned.as_mut().poll_next(&mut context) {
        events.push(event);
    }
    events
}

/// 安装 Slint 的渲染 notifier（每个窗口一次）。
pub(crate) fn install_notifier(
    window: &slint::Window,
    host: &Rc<RefCell<Host>>,
    ui: slint::Weak<AppWindow>,
) -> Result<(), crate::error::AppError> {
    let host = Rc::clone(host);
    window
        .set_rendering_notifier(move |state, api| {
            let Ok(mut host) = host.try_borrow_mut() else {
                tracing::warn!("渲染回调重入：跳过这一帧");
                return;
            };
            tracing::trace!(?state, "渲染回调");
            match state {
                RenderingState::RenderingSetup => host.setup(api),
                RenderingState::BeforeRendering => {
                    let size = ui
                        .upgrade()
                        .map(|ui| ui.window().size())
                        .unwrap_or_else(|| slint::PhysicalSize::new(0, 0));
                    host.before_rendering((size.width, size.height));
                }
                RenderingState::AfterRendering => host.after_rendering(),
                RenderingState::RenderingTeardown => host.teardown(),
                _ => {}
            }
        })
        .map_err(|error| AppError::Playback(format!("安装渲染回调失败：{error}")))
}

/// 捕获结果的共享槽（`ui` 模块在事件循环结束后读它，决定退出码）。
pub(crate) type CaptureSlot = Rc<RefCell<Option<capture::Outcome>>>;

/// 结束会话时的收尾方式（[`Player::end_session`]）。
///
/// 两者的区别只在**要不要在界面线程上有界地等一次补发**：换片一秒都不能停，
/// 离开播放页则可以等（补发结果直接决定下次的续播点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionEnd {
    /// 换片：引擎先停，进度交给 worker 异步补发。
    Switch,
    /// 离开播放页：有界等待一次补发（`sink::FLUSH_TIMEOUT`）。
    Exit,
}

/// 播放页控制器（主线程；持有会话、进度上报与界面视图）。
/// 自动重连次数上限（0.5+1+2+4+8 秒退避，合计约 15 秒）。
const MAX_AUTO_RETRY: u32 = 5;

pub(crate) struct Player {
    app: Rc<App>,
    local: Arc<Local>,
    runtime: Handle,
    host: Rc<RefCell<Host>>,
    progress: sink::Progress,
    capture_out: CaptureSlot,
    selector: TrackSelector,
    session: Session,
    prefs: UserPreferences,
    /// 自动重连：已尝试次数与下一次尝试的时间点。
    ///
    /// 断线在家庭网络里是常态（Wi-Fi 切换、NAS 重启、笔记本合盖），
    /// 一次抖动就弹错误卡让用户手点重试是很糟的体验。
    auto_retry: u32,
    retry_at: Option<std::time::Instant>,
    /// 本次会话拿到的请求（重试用）。
    request: Option<OpenRequest>,
    /// 当前条目 / 资源。
    target: Option<Target>,
    /// 待播放的条目 / 资源（取直链期间就有）。
    pending: Option<Target>,
    /// 最后一次起播目标的快照（条目 + 版本），取直链之前就记下。
    ///
    /// 重试必须能用它重跑一遍 [`Player::begin`]：取直链失败的路径会把
    /// `request` / `pending` / `target` 全部清空，没有快照时错误卡上的「重试」
    /// 会静默地什么都不做（点了没反应）。
    last_start: Option<StartPoint>,
    /// 引擎枚举出的轨道（`TracksChanged` 之前用服务端资源的兜底）。
    tracks: TrackSet,
    selected_audio: Option<u32>,
    selected_subtitle: SubtitleSelection,
    /// 起播失败 / 无服务器等界面错误（不是会话状态机给的）。
    error: Option<String>,
    /// 取直链的代数（丢弃过期结果）。
    generation: u64,
    /// 播放会话代号：每次 [`Player::begin`] 递增，随每条 [`PlaybackProgress`] 上报。
    ///
    /// 上报器与 sink 靠它区分"同一部片重播"与"同一个会话"；有它才能保证重播时
    /// 队列里上一会话的待发项不把新会话的第一条上报吞掉，补发旧 `Exit` 时也不会
    /// 给已经 `stop` 的会话再发一次 `Started`。
    session_seq: u64,
    /// **本会话**的服务端会话标识（`PlaybackInfo` 给的 `PlaySessionId`）。
    ///
    /// 在起播前那次 `PlaybackInfo` 成功返回的那一刻记下来（[`Player::refresh_before_play`]），
    /// 由 [`Player::build_progress_at`] 写进每条进度。不能依赖 provider 的
    /// "条目最近一次"映射：同一部片播放期间详情页也会取 `PlaybackInfo`
    /// （`Shell::spawn_detail`），会把条目上的记录覆盖成第三个标识。
    ///
    /// `None` 表示**不知道**（起播前刷新超时 / 失败，没有拿到 `PlaybackInfo`）。
    play_session_id: Option<String>,
    /// 测试：替换 [`Local::provider_dyn`]。
    ///
    /// 真 provider 的会话标识只能从网络拿到（`PlaybackInfo`），player 级用例用假
    /// provider 才能确定地给出"起播 / 详情刷新 / 重播各一个标识"。
    #[cfg(test)]
    provider: Option<Arc<dyn nova_provider::MediaProvider>>,
    /// 捕获（`--capture`）。
    capture: Option<capture::Capture>,
    /// 首帧是否已经到过（捕获计时起点）。
    saw_first_frame: bool,
    /// 取直链结果的投递入口（`retry` 重走准备流程时用）。
    post: Option<Post>,
    last_push: Instant,
    /// 状态变化需要立刻推界面。
    dirty: bool,
    /// 待提交给界面的视图（见 [`Player::push_view`]）。
    pending_view: Option<PlayerView>,
    /// 上一次用 `take_snapshot()` 兜底驱动渲染的时刻。
    last_snapshot: Option<Instant>,
    volume: f32,
    muted: bool,
    rate: f32,
    /// 当前字幕上抬量（屏幕高度的百分比，`0` = 原位）：避免重复下发。
    subtitle_margin: i64,
    /// 当前字幕延迟（秒，可为负）：菜单显示用，同时下发给 mpv `sub-delay`。
    subtitle_delay: f64,
}

/// 当前播放目标。
#[derive(Clone, Debug)]
struct Target {
    item_id: ItemId,
    source_id: nova_core::SourceId,
    title: String,
    subtitle: Option<String>,
    duration: Option<Duration>,
}

/// 最后一次起播的参数（重试用）。
///
/// 只存 [`Player::begin`] 需要的两样东西：条目与版本（`None` = 自动挑）。
/// 偏好**不存快照**：重试与重播一样传 `None`，由 `begin` 读数据库里的当前值——
/// 运行中改过的偏好（续播阈值、轨道语言）不能因为一次重试就退回启动时间的快照。
#[derive(Clone, Debug)]
struct StartPoint {
    item_id: String,
    source_id: Option<String>,
}

impl Player {
    /// 新建控制器。`capture_options` 非空时启用 `--capture`。
    pub(crate) fn new(
        app: Rc<App>,
        local: Arc<Local>,
        runtime: Handle,
        host: Rc<RefCell<Host>>,
        capture_options: Option<CaptureOptions>,
        capture_out: CaptureSlot,
    ) -> Self {
        let progress = sink::Progress::start(&local, &runtime);
        let prefs = local.preferences.clone();
        Self {
            auto_retry: 0,
            retry_at: None,
            app,
            local,
            runtime,
            host,
            progress,
            capture_out,
            selector: TrackSelector::new(),
            session: Session::new(Arc::new(SystemClock)),
            prefs,
            request: None,
            target: None,
            last_start: None,
            pending: None,
            tracks: TrackSet::default(),
            selected_audio: None,
            selected_subtitle: SubtitleSelection::Off,
            error: None,
            generation: 0,
            session_seq: 0,
            play_session_id: None,
            #[cfg(test)]
            provider: None,
            capture: capture_options.map(capture::Capture::new),
            saw_first_frame: false,
            post: None,
            last_push: Instant::now(),
            dirty: true,
            pending_view: None,
            last_snapshot: None,
            volume: 1.0,
            muted: false,
            rate: 1.0,
            subtitle_margin: 0,
            subtitle_delay: 0.0,
        }
    }

    /// 捕获宿主（`ui` 模块读结果用）。
    /// 开始播放一个条目。
    ///
    /// 只做本地准备（条目 / 版本 / 轨道 / 续播位置），取直链的网络请求在 tokio 上，
    /// 结果通过 `post` 回到主线程。
    ///
    /// `prefs` 是起播路径**已经读好**的偏好（`Shell::play` 与播放页初始位置共用同一份，
    /// 偏好只读一次）；`None` 时自己读数据库（重试等自发起播）。
    pub(crate) fn begin(
        &mut self,
        item_id: &str,
        source_id: Option<String>,
        prefs: Option<UserPreferences>,
        post: &Post,
    ) {
        // 播完之后再播**同一个条目** = 重播：用户的明确操作，必须从 0 开始。
        //
        // 不能沿用续播点：播完的条目在服务端/本地缓存里的续播点常常还贴在片尾
        // （体检 R8-B1/B2 的真机现象就是“按了播放键，画面还是最后一帧”），续上去
        // 等于什么也没发生。会话代号照常递增：同一部片重播也是**新会话**。
        let replay = self.session.state() == SessionState::Ended
            && self
                .target
                .as_ref()
                .or(self.pending.as_ref())
                .is_some_and(|target| target.item_id.as_str() == item_id);
        // 换片：先把旧会话收尾。只 `session.reset()`（旧实现）会让旧片子的最后一段进度
        // 永远上不了报：Exit 提交、`stop`、补发都不发生，服务端的续播点停在上一次
        // 定时上报的位置，服务端也一直不知道这次会话已经结束。
        if self.session.state() != SessionState::Idle {
            // 换片：引擎**立刻**停（旧片的声音不能等到上报补发完），进度交给
            // 上报 worker 异步补发，绝不在这条路径上 `block_on`。
            self.end_session(SessionEnd::Switch);
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        // 起播目标快照：取直链失败会把 request / pending / target 全清掉，
        // 错误卡上的「重试」只能靠它重跑一遍起播。
        self.last_start = Some(StartPoint {
            item_id: item_id.to_owned(),
            source_id: source_id.clone(),
        });
        // 换片 / 重播：旧会话的 `Exit` 已在上面用**旧代号**交出去了，这里再递增。
        // 代号 `0` 留给"没有代号"的旧数据，所以从 1 开始。
        self.session_seq = self.session_seq.saturating_add(1).max(1);
        // 旧会话的 `Exit` 已经在上面用旧标识交出去了，这里清掉；本会话的标识要等
        // 起播前的 `PlaybackInfo` 回来（`refresh_before_play`）才知道。
        self.play_session_id = None;
        self.error = None;
        self.saw_first_frame = false;
        self.request = None;
        self.auto_retry = 0;
        self.retry_at = None;
        self.pending = None;
        self.target = None;
        self.post = Some(Arc::clone(post));
        self.session.reset();
        self.selector.clear();
        self.selected_audio = None;
        self.selected_subtitle = SubtitleSelection::Off;
        self.tracks = TrackSet::default();
        self.dirty = true;
        self.subtitle_margin = 0;

        // 倍速 / 字幕延迟是 mpv 的引擎级属性：上一部片子调过的值如果不复位，
        // 新片子会直接沿用（体检 R3-3 的“会话级状态全部复位”）。
        if self.rate != 1.0 {
            self.rate = 1.0;
            let runtime = self.runtime.clone();
            self.host.borrow_mut().set_rate(&runtime, 1.0);
        }
        if self.subtitle_delay != 0.0 {
            self.subtitle_delay = 0.0;
            let runtime = self.runtime.clone();
            self.host.borrow_mut().set_subtitle_delay(&runtime, 0.0);
        }

        // 界面上的会话级状态先整体复位：即使后面在“取条目 / 取直链”这一步就失败，
        // 错误页也不能显示上一部片子的进度、时长、标题、轨道（体检 R3-3）。
        let mut view = self.app.player_view();
        view.reset_session();
        self.pending_view = Some(view);

        let Some(server) = self.local.server_id() else {
            self.fail(NO_SERVER_PLAYBACK);
            return;
        };
        let id = ItemId::new(item_id);
        let item = match self.local.db.item(&server, &id) {
            Ok(Some(item)) => item,
            Ok(None) => {
                self.fail("本地缓存里没有这个条目：回到详情页刷新一次再试");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "读取条目失败");
                self.fail("读取本地缓存失败，详见日志");
                return;
            }
        };
        let sources = match self.local.db.sources(&server, &id) {
            Ok(sources) => sources,
            Err(error) => {
                tracing::warn!(%error, "读取播放源失败");
                Vec::new()
            }
        };

        // 起播前向服务端要一次最新状态：**续播点只有服务端的是准的**，
        // 本地缓存里的 `resume_position` 可能是几分钟前（甚至 0）的快照，
        // 用它会导致"上次看到 45 秒，这次又从 0 开始"。
        // 超时/失败一律退回缓存，绝不让起播卡在网络。
        let (item, sources, play_session_id) = self.refresh_before_play(item, sources);
        self.play_session_id = play_session_id;
        let Some(source) = pick_source(&sources, source_id.as_deref()) else {
            self.fail("这个条目没有可播放的版本：确认服务端上文件还在");
            return;
        };

        // 续播阈值 / 轨道偏好以**数据库里的当前值**为准，而不是启动时的快照：
        // 详情页（`views::detail_view`）每次读 `db.preferences()`，起播若用启动快照，
        // 运行中改过偏好就会出现“详情说从头播、起播却续在那里”。
        // 起播路径（`Shell::play`）已经把偏好读好并传进来，与播放页初始位置同源；
        // 只有自己发起的起播（重试）才读数据库，读失败退回启动快照
        // （离线优先：不因偏好读失败拦住起播）。
        let prefs = match prefs {
            Some(prefs) => {
                self.prefs = prefs.clone();
                prefs
            }
            None => match self.local.db.preferences() {
                Ok(prefs) => {
                    self.prefs = prefs.clone();
                    prefs
                }
                Err(error) => {
                    tracing::warn!(%error, "读取用户偏好失败，沿用启动快照");
                    self.prefs.clone()
                }
            },
        };
        let resume_at = if replay {
            // 重播不读续播点：起播位置由 `Session::begin` / `OpenRequest` 的
            // 0 决定，服务端那边也会因为新会话的第一个 `Started` 重新开始。
            Duration::ZERO
        } else {
            resume_position_for_item(&item, &prefs)
        };
        let duration = source.runtime.or(item.runtime);
        self.tracks = TrackSet::from_source(&source);
        let plan = self.selector.reapply(&self.tracks, &prefs);
        self.apply_plan_locally(&plan);
        self.session.begin(resume_at, duration);

        let title = item.title.clone();
        let subtitle = crate::views::subtitle_for(&item);
        self.pending = Some(Target {
            item_id: item.id.clone(),
            source_id: source.id.clone(),
            title: title.clone(),
            subtitle,
            duration,
        });
        self.target = None;

        // 标题先上界面（起播期间不黑屏、不留空白标题）。
        let mut view = self
            .pending_view
            .take()
            .unwrap_or_else(|| self.app.player_view());
        view.title = title;
        view.duration = duration;
        view.position = resume_at;
        view.buffering = false;
        view.playing = true;
        self.pending_view = Some(view);

        let Some(provider) = self.provider() else {
            self.fail(NO_SERVER_PLAYBACK);
            return;
        };
        let post = Arc::clone(post);
        let item_id = item.id.clone();
        let source_id = source.id.clone();
        self.runtime.spawn(async move {
            let request = match provider.stream_target(&item_id, &source_id).await {
                Ok(target) => Ok(Box::new(open::build_request(
                    target.url(),
                    resume_at,
                    &plan,
                ))),
                Err(error) => {
                    tracing::warn!(%error, "取播放直链失败");
                    Err(OpenFailure::Provider(error))
                }
            };
            post(Message::OpenReady {
                generation,
                request,
            });
        });
    }

    /// 取直链的结果回来了。
    pub(crate) fn on_open_ready(
        &mut self,
        generation: u64,
        request: Result<Box<OpenRequest>, OpenFailure>,
    ) {
        if generation != self.generation {
            return;
        }
        match request {
            Ok(request) => {
                self.target = self.pending.take();
                self.request = Some((*request).clone());
                self.host.borrow_mut().enqueue_open(*request);
                self.dirty = true;
            }
            Err(failure) => {
                self.pending = None;
                let message = failure.text();
                self.fail(&message);
            }
        }
    }

    /// 8ms 心跳。
    pub(crate) fn tick(&mut self) {
        let setup_error = self.host.borrow_mut().take_setup_error();
        if let Some(error) = setup_error {
            self.fail(&error);
        }
        self.ensure_open();
        self.pump_events();
        self.reconnect_if_due();

        if let Some(error) = self.session.tick() {
            tracing::warn!(%error, "会话判定卡死");
            self.dirty = true;
        }

        // 帧 watchdog：有新帧 / 长时间没渲染就请求一次重绘。
        let needs_redraw = self.host.borrow().needs_redraw();
        if needs_redraw {
            self.request_redraw();
        }
        // 合成器不给帧回调（锁屏 / 遮挡）时改用 `take_snapshot()` 主动驱动一轮完整渲染，
        // `RenderingSetup`/`BeforeRendering`/`AfterRendering` 都会照常跑到。起播阶段
        // mpv 还没有 `has_video`，所以不能等 `needs_redraw()`——必须无条件（限速）驱动。
        if self.host.borrow().presenter_stalled() && (needs_redraw || self.wants_drive()) {
            self.snapshot_draw();
        }
        if self.host.borrow_mut().refresh_frame_image() {
            self.dirty = true;
        }
        self.session_ticks();
        self.capture_step();
        self.push_view(false);
        if self.host.borrow().needs_redraw() {
            self.request_redraw();
        }
    }

    /// 记录首帧、按间隔上报进度。
    /// 起播 / 播放 / 缓冲 / 等待捕获时需要有人不停驱动渲染。
    fn wants_drive(&self) -> bool {
        if matches!(
            self.session.state(),
            SessionState::Opening | SessionState::Playing | SessionState::Buffering
        ) {
            return true;
        }
        if self.host.borrow().capture_waiting() {
            return true;
        }
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.due() && !capture.captured())
    }

    /// 用 `take_snapshot()` 主动驱动一帧（无合成器帧回调时的兜底）。
    fn snapshot_draw(&mut self) {
        if let Some(at) = self.last_snapshot
            && at.elapsed() < SNAPSHOT_INTERVAL
        {
            return;
        }
        self.last_snapshot = Some(Instant::now());
        let marker = self.host.borrow().present_marker();
        let result = self.app.ui().window().take_snapshot();
        self.host.borrow_mut().restore_present(marker);
        match result {
            Ok(buffer) => tracing::trace!(
                width = buffer.width(),
                height = buffer.height(),
                "take_snapshot 兜底驱动"
            ),
            Err(error) => tracing::debug!(?error, "take_snapshot 兜底驱动失败"),
        }
    }

    fn session_ticks(&mut self) {
        let state = self.session.state();
        if !self.saw_first_frame
            && matches!(
                state,
                SessionState::Playing | SessionState::Paused | SessionState::Buffering
            )
        {
            self.saw_first_frame = true;
            let position = self.session.position();
            if let Some(capture) = self.capture.as_mut() {
                capture.note_first_frame(position);
            }
            if let Some(diagnostics) = self.host.borrow().diagnostics() {
                self.session.set_diagnostics(diagnostics);
            }
            self.dirty = true;
        }
        let progress = self.build_progress();
        if let Some(progress) = progress {
            self.progress.submit(ProgressTrigger::Tick, progress);
        }
    }

    /// 捕获模式：到点就回读、写 PNG、打印指标、退出事件循环。
    fn capture_step(&mut self) {
        let Some(capture) = self.capture.as_ref() else {
            return;
        };
        if capture.captured() {
            return;
        }
        if !capture.due() {
            return;
        }
        {
            let mut host = self.host.borrow_mut();
            if !host.drew {
                return;
            }
            host.request_capture();
        }
        self.request_redraw();
        let shot = self.host.borrow_mut().take_shot();
        let Some((frame, error)) = shot else {
            return;
        };
        self.finish_capture(frame, error);
    }

    fn finish_capture(&mut self, frame: gl::Frame, error: Option<String>) {
        let Some(capture) = self.capture.as_mut() else {
            return;
        };
        capture.mark_captured();
        let total_frames = self.host.borrow().frames;
        let dropped = self.host.borrow().dropped_frames();
        let diagnostics = self.host.borrow().diagnostics().unwrap_or_default();
        let latency = self.host.borrow().first_frame_latency();
        let window = self.host.borrow().window;
        let hwdec = diagnostics
            .active_hwdec
            .clone()
            .unwrap_or_else(|| "no".to_owned());
        if let Some(error) = error {
            tracing::warn!(%error, "回读窗口失败");
        }
        if let Err(error) = gl::write_png(capture.path(), &frame) {
            tracing::warn!(%error, "PNG 写盘失败");
        }
        let outcome = capture.finish(
            self.session.position(),
            total_frames,
            dropped,
            hwdec,
            latency,
            window,
            self.session.duration(),
            &frame.stats,
        );
        outcome.print();
        *self.capture_out.borrow_mut() = Some(outcome);

        // 捕获模式直接退事件循环，绕过了 exit()，所以这里要自己把进度交出去，
        // 否则服务端收不到 Stop，续播点永远是 0（实测）。退出路径复用 `end_session`
        // （与正常退出同一套日志文案与补发逻辑），不再在这里手写一份。
        self.end_session(SessionEnd::Exit);

        self.request_redraw();
        if let Err(error) = slint::quit_event_loop() {
            tracing::debug!(?error, "退出事件循环失败");
        }
    }

    fn ensure_open(&mut self) {
        let runtime = self.runtime.clone();
        let result = self.host.borrow_mut().try_open(&runtime);
        match result {
            Some(Ok(())) => {
                self.error = None;
                self.dirty = true;
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "engine.open 失败");
                self.fail(error.hint());
            }
            None => {}
        }
    }

    fn pump_events(&mut self) {
        let events = self.host.borrow_mut().drain_events();
        // 本次 open 的请求还没下发时，事件流里的一切都属于**上一次**会话：
        // 退出播放页只 `pause` 引擎（不 unload），mpv 里还挂着上一部片子，属性观察、
        // 帧回调、缓冲翻转都照跑，排队等 `try_open` 的这段时间（取直链的网络往返）里
        // 它们仍在往通道里落。把它们写进刚 `begin()` 的新会话，就是把上一部的位置 /
        // 轨道 / 首帧当成新会话的（体检 R5-1：缓冲期读数 = 上一部的续播点）。
        if self.request.is_none() {
            if !events.is_empty() {
                tracing::trace!(events = events.len(), "丢弃属于上一次会话的引擎事件");
            }
            return;
        }
        for event in events {
            match &event {
                EngineEvent::TracksChanged(tracks) => {
                    let tracks = tracks.clone();
                    self.session.handle(event);
                    self.tracks = tracks;
                    self.reapply_tracks();
                    self.dirty = true;
                }
                EngineEvent::FirstFrame => {
                    self.session.handle(event);
                    // 真出画了才算重连成功：清空退避与"重连中"提示。
                    self.auto_retry = 0;
                    self.retry_at = None;
                    self.error = None;
                    self.dirty = true;
                }
                EngineEvent::Error(error) => {
                    tracing::warn!(%error, "引擎错误");
                    let retryable = error.is_retryable();
                    self.session.handle(event);
                    if retryable {
                        self.schedule_reconnect();
                    }
                    self.dirty = true;
                }
                EngineEvent::Ended => {
                    // 已经停在 `Ended` 时又收到一次播完（引擎重复上报 / 两条信号都到）：
                    // 不再顺手报一次 `Pause`——位置已经被钉在时长，多报一条只会给上报
                    // 队列多塞一条同位置的记录，上报文案也从「播完」变成「暂停」。
                    let already_ended = self.session.state() == SessionState::Ended;
                    self.session.handle(event);
                    if !already_ended {
                        self.report(ProgressTrigger::Pause);
                    }
                    self.dirty = true;
                }
                _ => self.session.handle(event),
            }
        }
    }

    /// `TracksChanged` 之后重放一次选择（引擎 seek / 重连会把轨道重置）。
    fn reapply_tracks(&mut self) {
        let plan = self.selector.reapply(&self.tracks, &self.prefs);
        if plan.audio != self.selected_audio {
            let runtime = self.runtime.clone();
            if let Some(index) = plan.audio {
                self.host.borrow_mut().select_audio(&runtime, index);
            }
            self.selected_audio = plan.audio;
        }
        if plan.subtitle != self.selected_subtitle {
            let runtime = self.runtime.clone();
            self.host
                .borrow_mut()
                .select_subtitle(&runtime, plan.subtitle.clone());
            self.selected_subtitle = plan.subtitle;
        }
    }

    fn apply_plan_locally(&mut self, plan: &TrackPlan) {
        self.selected_audio = plan
            .audio
            .or_else(|| self.tracks.audio().next().map(|stream| stream.index));
        self.selected_subtitle = plan.subtitle.clone();
    }

    // ---- 界面回调 ----

    /// 播放 / 暂停（`playing` 是用户想要的最终状态）。
    pub(crate) fn set_playing(&mut self, playing: bool) {
        // 播完（`Ended`）再按播放：重播，而且是新会话 + 从 0 开始。
        //
        // 界面在播完后本来就会走起播路径（`player_finished` → `play_current`），
        // 这里兼作防御：界面视图晚一拍（节流推送）时，`set_playing(true)` 也不能
        // 落到“引擎停在 EOF、只收到一个 play”的死局。
        if playing && self.session.state() == SessionState::Ended {
            self.replay();
            return;
        }
        let runtime = self.runtime.clone();
        if playing {
            if self.session.play() {
                self.host.borrow_mut().play(&runtime);
            }
        } else {
            if self.session.pause() {
                self.host.borrow_mut().pause(&runtime);
            }
            self.report(ProgressTrigger::Pause);
        }
        self.dirty = true;
    }

    /// seek（只有进度条松手才会到这里）。
    pub(crate) fn seek(&mut self, position: Duration) {
        // 播完后的拖动：会话会从 `Ended` 转回 `Playing`（体检 R8-B3）。
        let resumed_from_end = self.session.state() == SessionState::Ended;
        if self.session.seek(position) {
            let runtime = self.runtime.clone();
            self.host.borrow_mut().seek(&runtime, position);
            if resumed_from_end {
                // 引擎还停在 EOF（`keep-open` 在那里暂停），不补一次 play 的话
                // 进度条动了、画面仍是最后一帧。
                self.host.borrow_mut().play(&runtime);
            }
            self.report(ProgressTrigger::Seek);
        }
        self.dirty = true;
    }

    /// 音量。
    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        let runtime = self.runtime.clone();
        self.host.borrow_mut().set_volume(&runtime, self.volume);
        self.dirty = true;
    }

    /// 静音。
    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        let runtime = self.runtime.clone();
        self.host.borrow_mut().set_muted(&runtime, muted);
        self.dirty = true;
    }

    /// 字幕延迟（秒，可为负）。`viewmodel` 侧记一份，界面菜单显示当前值。
    pub(crate) fn set_subtitle_delay(&mut self, seconds: f64) {
        let seconds = if seconds.is_finite() { seconds } else { 0.0 };
        self.subtitle_delay = seconds;
        let runtime = self.runtime.clone();
        self.host.borrow_mut().set_subtitle_delay(&runtime, seconds);
        self.dirty = true;
    }

    /// 字幕安全区：控制层可见时把字幕往上抬，隐藏时归零（只在数值变化时下发）。
    ///
    /// 界面（`nova-ui`）传来的是**逻辑像素**：控制块顶边到窗口底边的距离 + theme 的
    /// 安全间距（`PlayerOverlay.subtitle-safe-inset`，由控制层真实布局算出），不是
    /// 固定比例的窗口高度——矮窗口（800×600）下控制块占窗口约 42%，固定系数盖不住。
    ///
    /// 引擎要的是百分比（mpv `sub-pos`），换算在 [`subtitle_lift_percent`] 里：
    /// 窗口尺寸必须是**逻辑像素**（与传入的 px 同一坐标系），且要去掉 letterbox 的
    /// 下黑边——`sub-pos` 的百分比是相对**画面**高度，不是窗口高度。
    pub(crate) fn set_subtitle_margin(&mut self, safe_inset_px: i64) {
        let window = self.window_logical_size();
        let video = self.host.borrow().video_size();
        let lift = subtitle_lift_percent(safe_inset_px, window, video);
        if self.subtitle_margin == lift {
            return;
        }
        self.subtitle_margin = lift;
        let runtime = self.runtime.clone();
        self.host.borrow_mut().set_subtitle_margin(&runtime, lift);
    }

    /// 窗口逻辑尺寸；读不到时返回 `(0, 0)`（调用方按“不抬”处理）。
    fn window_logical_size(&self) -> (f64, f64) {
        let window = self.app.ui().window();
        let scale = f64::from(window.scale_factor());
        let size = window.size();
        if scale > 0.0 {
            (
                f64::from(size.width) / scale,
                f64::from(size.height) / scale,
            )
        } else {
            (0.0, 0.0)
        }
    }

    /// 倍速。
    pub(crate) fn set_rate(&mut self, rate: f32) {
        if rate.is_finite() && rate > 0.0 {
            self.rate = rate;
            let runtime = self.runtime.clone();
            self.host.borrow_mut().set_rate(&runtime, rate);
        }
        self.dirty = true;
    }

    /// 选音轨：写回 `TrackSelector` 与用户偏好，再交给引擎（不重启播放）。
    pub(crate) fn select_audio(&mut self, index: u32) {
        self.selector.select_audio(index);
        self.selected_audio = Some(index);
        let runtime = self.runtime.clone();
        self.host.borrow_mut().select_audio(&runtime, index);
        if let Some(stream) = self.tracks.audio_at(index) {
            let language = stream.language.clone();
            self.remember_language(true, &language);
        }
        self.report(ProgressTrigger::TrackChange);
        self.dirty = true;
    }

    /// 选字幕：`None` = 关闭。
    pub(crate) fn select_subtitle(&mut self, index: Option<u32>) {
        let selection = match index {
            Some(index) => SubtitleSelection::Track(index),
            None => SubtitleSelection::Off,
        };
        self.selector.select_subtitle(selection.clone());
        self.selected_subtitle = selection.clone();
        let runtime = self.runtime.clone();
        self.host.borrow_mut().select_subtitle(&runtime, selection);
        if index.is_none() {
            self.remember_subtitle_off();
        } else if let Some(stream) = index.and_then(|index| self.tracks.subtitle_at(index)) {
            let language = stream.language.clone();
            self.remember_language(false, &language);
        }
        self.report(ProgressTrigger::TrackChange);
        self.dirty = true;
    }

    /// 安排一次自动重连（指数退避，上限 [`MAX_AUTO_RETRY`] 次）。
    fn schedule_reconnect(&mut self) {
        if self.auto_retry >= MAX_AUTO_RETRY {
            // 次数用尽：交给用户决定（错误卡上的"重试"按钮会重置计数）。
            self.error =
                Some("连接中断，已重试多次仍失败。检查服务器是否在运行，或稍后再试。".to_owned());
            return;
        }
        self.auto_retry += 1;
        let backoff_ms = 500u64.saturating_mul(1 << (self.auto_retry - 1).min(4));
        self.retry_at =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(backoff_ms));
        // 重连期间显示"缓冲中"，而不是错误卡：用户不需要为一次网络抖动做决定。
        self.error = None;
        self.dirty = true;
        tracing::info!(
            attempt = self.auto_retry,
            backoff_ms,
            "连接中断，准备自动重连"
        );
    }

    /// 到点就重连。
    fn reconnect_if_due(&mut self) {
        let Some(at) = self.retry_at else {
            return;
        };
        if std::time::Instant::now() < at {
            return;
        }
        self.retry_at = None;
        self.retry();
    }

    /// 重试：`session.retry()` 拿到位置，再按同一请求重新 open。
    pub(crate) fn retry(&mut self) {
        let Some(request) = self.request.clone() else {
            // 还没拿到请求：按最后一次起播目标从头再来一遍准备流程。
            let (Some(post), Some(start)) = (self.post.clone(), self.last_start.clone()) else {
                tracing::warn!(
                    has_post = self.post.is_some(),
                    has_start = self.last_start.is_some(),
                    "重试时既没有播放请求也没有可重跑的起播目标，忽略"
                );
                return;
            };
            self.begin(
                start.item_id.as_str(),
                start.source_id.clone(),
                // 偏好传 `None`（与 `replay` 一致）：读数据库里的当前值，
                // 不用起播那一刻的快照。
                None,
                &post,
            );
            return;
        };
        let position = self.session.retry();
        self.error = None;
        self.auto_retry = 0;
        self.retry_at = None;
        self.host
            .borrow_mut()
            .enqueue_open(request.with_resume_at(position));
        self.dirty = true;
    }

    /// 播完之后从头重播（同一部片，新会话）。
    ///
    /// 走 [`Player::begin`]：会话代号递增、旧会话交出 `Exit`（服务端收到 `stop` 与
    /// “已看完”）、重新取一次直链，并且由 `begin` 把起播位置钉在 0。不是“慢不慢”
    /// 的问题——同一部片重播就是**新会话**，`session_seq` / 上报队列 / `Started`
    /// 全按这条规则写的（见 `Player::session_seq` 的注释）。
    ///
    /// 不靠 `Session::play`：状态机能表达“回到开头继续”，但引擎还停在 EOF
    /// （mpv `keep-open`），只发 `play` 不会重新出画。
    fn replay(&mut self) {
        let (Some(post), Some(target)) = (
            self.post.clone(),
            self.target.clone().or_else(|| self.pending.clone()),
        ) else {
            tracing::warn!(
                has_post = self.post.is_some(),
                has_target = self.target.is_some() || self.pending.is_some(),
                "重播时缺少取直链入口或当前目标，忽略"
            );
            return;
        };
        let source = Some(target.source_id.as_str().to_owned());
        self.begin(target.item_id.as_str(), source, None, &post);
    }

    /// 退出播放页：先提交一次进度并尽力补发，再停掉引擎与计时器。
    ///
    /// `_ui_position` 是界面自己的读数，**不再使用**：旧实现拿它当
    /// `session.position()` 为 0 时的兜底，结果是两边都可能被污染（界面读数基于节流
    /// 推送的视图，缓冲期它就是上一部片子的位置或 0）。现在只提交
    /// `Session::reportable_position()`：本次打开还没出过画面就一条也不发。
    pub(crate) fn exit(&mut self, _ui_position: Duration) {
        self.end_session(SessionEnd::Exit);
        // 会话级的错误也属于这次播放：退出播放页必须一并收尾。只 `session.reset()`
        // 不够——起播早退（没有服务器 / 条目不在缓存）走的是 `fail()` 设置的
        // `self.error`，不清理就会一直被调试通道读成「当前错误」，进另一个条目的
        // 详情页也还在（体检 R4-2）。
        self.error = None;
        self.auto_retry = 0;
        self.retry_at = None;
        // 轨道与选择同样是会话状态：不清的话 `fill_view` 会从 `self.tracks`
        // 重新填出上一部片子的音轨 / 字幕列表。
        self.selector.clear();
        self.tracks = TrackSet::default();
        self.selected_audio = None;
        self.selected_subtitle = SubtitleSelection::Off;
        self.capture = None;
        self.request = None;
        self.target = None;
        self.pending = None;
        // 起播目标也属于这次播放：不清的话，退出播放页后错误卡（或调试通道）
        // 上的「重试」会把已经离开的条目重新拉起来。
        self.last_start = None;
        // 界面视图也复位：标题 / 进度 / 时长 / 轨道 / 错误都随会话结束。
        // 音量与静音是用户偏好，`reset_session` 会保留。
        let mut view = self
            .pending_view
            .take()
            .unwrap_or_else(|| self.app.player_view());
        view.reset_session();
        self.pending_view = Some(view);
        self.dirty = true;
    }

    /// 结束当前会话：停引擎、提交一次 `Exit` 进度（服务端据此把续播点落盘并收到
    /// `stop`），然后按 [`SessionEnd`] 决定要不要有界地等一次补发。
    ///
    /// 退出播放页（[`Player::exit`]）与换片（[`Player::begin`] 里已有会话）都走这里。
    /// 换片以前只 `session.reset()`：旧会话最后一段进度既没提交，服务端也收不到 `stop`。
    fn end_session(&mut self, how: SessionEnd) {
        // **顺序**：先停引擎再提交进度。
        //
        // 以前是「提交 → 有界等待补发 → pause」：换片时界面线程被 `flush()` 的
        // `block_on`（上限 1500ms）卡住，这段时间旧片的声音还在放、画面还在动。
        let runtime = self.runtime.clone();
        self.host.borrow_mut().pause(&runtime);

        // `submitted` 才是"本次退出有没有把进度交给上报队列"；下面的 `flushed` 只是
        // 补发队列（之前没发出去的）这次发出去了几条。旧日志把补发条数写成 `sent`，
        // 正常（无需补发）时永远是 0，被误读成"退出时没有上报进度"。
        let submitted = if let Some(progress) = self.build_progress() {
            // `Exit` 交给 worker 就是 fire-and-forget：worker 收到后会先 `stop`
            // 再自己补发积压的队列（`ProgressReporter::submit` 对 `Exit` 会 drain），
            // 所以换片路径不需要界面线程在这里等。
            self.progress.submit(ProgressTrigger::Exit, progress)
        } else {
            false
        };
        match how {
            // 离开播放页：这一次**有界**等待（`FLUSH_TIMEOUT`）能把补发文件落盘，
            // 用户在等页面退出，卡一下可以接受。
            SessionEnd::Exit => {
                if let Some(outcome) = self.progress.flush() {
                    tracing::info!(
                        submitted,
                        flushed = outcome.sent,
                        failed = outcome.failed,
                        queued = outcome.queued,
                        "结束播放会话：补发进度"
                    );
                }
            }
            // 换片：界面线程一秒都不能停——用户在等着看新片。
            SessionEnd::Switch => {
                tracing::debug!(
                    submitted,
                    "结束播放会话：进度已交给上报 worker（换片不等待补发）"
                );
            }
        }
        self.session.reset();
    }

    /// 进程退出前的收尾（落盘补发文件）。
    pub(crate) fn shutdown(&mut self) {
        if let Some(progress) = self.build_progress() {
            self.progress.submit(ProgressTrigger::Exit, progress);
        }
        self.progress.shutdown();
    }

    // ---- 内部 ----

    /// 当前 provider：起播与起播前刷新用它（测试可注入假 provider）。
    fn provider(&self) -> Option<Arc<dyn nova_provider::MediaProvider>> {
        #[cfg(test)]
        if let Some(provider) = &self.provider {
            return Some(Arc::clone(provider));
        }
        self.local.provider_dyn()
    }

    fn fail(&mut self, message: &str) {
        self.error = Some(message.to_owned());
        self.session.fail(PlaybackError::engine(message));
        self.dirty = true;
        // 立刻把错误态排进待提交视图：早退路径（没有服务器 / 条目不在缓存）
        // 不走 `tick`，不能等下一拍才把错误卡显示出来。
        self.push_view(true);
    }

    /// 起播前刷新条目与资源（best effort，400ms 上限）。
    ///
    /// 为什么必须做：进度是**跨进程、跨设备**的状态，服务端才是事实来源。只读本地缓存会出现
    /// "上次播到 45s，这次从 0 开始"，以及"服务端已经标记看完，本地还显示继续观看"。
    ///
    /// 第三个返回值是**本会话**的 `PlaySessionId`：它和资源出自**同一次**
    /// `PlaybackInfo`（`MediaProvider::sources_with_session`），所以中间插不进别的
    /// `PlaybackInfo`——旧实现是「先 `sources()`、再查"最近一次"」，播放期间的详情页
    /// 刷新恰好夹在两次调用之间时，本会话就会拿到别人的标识。
    /// 失败 / 超时才没有标识：拿条目上的旧记录顶替会让本会话的 `stop` 带旧标识。
    fn refresh_before_play(
        &self,
        item: nova_core::MediaItem,
        sources: Vec<nova_core::MediaSource>,
    ) -> (
        nova_core::MediaItem,
        Vec<nova_core::MediaSource>,
        Option<String>,
    ) {
        let Some(provider) = self.provider() else {
            return (item, sources, None);
        };
        let id = item.id.clone();
        let refreshed = self.runtime.block_on(async {
            // 上限必须小：这段是**同步阻塞起播**的。实测在真实 WAN 上的 Emby，
            // 2 秒超时会把首帧推到 2.001s；400ms 足够覆盖局域网，WAN 超时就退回缓存。
            match tokio::time::timeout(std::time::Duration::from_millis(400), async {
                let fresh = provider.item(&id).await?;
                // 同一个响应里带回资源与会话标识：不再「先 `sources()`、再查最近一次」。
                let playback = provider.sources_with_session(&id).await?;
                Ok::<_, nova_provider::ProviderError>((
                    fresh,
                    playback.sources,
                    playback.play_session_id,
                ))
            })
            .await
            {
                Ok(Ok(pair)) => Some(pair),
                Ok(Err(error)) => {
                    tracing::debug!(%error, "起播前刷新失败，使用本地缓存");
                    None
                }
                Err(_) => {
                    tracing::debug!("起播前刷新超时，使用本地缓存");
                    None
                }
            }
        });
        match refreshed {
            Some((fresh, fresh_sources, session)) if !fresh_sources.is_empty() => {
                (fresh, fresh_sources, session)
            }
            Some((fresh, _, session)) => (fresh, sources, session),
            // 刷新失败 / 超时：拿不到本会话的标识。行为（见 `docs/ui-architecture.md`）：
            // 本会话的进度 `play_session_id` 是 `None`，入队时 `sink::Progress::stamp`
            // 退回 provider 的"条目最近一次"映射——那可能是上一次会话的旧标识，
            // 预期服务端把它当作无效会话丢弃（续播点这次不准；推测，未在服务端验证）。
            // 直链（`stream_target`）只按已有资源拼地址、不再取 `PlaybackInfo`，所以
            // 后续拿到直链也补不回来；要补只能再取一次 `PlaybackInfo`，而服务端会给
            // 一个**新**会话标识，补记反而多出一个会话，不做。
            None => (item, sources, None),
        }
    }

    fn build_progress(&self) -> Option<PlaybackProgress> {
        // **只有本次打开真的出过画面之后的位置才可以上报**（见
        // `Session::reportable_position`）：缓冲期不提交任何东西，服务端已有的续播点
        // 既不会被写成上一部的位置，也不会被清成 0（体检 R5-1 / W5-2 实证）。
        let position = self.session.reportable_position()?;
        self.build_progress_at(position)
    }

    fn build_progress_at(&self, position: Duration) -> Option<PlaybackProgress> {
        let target = self.target.as_ref()?;
        let duration = self.session.duration().or(target.duration);
        let played = duration
            .map(|total| {
                total.saturating_sub(position) <= self.prefs.finished_threshold && !total.is_zero()
            })
            .unwrap_or(false);
        Some(PlaybackProgress {
            session_seq: self.session_seq,
            // 服务端会话标识（`PlaySessionId`）绑在**本会话**上：起播时（
            // `refresh_before_play`）就快照进 `Player`，这里原样带走。不能等到发送时
            // 按条目去查"最近一次"——同一部片播放期间详情页刷新、重播都会覆盖它。
            play_session_id: self.play_session_id.clone(),
            item_id: target.item_id.clone(),
            source_id: target.source_id.clone(),
            position,
            duration,
            played,
            paused: self.session.state() != SessionState::Playing,
            method: PlayMethod::DirectPlay,
        })
    }

    fn report(&mut self, trigger: ProgressTrigger) {
        if let Some(progress) = self.build_progress() {
            self.progress.submit(trigger, progress);
        }
    }

    /// 把用户刚选的轨道语言提到偏好列表最前面（下次自动选同一条）。
    fn remember_language(&mut self, audio: bool, language: &nova_core::Language) {
        if language.is_undetermined() {
            return;
        }
        let mut prefs = self.prefs.clone();
        let list = if audio {
            &mut prefs.audio_languages
        } else {
            &mut prefs.subtitle_languages
        };
        list.retain(|existing| !existing.matches(language));
        list.insert(0, language.clone());
        if !audio {
            prefs.subtitle_mode = nova_core::SubtitleMode::Always;
        }
        self.prefs = prefs.clone();
        self.save_preferences(prefs);
    }

    fn remember_subtitle_off(&mut self) {
        let mut prefs = self.prefs.clone();
        prefs.subtitle_mode = nova_core::SubtitleMode::Never;
        self.prefs = prefs.clone();
        self.save_preferences(prefs);
    }

    /// 写回偏好（失败只记日志：轨道已经切好了，存偏好失败不该影响播放）。
    fn save_preferences(&self, prefs: UserPreferences) {
        if let Err(error) = self.local.db.set_preferences(&prefs) {
            tracing::warn!(%error, "写回用户偏好失败");
        }
    }

    fn request_redraw(&self) {
        self.app.ui().window().request_redraw();
    }

    /// 把引擎状态写进 `PlayerView`。
    ///
    /// **不在这里调 `App::set_player`**：那个调用会同步触发 nova-ui 的播放页回调
    /// （例如字幕安全区），回调又要改播放器状态——如果此时 `Shell::player` 还被
    /// 借用着就会炸 `RefCell`。所以只排队，由 `Shell` 在放下借用之后统一提交。
    fn push_view(&mut self, force: bool) {
        if !force && !self.dirty && self.last_push.elapsed() < VIEW_INTERVAL {
            return;
        }
        self.last_push = Instant::now();
        self.dirty = false;

        let mut view = self
            .pending_view
            .take()
            .unwrap_or_else(|| self.app.player_view());
        self.fill_view(&mut view);
        self.pending_view = Some(view);
    }

    /// 取走待提交的视图（`Shell` 放下借用后调用 `App::set_player`）。
    pub(crate) fn take_pending_view(&mut self) -> Option<PlayerView> {
        self.pending_view.take()
    }

    /// 当前播放状态的一份**即时**视图（不走 `pending_view` 的节流）。
    ///
    /// 调试通道读状态必须用它：`UiState.player` 只有在 `set_player` 被调用时才更新，
    /// 而 `set_player` 受 `VIEW_INTERVAL` 节流，读它会把"正在播放"读成"暂停"——
    /// 我就因为这个误判过"播放器按钮点不动"。
    pub(crate) fn live_view(&mut self) -> PlayerView {
        // 用**尚未提交**的待提交视图做底：它比 `App` 里的上一拍视图新（`begin`
        // 刚复位完会话、还没轮到 `Shell` 提交时，调试通道读到的也必须是新会话）。
        let mut view = match self.pending_view.clone() {
            Some(view) => view,
            None => self.app.player_view(),
        };
        self.fill_view(&mut view);
        view
    }

    fn fill_view(&mut self, view: &mut PlayerView) {
        let state = self.session.state();
        view.position = self.session.position();
        view.duration = self.session.duration();
        // 显式的"播完"标志：时长未知时位置推不到时长，续播点贴片尾时位置又会
        // 在 Opening 首帧里就 >= 时长，两者都不能用 `position >= duration` 判定。
        view.ended = state == SessionState::Ended;
        view.buffering = state == SessionState::Buffering || self.session.show_spinner();
        view.playing = matches!(
            state,
            SessionState::Opening | SessionState::Playing | SessionState::Buffering
        );
        view.volume = self.volume;
        view.muted = self.muted;
        view.rate = self.rate;
        view.subtitle_delay = self.subtitle_delay as f32;
        if let Some(target) = &self.target {
            view.title = target.title.clone();
            view.subtitle = target.subtitle.clone();
        }

        let message = self
            .error
            .clone()
            .or_else(|| self.session.error().map(|error| error.hint().to_owned()));
        match message {
            Some(message) => {
                view.error = Some(message);
                view.error_action = Some("重试".to_owned());
                view.buffering = false;
                if !view.controls_visible {
                    let _ = view.set_controls_visible(true);
                }
            }
            None => {
                view.error = None;
                view.error_action = None;
            }
        }

        view.audio_tracks = view::audio_tracks(&self.tracks, self.selected_audio);
        view.subtitle_tracks = view::subtitle_tracks(&self.tracks, &self.selected_subtitle);

        let diagnostics = self.host.borrow().diagnostics().unwrap_or_default();
        view.hwdec_notice = diagnostics.hwdec_notice();
        let size = self.host.borrow().video_size();
        view.engine_line = Some(view::engine_line(&diagnostics, size));
        if let Some(image) = self.host.borrow().frame_image() {
            // `nova-ui` 的 `player_data` 从 `ImageStore` 取 `frame`，所以帧图走
            // `set_backdrop` 同一个入口（键与背景图一致，退出播放页后会被真背景覆盖）。
            if let Some(target) = &self.target {
                self.app.set_backdrop(target.item_id.as_str(), image);
            }
        }
    }
}

/// 字幕上抬量：窗口逻辑像素 → **画面**高度的百分比（mpv `sub-pos` 的坐标系）。
///
/// `safe_inset_px` 是从窗口底边到控制块顶边的距离 + 安全间距（逻辑像素）；
/// `window_logical` 是窗口逻辑尺寸；`video` 是画面像素尺寸（拿不到时按“画面铺满窗口”）。
///
/// 关键：mpv 的 `sub-pos` 百分比相对**画面**高度，不是窗口高度。letterbox 时画面
/// 比窗口矮、上下各有一条黑边，字幕原位在画面底边（= 窗口底部往上一条黑边），
/// 所以先减掉下方黑边，再除以画面高度。读不到窗口尺寸（ 0 / 非有限值）或没有
/// 需要抬起的距离时返回 0（= 不抬，回到 mpv 原位）：宁可不抬，也不能因为一个
/// 坏读数把字幕推到画面中间。
fn subtitle_lift_percent(
    safe_inset_px: i64,
    window_logical: (f64, f64),
    video: Option<(u32, u32)>,
) -> i64 {
    let (window_w, window_h) = window_logical;
    if safe_inset_px <= 0
        || !window_w.is_finite()
        || !window_h.is_finite()
        || window_w <= 0.0
        || window_h <= 0.0
    {
        return 0;
    }
    // 画面按 `contain` 缩放到窗口内（mpv 默认 `keepaspect`）：宽度受限时上下留黑边，
    // 高度受限时左右留黑边（后者不影响纵向换算）。没有画面尺寸时按无黑边处理。
    let (picture_h, letterbox_bottom) = match video {
        Some((video_w, video_h)) if video_w > 0 && video_h > 0 => {
            let picture_h = (window_w * f64::from(video_h) / f64::from(video_w)).min(window_h);
            (picture_h, (window_h - picture_h) / 2.0)
        }
        _ => (window_h, 0.0),
    };
    let lift_px = safe_inset_px as f64 - letterbox_bottom;
    if lift_px <= 0.0 {
        return 0;
    }
    let percent = (lift_px * 100.0 / picture_h).round();
    percent.clamp(0.0, 90.0) as i64
}

/// 把 provider 的错误翻译成可行动的中文（不含 URL / 令牌）。
fn player_error_text(error: &nova_provider::ProviderError) -> String {
    match error {
        // 本机没有令牌：请求根本没发出去，说“未登录”而不是“状态已失效”。
        nova_provider::ProviderError::NoToken => "尚未登录，无法获取播放地址".to_owned(),
        nova_provider::ProviderError::Unauthorized => "登录状态已失效，请重新登录服务器".to_owned(),
        nova_provider::ProviderError::NotFound => {
            "服务器上找不到这个文件，可能已被移动或删除".to_owned()
        }
        nova_provider::ProviderError::Network(_) => "服务器不可达，检查网络或服务器状态".to_owned(),
        _ => "无法从服务器取得播放地址，稍后重试".to_owned(),
    }
}

/// 选一个可播放版本。
fn pick_source(
    sources: &[nova_core::MediaSource],
    wanted: Option<&str>,
) -> Option<nova_core::MediaSource> {
    if let Some(wanted) = wanted {
        if let Some(found) = sources.iter().find(|source| source.id.as_str() == wanted) {
            return Some(found.clone());
        }
    }
    sources.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::{MediaSource, SourceId};
    use nova_playback::{ProgressSink, ReportError};

    fn source(id: &str) -> MediaSource {
        MediaSource {
            id: SourceId::new(id),
            item_id: ItemId::new("item"),
            name: None,
            container: None,
            size: None,
            bitrate: None,
            runtime: Some(Duration::from_secs(30)),
            streams: Vec::new(),
        }
    }

    /// 体检 R3-3：错误会话不能继承上一部片子的进度 / 时长 / 标题 / 倍速 / 字幕延迟。
    ///
    /// 用真实 `Player` + 无头 Slint 后端 + 一个没有任何条目的本地缓存：
    /// “本地缓存里没有这个条目”在**调用引擎之前**就早退，所以这条路径根本不需要 mpv。
    /// 上一会话的界面与状态直接按真实播放后的样子摆好（播放位置 / 时长 / 倍速 /
    /// 字幕延迟），再开始新会话，断言界面拿到的即时视图没有继承任何一项。
    #[test]
    fn failed_session_does_not_inherit_the_previous_sessions_progress() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&test_server_info(), 1).expect("写服务器");
        }
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        // 多线程 runtime：这个 `Local` 有服务器（也就有 provider），换片时
        // `end_session()` 会 `block_on` 等上报 worker 补发；current_thread runtime
        // 上那个 worker（以及超时定时器）永远轮不到，用例会挂死。
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = Rc::new(RefCell::new(Host::new(true)));
        let post: Post = Arc::new(|_| {});
        let mut player = Player::new(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
            host,
            None,
            Rc::new(RefCell::new(None)),
        );

        // 上一部（6489）：界面停在 8:37 / 1:31:23，倍速与字幕延迟也都留着。
        let previous = PlayerView::new("上一部")
            .with_duration(Duration::from_secs(5483))
            .with_position(Duration::from_secs(517))
            .with_buffered(Duration::from_secs(900))
            .with_playing(true)
            .with_rate(1.25)
            .with_audio_tracks(vec![nova_ui::TrackItem::new(1, "国语")]);
        app.set_player(&previous);
        player
            .session
            .begin(Duration::from_secs(517), Some(Duration::from_secs(5483)));
        player.rate = 1.25;
        player.subtitle_delay = 0.5;

        // 新会话：条目不在本地缓存 → 不碰引擎的失败早退。
        player.begin("999999999", None, None, &post);

        let view = player.live_view();
        assert!(view.error.is_some(), "应停在错误态：{:?}", view.error);
        assert_eq!(
            view.position,
            Duration::ZERO,
            "错误页不该显示上一部片子的进度"
        );
        assert_eq!(view.duration, None, "错误页不该显示上一部片子的时长");
        assert_eq!(view.buffered, None);
        assert_eq!(view.title, "", "错误页不该显示上一部片子的标题");
        assert!(view.audio_tracks.is_empty(), "音轨列表也不属于新会话");
        assert!(!view.playing);
        assert_eq!(view.rate, 1.0, "倍速随会话复位");
        assert_eq!(view.subtitle_delay, 0.0, "字幕延迟随会话复位");
    }

    type ResumeFixture = (
        tempfile::TempDir,
        Arc<Local>,
        Player,
        tokio::runtime::Runtime,
        Arc<std::sync::Mutex<Vec<Message>>>,
    );

    /// 起播前的本地准备：一个 `Played = true`、带 45:47 续播点的条目，
    /// `Local` 已打开（启动快照是默认偏好），返回可直接调用 `begin()` 的播放器。
    ///
    /// `with_token` 为 `false` 时**不写令牌**：起播会在 `stream_target` 拿到
    /// [`nova_provider::ProviderError::NoToken`]，`OpenReady` 带着错误回来。
    /// 无令牌的用例用另一个服务器标识，避免蹭到进程内共享凭证存储里别的用例写的令牌。
    fn resume_fixture(with_token: bool) -> ResumeFixture {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let server = nova_core::ServerInfo {
            id: nova_core::ServerId::new(if with_token {
                "srv-player-resume"
            } else {
                "srv-player-resume-nocreds"
            }),
            kind: nova_core::ServerKind::Jellyfin,
            name: "续播测试".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            version: None,
            user_id: None,
            auth: nova_core::AuthState::Unauthorized,
            metadata_language: nova_core::Language::undetermined(),
        };
        // provider 在**构造时**从凭证存储读一次令牌；进程内存储是整个进程共享的，
        // 所以这里写进去的令牌能被接下来 `Local::open` 构造的 provider 看到。
        if with_token {
            crate::env::open_credentials(crate::env::CredentialMode::Memory)
                .store
                .set(
                    &format!("jellyfin/{}/token", server.id.as_str()),
                    &nova_secrets::SecretString::new("test-token"),
                )
                .expect("写测试令牌");
        }
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server, 1).expect("写服务器");
            let item = nova_core::MediaItem {
                id: ItemId::new("item-1"),
                kind: nova_core::MediaKind::Movie,
                title: "6489".to_owned(),
                sort_title: String::new(),
                original_title: None,
                year: None,
                overview: None,
                genres: Vec::new(),
                runtime: Some(Duration::from_secs(5483)),
                parent_id: None,
                index_number: None,
                index_number_end: None,
                child_count: None,
                images: nova_core::ImageSet::default(),
                // 真实形态：服务端标记看完过，但仍保留 45:47 的续播点。
                user_data: nova_core::UserData {
                    played: true,
                    resume_position: Duration::from_secs(2747),
                    ..nova_core::UserData::default()
                },
            };
            db.upsert_items(&server.id, &[item.clone(), second_item()], 2)
                .expect("写条目");
            let mut source_1 = source("src-1");
            source_1.item_id = ItemId::new("item-1");
            source_1.runtime = Some(Duration::from_secs(5483));
            let mut second_source = source("src-2");
            second_source.item_id = ItemId::new("item-2");
            second_source.runtime = Some(Duration::from_secs(5483));
            db.upsert_sources(&server.id, &[source_1, second_source])
                .expect("写播放入口");
        }
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开本地状态"));
        assert!(
            local.provider_dyn().is_some(),
            "有服务器就该有 provider（令牌只影响直链，不该影响续播位置）"
        );
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        // 多线程 runtime：`begin()` 里用 `Handle::block_on` 做起播前刷新，
        // 而 `Handle::block_on` 驱动不了 current_thread runtime 的 IO / 定时器，
        // 那样 400ms 超时永远不会到点（测试会挂住，真机上是多线程 runtime）。
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = Rc::new(RefCell::new(Host::new(true)));
        let player = Player::new(
            app,
            Arc::clone(&local),
            runtime.handle().clone(),
            host,
            None,
            Rc::new(RefCell::new(None)),
        );
        let posted: Arc<std::sync::Mutex<Vec<Message>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        (dir, local, player, runtime, posted)
    }

    /// 同一份本地数据里的第二个条目：`Played = true`，续播点在 30:00。
    ///
    /// 用来验“播完 A 之后起播 B”不能被当成重播 A（否则试播位置会被钉成 0）。
    fn second_item() -> nova_core::MediaItem {
        nova_core::MediaItem {
            id: ItemId::new("item-2"),
            kind: nova_core::MediaKind::Movie,
            title: "第二部".to_owned(),
            sort_title: String::new(),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime: Some(Duration::from_secs(5483)),
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: nova_core::ImageSet::default(),
            user_data: nova_core::UserData {
                played: true,
                resume_position: Duration::from_secs(1800),
                ..nova_core::UserData::default()
            },
        }
    }

    fn post_to(posted: &Arc<std::sync::Mutex<Vec<Message>>>) -> Post {
        let sink = Arc::clone(posted);
        Arc::new(move |message| match sink.lock() {
            Ok(mut queue) => queue.push(message),
            Err(poisoned) => poisoned.into_inner().push(message),
        })
    }

    /// 轮询等待 `begin()` 投递 `OpenReady`（上限 2s）。
    ///
    /// 固定 sleep 50ms 在慢 CI 上不稳定：机器一忙，tokio 任务还没跑完就断言，
    /// 测试会偶发红灯。轮询有结果就立刻返回，超时才失败。
    ///
    /// 引擎返回 `Err` 时**立刻**失败并把错误打出来：以前只认 `Ok`，取直链失败会被
    /// 2 秒超时掩盖成“没等到 OpenReady”，真正的失败原因反而看不到。
    ///
    /// 错误用 `{:?}` 打印：带上错误**类型**（`Provider(NoToken)` / `Provider(Unauthorized)`
    /// …），而不是界面文案——文案会随产品调整，不能钉在断言里。
    fn wait_for_resume_at(posted: &Arc<std::sync::Mutex<Vec<Message>>>) -> Duration {
        match wait_for_open(posted) {
            Ok(request) => request.resume_at,
            Err(failure) => panic!("引擎返回错误（begin() 失败）：{failure:?}"),
        }
    }

    /// 轮询等待 `begin()` 投递 `OpenReady`（上限 2s），把结果原样交出去。
    ///
    /// 取走队列里的第一条消息（`OpenFailure` 不可 `Clone`：它保留 provider 的错误，
    /// 而 `ProviderError` 不是 `Copy`）。
    fn wait_for_open(
        posted: &Arc<std::sync::Mutex<Vec<Message>>>,
    ) -> Result<Box<OpenRequest>, OpenFailure> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            {
                let mut queue = match posted.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if !queue.is_empty() {
                    let message = queue.remove(0);
                    let Message::OpenReady { request, .. } = message;
                    return request;
                }
            }
            assert!(
                Instant::now() < deadline,
                "2 秒内没有等到 OpenReady：begin() 没有把请求交给引擎"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 体检 R7-D2：`played` 且有中段续播点时，`begin()` 交给引擎的
    /// `OpenRequest.resume_at` 不能是 0。
    ///
    /// 走真实 `begin()` 路径（本地缓存 → 起播前刷新 → `stream_target` → 投递
    /// `OpenReady`）。令牌只为了让 `stream_target` 拼出直链（它不发请求）；
    /// `base_url` 指向 `127.0.0.1:1`，起播前刷新会立刻失败并回退到缓存——这正是
    /// “起播不被网络卡住”的设计。
    #[test]
    fn begin_resumes_a_played_item_with_a_mid_resume_point() {
        let (_dir, _local, mut player, _runtime, posted) = resume_fixture(true);
        let post = post_to(&posted);
        player.begin("item-1", None, None, &post);
        let resume_at = wait_for_resume_at(&posted);
        assert_eq!(
            resume_at,
            Duration::from_secs(2747),
            "played 不代表从头播：交给引擎的 OpenRequest 必须续在 45:47"
        );
        assert_eq!(
            player.session.position(),
            Duration::from_secs(2747),
            "会话也不能把续播位置夹成 0（版本时长会参与 clamp）"
        );
    }

    /// 播完 A 之后起播 B：B 必须用它**自己的**续播点，不能被当成“重播 A”而钉在 0。
    ///
    /// 回归保险：[`Player::begin`] 里的 `replay` 判定看的是“会话是 `Ended` 且目标
    /// 条目相同”。只拿“会话是 Ended”当事会当场把 B 从头开。
    #[test]
    fn ended_session_starting_another_item_resumes_at_its_own_point() {
        let (_dir, _local, mut player, _runtime, posted) = resume_fixture(true);
        let post = post_to(&posted);

        // 先把 item-1 放到 `Ended`：`target` / `pending` 都指向 item-1。
        player.begin("item-1", None, None, &post);
        let request = match wait_for_open(&posted) {
            Ok(request) => request,
            Err(failure) => panic!("第一次起播就该拿到直链：{failure:?}"),
        };
        player.on_open_ready(player.generation, Ok(request));
        player.session.handle(EngineEvent::Ended);
        assert_eq!(
            player.session.state(),
            SessionState::Ended,
            "用例前提：会话停在片尾"
        );

        // 换一部片：同一会话状态、不同条目 → 照旧从它的续播点（30:00）起播。
        player.begin("item-2", None, None, &post);
        assert_eq!(
            wait_for_resume_at(&posted),
            Duration::from_secs(1800),
            "播完 A 后起播 B 不是重播：B 必须从自己的续播点开始"
        );
    }

    /// 回归（复审 🟡3）：重播取直链失败后，错误卡上的「重试」必须重跑同一条目。
    ///
    /// 旧实现只在 `request` 有值时才重开引擎；取直链失败的路径把
    /// `request` / `pending` / `target` 全清空，`retry` 于是静默 return——用户
    /// 看到的就是“点了没反应”（`Failed` 下 toggle 也什么都发不出去）。
    /// 现在靠 [`StartPoint`] 快照重跑 `begin`。
    #[test]
    fn retry_after_a_failed_replay_starts_the_same_item_again() {
        // 无令牌：`stream_target` 必失败，验证的就是“取直链失败”这条路径。
        let (_dir, _local, mut player, _runtime, posted) = resume_fixture(false);
        let post = post_to(&posted);

        player.begin("item-1", None, None, &post);
        let failure = match wait_for_open(&posted) {
            Ok(_) => panic!("没有令牌时不该拿到直链"),
            Err(failure) => failure,
        };
        player.on_open_ready(player.generation, Err(failure));
        assert!(player.error.is_some(), "应停在错误卡：{:?}", player.error);
        assert!(
            player.request.is_none() && player.pending.is_none() && player.target.is_none(),
            "用例前提：取直链失败后三个字段都空（旧实现就是在这里静默 return 的）"
        );

        player.retry();

        // "重试"必须真的又走了一遍起播：队列里出现第二个 `OpenReady`（依旧失败）。
        assert!(
            wait_for_open(&posted).is_err(),
            "没有令牌时重试也不该成功，但它必须把新的 OpenReady 投递回来"
        );
        assert_eq!(
            player
                .pending
                .as_ref()
                .map(|target| target.item_id.as_str()),
            Some("item-1"),
            "重试必须重新起播同一条目"
        );
    }

    /// 取直链失败时 `wait_for_resume_at` 必须**立刻**把引擎错误抛出来。
    ///
    /// 以前它只认 `OpenReady` 里的 `Ok`，`Err` 会被 2 秒超时掩盖成
    /// “没等到 OpenReady”（测试还得多等 2 秒），真正的失败原因（这里是没有令牌）
    /// 在断言里完全看不到。
    #[test]
    fn engine_error_fails_the_wait_immediately() {
        let (_dir, _local, mut player, _runtime, posted) = resume_fixture(false);
        let post = post_to(&posted);
        player.begin("item-1", None, None, &post);

        // 不能等完 2 秒超时：错误必须立刻回来。
        let deadline = Instant::now() + Duration::from_secs(1);
        let failure = loop {
            {
                // 锁必须在 sleep 之前放掉：拿着锁轮询会把后台线程（它要 push 消息）
                // 饿死，永远等不到结果。
                let mut queue = match posted.lock() {
                    Ok(queue) => queue,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if !queue.is_empty() {
                    let message = queue.remove(0);
                    let Message::OpenReady { request, .. } = message;
                    break request.expect_err("没有令牌时取直链必须失败");
                }
            }
            assert!(
                Instant::now() < deadline,
                "取直链失败也必须立刻回到界面上，不能等 2 秒超时"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        // 按错误**类型**判定（本机没有令牌），不匹配界面文案。
        assert!(
            matches!(
                failure,
                OpenFailure::Provider(nova_provider::ProviderError::NoToken)
            ),
            "失败原因必须保留 provider 的错误码：{failure:?}"
        );
    }

    /// 起播与详情页必须读同一个偏好来源：详情每次读 `db.preferences()`，
    /// 起播以前用的是 `Local::open` 时的启动快照。运行中把 `resume_min_position`
    /// 调高后，旧实现会出现“详情写播放、起播却续在 45:47”。
    #[test]
    fn begin_reads_preferences_from_the_database_not_the_startup_snapshot() {
        let (_dir, local, mut player, _runtime, posted) = resume_fixture(true);
        // 启动快照是默认 30s；改成“续播点至少 3000s 才算”后，2747s 不该再续播。
        let mut prefs = local.preferences.clone();
        prefs.resume_min_position = Duration::from_secs(3000);
        local.db.set_preferences(&prefs).expect("写偏好");

        // 详情页读 DB：同一份偏好下文案必须是「播放」。
        let detail = local.detail("item-1").expect("读详情").expect("条目存在");
        assert_eq!(detail.play_label, "播放");

        let post = post_to(&posted);
        player.begin("item-1", None, None, &post);
        let resume_at = wait_for_resume_at(&posted);
        assert_eq!(
            resume_at,
            Duration::ZERO,
            "起播必须按数据库里的当前偏好判定（2747s < 3000s 不续播）"
        );
    }

    /// 体检 R4-2：退出播放页必须把会话级错误一起收尾。
    ///
    /// 以前 `exit()` 只 `session.reset()`，`self.error`（起播早退 / 引擎错误）与轨道
    /// 选择都留在播放器里；调试通道在另一个条目的详情页仍能读到上一会话的
    /// `view.player.error`，回放菜单也会带着上一部的轨道。
    #[test]
    fn exit_clears_the_previous_sessions_error_and_session_state() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        // 不建服务器：这条用例只验证 `exit()` 的会话收尾，不需要 provider
        // （有 provider 时 `Progress::flush()` 会走阻塞补发，那是生产路径，不是这里要测的）。
        let local = Arc::new(Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = Rc::new(RefCell::new(Host::new(true)));
        let mut player = Player::new(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
            host,
            None,
            Rc::new(RefCell::new(None)),
        );

        // 上一会话：界面停在 8:37 / 1:31:23，引擎早退错误 + 看门狗错误都在，
        // 音轨 / 字幕选择也留着。
        app.set_player(
            &PlayerView::new("上一部")
                .with_duration(Duration::from_secs(5483))
                .with_position(Duration::from_secs(517))
                .with_playing(true)
                .with_error("连续 15 秒没有收到画面…", "重试")
                .with_audio_tracks(vec![nova_ui::TrackItem::new(1, "国语").with_selected(true)]),
        );
        player.error = Some("连续 15 秒没有收到画面…".to_owned());
        player
            .session
            .begin(Duration::from_secs(517), Some(Duration::from_secs(5483)));
        player.session.fail(PlaybackError::Stalled);
        player.tracks = TrackSet::new(vec![nova_core::MediaStream::audio(
            1,
            "eac3",
            nova_core::AudioDetails {
                channels: Some(2),
                sample_rate: Some(48_000),
                bitrate: None,
                profile: None,
            },
            nova_core::Language::new("chi"),
        )]);
        player.selected_audio = Some(1);

        player.exit(Duration::from_secs(517));

        let view = player.live_view();
        assert!(
            view.error.is_none(),
            "退出播放页后不能再报上一会话的错误：{:?}",
            view.error
        );
        assert_eq!(view.title, "", "标题属于上一会话");
        assert_eq!(view.position, Duration::ZERO);
        assert_eq!(view.duration, None);
        assert!(!view.playing);
        assert!(view.audio_tracks.is_empty(), "轨道列表属于上一会话");
        assert!(view.subtitle_tracks.is_empty());
    }

    /// 健康会话切换到新片子：在首帧到来之前，视图也不能闪现上一部片子的标题 /
    /// 进度 / 时长 / 音轨 / 倍速（体检 R3-3 的同类残留）。
    #[test]
    fn a_new_session_starts_from_the_new_item_not_the_previous_one() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let server = test_server_info();
        let server_id = server.id.clone();
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server, 1).expect("写服务器");
            let item = nova_core::MediaItem {
                id: ItemId::new("b"),
                kind: nova_core::MediaKind::Movie,
                title: "新片 B".to_owned(),
                sort_title: "新片 B".to_owned(),
                original_title: None,
                year: Some(2025),
                overview: None,
                genres: Vec::new(),
                runtime: Some(Duration::from_secs(6917)),
                parent_id: None,
                index_number: None,
                index_number_end: None,
                child_count: None,
                images: nova_core::ImageSet::default(),
                user_data: nova_core::UserData::default(),
            };
            db.upsert_items(&server_id, &[item], 2).expect("写条目");
            let source = nova_core::MediaSource {
                id: nova_core::SourceId::new("src-b"),
                item_id: ItemId::new("b"),
                name: None,
                container: None,
                size: None,
                bitrate: None,
                runtime: Some(Duration::from_secs(6917)),
                streams: Vec::new(),
            };
            db.upsert_sources(&server_id, &[source]).expect("写播放源");
        }
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = Rc::new(RefCell::new(Host::new(true)));
        let post: Post = Arc::new(|_| {});
        let mut player = Player::new(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
            host,
            None,
            Rc::new(RefCell::new(None)),
        );

        // 上一部 A 的界面残留（真实播放后 `Shell` 最后一次 `set_player` 的形状）。
        let previous = PlayerView::new("上一部 A")
            .with_duration(Duration::from_secs(5483))
            .with_position(Duration::from_secs(517))
            .with_playing(true)
            .with_rate(1.25)
            .with_audio_tracks(vec![nova_ui::TrackItem::new(1, "上一部的音轨")]);
        app.set_player(&previous);
        player.rate = 1.25;

        // 直接播放 B：首帧（引擎事件）还没到，先看界面看到了什么。
        player.begin("b", None, None, &post);
        let view = player.live_view();
        assert_eq!(view.title, "新片 B", "不能闪现上一部的标题");
        assert_eq!(
            view.duration,
            Some(Duration::from_secs(6917)),
            "时长必须是 B 的"
        );
        assert_eq!(
            view.position,
            Duration::ZERO,
            "B 没有续播点，不是 A 的 8:37"
        );
        assert_eq!(view.rate, 1.0, "倍速随会话复位");
        assert!(view.audio_tracks.is_empty(), "A 的音轨不能漏到 B");
        assert!(
            view.error.is_none(),
            "起播途中不该是错误态：{:?}",
            view.error
        );
        assert!(view.playing, "起播期间视为播放中（不是上一部的暂停态）");
    }

    /// 复审 🟡1/🟡2：`PlayerView::ended` 必须由 `SessionState::Ended` 直传，
    /// 不能靠 `position >= duration` 推。
    ///
    /// 两种旧实现判错的场景：续播点贴在片尾、还在 `Opening` 时位置已 >= 时长
    /// （旧逻辑误判成播完）；时长未知时 `Ended` 推不动位置（旧逻辑永远判 false）。
    #[test]
    fn ended_state_is_carried_to_the_player_view_without_guessing_from_position() {
        let (_dir, _local, mut player, _runtime) = switch_fixture();

        // 续播点贴着片尾、首帧还没到：位置 >= 时长，但不是播完。
        player
            .session
            .begin(Duration::from_secs(5400), Some(Duration::from_secs(5400)));
        let opening = player.live_view();
        assert!(
            !opening.ended,
            "Opening 中不能因为位置满了就标成播完：{opening:?}"
        );
        assert!(opening.playing, "Opening 算播放中");

        // 时长未知 + Ended：位置推不到时长，只能靠显式标志。
        player.session.begin(Duration::ZERO, None);
        player.session.handle(EngineEvent::Ended);
        let ended = player.live_view();
        assert!(ended.ended, "Ended 必须传到视图：{ended:?}");
        assert!(ended.duration.is_none(), "用例前提：时长未知");
        assert!(!ended.playing, "播完后不再算播放中");
    }

    /// 复审 💡：已经停在 `Ended` 时又收到一次 `Ended`，不能再补报一条 `Pause`。
    ///
    /// 重复的播完（引擎两条信号都到 / 重连后重发）会把位置再钉一次时长，并给上报
    /// 队列多塞一条“暂停”——上报层的指纹去重只是巧合地接下它，语义仍然是错的。
    #[test]
    fn duplicate_ended_does_not_report_again() {
        let (_dir, local, mut player, runtime) = switch_fixture();
        let recorder = Arc::new(RecordingSink::default());
        player.progress = sink::Progress::for_test(
            &local,
            runtime.handle(),
            Arc::clone(&recorder) as Arc<dyn ProgressSink>,
        );

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);
        player.target = player.pending.take();
        player.request = Some(OpenRequest::new("http://127.0.0.1:1/Videos/a/stream"));
        player.session.handle(EngineEvent::FirstFrame);
        player.session.advance_position(Duration::from_secs(600));

        player.host.borrow_mut().push_event(EngineEvent::Ended);
        player.pump_events();
        assert_eq!(player.session.state(), SessionState::Ended);
        assert_eq!(
            player.progress.submit_count(),
            1,
            "第一次播完要上报一条进度"
        );

        // 已经停在 Ended 时再来一次：位置不会变、状态不会变，也不该再上报。
        player.host.borrow_mut().push_event(EngineEvent::Ended);
        player.pump_events();
        assert_eq!(player.session.state(), SessionState::Ended);
        assert_eq!(
            player.progress.submit_count(),
            1,
            "重复的 Ended 不能再补报一条进度"
        );
    }

    /// 记录上报调用的假 sink（换片收尾必须能被观测到）。
    ///
    /// `stop` 只应发生在 `Exit` 那一条上报之后：它是服务端落盘续播点的唯一入口。
    #[derive(Default)]
    struct RecordingSink {
        reports: std::sync::Mutex<Vec<PlaybackProgress>>,
        stops: std::sync::Mutex<Vec<PlaybackProgress>>,
    }

    impl RecordingSink {
        fn reports(&self) -> Vec<(String, Duration)> {
            freeze(&self.reports)
        }

        fn stops(&self) -> Vec<(String, Duration)> {
            freeze(&self.stops)
        }

        /// `stop` 收到的（条目, 会话代号）：用来看旧会话的 `Exit` 有没有用旧代号。
        fn exit_seqs(&self) -> Vec<(String, u64)> {
            let calls = self
                .stops
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            calls
                .iter()
                .map(|progress| (progress.item_id.as_str().to_owned(), progress.session_seq))
                .collect()
        }

        /// `report` 收到的服务端会话标识（按发送顺序）。
        fn report_sessions(&self) -> Vec<Option<String>> {
            session_ids(&self.reports)
        }

        /// `stop` 收到的服务端会话标识（按发送顺序）。
        fn stop_sessions(&self) -> Vec<Option<String>> {
            session_ids(&self.stops)
        }
    }

    fn session_ids(calls: &std::sync::Mutex<Vec<PlaybackProgress>>) -> Vec<Option<String>> {
        let calls = calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        calls
            .iter()
            .map(|progress| progress.play_session_id.clone())
            .collect()
    }

    fn freeze(calls: &std::sync::Mutex<Vec<PlaybackProgress>>) -> Vec<(String, Duration)> {
        let calls = calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        calls
            .iter()
            .map(|progress| (progress.item_id.as_str().to_owned(), progress.position))
            .collect()
    }

    #[async_trait::async_trait]
    impl nova_playback::ProgressSink for RecordingSink {
        async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
            self.reports
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(progress.clone());
            Ok(())
        }

        async fn stop(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
            self.stops
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(progress.clone());
            Ok(())
        }
    }

    /// 把上报调用记进**共享 trace** 的 sink：与 `Host` 的 `pause` / `open` 写同一条
    /// 时间线，才能断言「先停引擎、再上报」的顺序。
    struct TracingSink {
        trace: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl nova_playback::ProgressSink for TracingSink {
        async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
            traced(&self.trace).push(format!("report:{}", progress.position.as_secs()));
            Ok(())
        }

        async fn stop(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
            traced(&self.trace).push(format!("stop:{}", progress.position.as_secs()));
            Ok(())
        }
    }

    fn traced(
        trace: &Arc<std::sync::Mutex<Vec<String>>>,
    ) -> std::sync::MutexGuard<'_, Vec<String>> {
        trace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// trace 的一份拷贝（持锁时间只在函数内，断言消息里再取一次也不会死锁）。
    fn traced_text(trace: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
        traced(trace).clone()
    }

    /// 本地缓存里有两个条目（`a` / `b`），但本机没有令牌：起播会走到取直链那一步
    /// 并立刻失败（`base_url` 指向 `127.0.0.1:1`），不需要 mpv。会话状态由用例自己推进。
    fn switch_fixture() -> (
        tempfile::TempDir,
        Arc<Local>,
        Player,
        tokio::runtime::Runtime,
    ) {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let server = test_server_info();
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server, 1).expect("写服务器");
            let items: Vec<nova_core::MediaItem> = ["a", "b"]
                .iter()
                .map(|id| nova_core::MediaItem {
                    id: ItemId::new(*id),
                    kind: nova_core::MediaKind::Movie,
                    title: format!("片子 {id}"),
                    sort_title: String::new(),
                    original_title: None,
                    year: None,
                    overview: None,
                    genres: Vec::new(),
                    runtime: Some(Duration::from_secs(5400)),
                    parent_id: None,
                    index_number: None,
                    index_number_end: None,
                    child_count: None,
                    images: nova_core::ImageSet::default(),
                    user_data: nova_core::UserData::default(),
                })
                .collect();
            db.upsert_items(&server.id, &items, 2).expect("写条目");
            let sources: Vec<nova_core::MediaSource> = ["a", "b"]
                .iter()
                .map(|id| {
                    let mut source = source(&format!("src-{id}"));
                    source.item_id = ItemId::new(*id);
                    source.runtime = Some(Duration::from_secs(5400));
                    source
                })
                .collect();
            db.upsert_sources(&server.id, &sources).expect("写播放源");
        }
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        // 多线程 runtime：`end_session()` 要 `block_on` 等上报 worker 补发完，
        // current_thread runtime 上那个 worker 永远轮不到（同 `resume_fixture`）。
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = Rc::new(RefCell::new(Host::new(true)));
        let player = Player::new(
            app,
            Arc::clone(&local),
            runtime.handle().clone(),
            host,
            None,
            Rc::new(RefCell::new(None)),
        );
        (dir, local, player, runtime)
    }

    /// 换片必须先结束旧会话：`Exit` 进度交给上报队列，服务端收到 `stop`。
    ///
    /// 以前 `begin` 开头只有 `session.reset()`：旧片最后一段进度（上一次定时上报之后的
    /// 十几秒）既没提交，`stop` 也从不发出去，服务端的续播点停在上一次 Tick 的位置。
    #[test]
    fn beginning_another_item_ends_the_previous_session() {
        let (_dir, local, mut player, runtime) = switch_fixture();
        let recorder = Arc::new(RecordingSink::default());
        player.progress = sink::Progress::for_test(
            &local,
            runtime.handle(),
            Arc::clone(&recorder) as Arc<dyn ProgressSink>,
        );

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);
        // A 真的播起来了：首帧到了，看到了 10:00。
        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        player.session.advance_position(Duration::from_secs(600));
        assert_eq!(player.session.position(), Duration::from_secs(600));

        player.begin("b", None, None, &post);

        // 换片路径不再阻塞等补发（那是界面线程上的 `block_on`），`Exit` 只交给
        // worker：上报与 `stop` 是异步发生的，所以这里轮询等它做完。
        assert!(
            wait_until(|| recorder.stops().len() == 1),
            "Exit 必须被 worker 处理掉（否则服务端收不到 stop）: {:?}",
            recorder.stops()
        );
        assert_eq!(
            recorder.reports(),
            vec![("a".to_owned(), Duration::from_secs(600))],
            "换片前必须把 A 的进度（10:00，不是 0 也不是上次 Tick）作为 Exit 交出去"
        );
        assert_eq!(
            recorder.stops(),
            vec![("a".to_owned(), Duration::from_secs(600))],
            "服务端只在收到 stop 时落盘续播点"
        );
        assert!(
            player.target.is_none(),
            "B 还没有直链，不能把 A 的目标留在会话里"
        );
        assert_eq!(
            player.session.position(),
            Duration::ZERO,
            "B 从头开始（不是 A 的 10:00）"
        );
    }

    /// 复审 🟡：会话身份必须带上代号。`begin()` 每次（含同一部片重播）都要递增，
    /// 旧会话的 `Exit` 则用**旧代号**交出去——否则上报器与 sink 分不清"重播"与
    /// "同一个会话"：新会话的第一条上报会被上一会话的待发项吞掉，补发旧 `Exit`
    /// 时又会被当成新会话重新发一次 `Started`。
    #[test]
    fn replaying_the_same_item_bumps_the_session_seq() {
        let (_dir, local, mut player, runtime) = switch_fixture();
        let recorder = Arc::new(RecordingSink::default());
        player.progress = sink::Progress::for_test(
            &local,
            runtime.handle(),
            Arc::clone(&recorder) as Arc<dyn ProgressSink>,
        );

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);
        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        player.session.advance_position(Duration::from_secs(600));
        let first = player
            .build_progress()
            .expect("A 出画后可以上报")
            .session_seq;
        assert!(first > 0, "代号 0 留给旧数据，真会话从 1 开始");

        // 重播同一部片：旧会话的 `Exit` 带旧代号交给 worker，新会话拿新代号。
        player.begin("a", None, None, &post);
        assert!(
            wait_until(|| recorder.stops().len() == 1),
            "worker 应在期限内交出旧会话的 Exit"
        );
        assert_eq!(
            recorder.exit_seqs(),
            vec![("a".to_owned(), first)],
            "旧会话的 Exit 必须用旧代号"
        );

        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        let second = player
            .build_progress()
            .expect("重播出画后可以上报")
            .session_seq;
        assert!(
            second > first,
            "同一部片重播也是新会话（代号 {second} 应大于 {first}）"
        );
    }

    /// 能给出**服务端会话标识**的假 provider。
    ///
    /// 真 provider 的标识只能从网络（`PlaybackInfo`）拿，player 级用例用假 provider 才能
    /// 确定地安排"起播 / 详情刷新 / 重播各一次 `PlaybackInfo`、各一个标识"。
    ///
    /// 只实现用例真的走得到的方法；其余一律 `unreachable!`（测试代码，不是生产代码）。
    struct SessionProvider {
        /// 每次 `sources()`（`PlaybackInfo`）依次给出的会话标识，用完就不再记新标识。
        sessions: std::sync::Mutex<std::collections::VecDeque<String>>,
        /// 已经发出去的标识（按调用顺序），用来确认刷新真的发生过。
        served: std::sync::Mutex<Vec<String>>,
        /// 当前"条目最近一次"的标识：真 provider 的 `play_sessions` 也是这样被覆盖的。
        latest: std::sync::Mutex<Option<String>>,
        /// 起播那次 `PlaybackInfo` 之后、player 用上标识之前，再插一次「详情页刷新」：
        /// 条目上的“最近一次”于是变成**别人的**标识（旧实现「先 `sources()`、再查最近一次」
        /// 就会拿到它）。
        interleaved: bool,
        /// `item()` 永不返回：模拟起播前刷新撞上 400ms 上限。
        stalling: bool,
    }

    impl SessionProvider {
        fn new(sessions: &[&str]) -> Self {
            Self {
                sessions: std::sync::Mutex::new(
                    sessions
                        .iter()
                        .map(|session| (*session).to_owned())
                        .collect(),
                ),
                served: std::sync::Mutex::new(Vec::new()),
                latest: std::sync::Mutex::new(None),
                interleaved: false,
                stalling: false,
            }
        }

        /// 用例：在起播那次 `PlaybackInfo` 之后插一次别人的 `PlaybackInfo`。
        fn interleaved(mut self) -> Self {
            self.interleaved = true;
            self
        }

        /// 用例：起播前刷新卡住（走到 400ms 超时分支）。
        fn stalling(mut self) -> Self {
            self.stalling = true;
            self
        }

        /// 用例：条目上已经有**上一个会话**的标识（超时分支只会查到它）。
        fn with_latest(self, session: &str) -> Self {
            match self.latest.lock() {
                Ok(mut latest) => *latest = Some(session.to_owned()),
                Err(poisoned) => *poisoned.into_inner() = Some(session.to_owned()),
            }
            self
        }

        /// 取下一个标识，并把它记成条目上的"最近一次"（真 provider 的行为）。
        fn take_session(&self) -> Option<String> {
            let next = match self.sessions.lock() {
                Ok(mut queue) => queue.pop_front(),
                Err(poisoned) => poisoned.into_inner().pop_front(),
            };
            if let Some(session) = &next {
                match self.served.lock() {
                    Ok(mut served) => served.push(session.clone()),
                    Err(poisoned) => poisoned.into_inner().push(session.clone()),
                }
                match self.latest.lock() {
                    Ok(mut latest) => *latest = Some(session.clone()),
                    Err(poisoned) => *poisoned.into_inner() = Some(session.clone()),
                }
            }
            next
        }

        fn sources_for(&self, id: &ItemId) -> Vec<nova_core::MediaSource> {
            let mut source = source("src-1");
            source.item_id = id.clone();
            source.runtime = Some(Duration::from_secs(5400));
            vec![source]
        }

        fn served(&self) -> Vec<String> {
            match self.served.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }
    }

    fn session_item(id: &ItemId) -> nova_core::MediaItem {
        nova_core::MediaItem {
            id: id.clone(),
            kind: nova_core::MediaKind::Movie,
            title: format!("片子 {}", id.as_str()),
            sort_title: String::new(),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime: Some(Duration::from_secs(5400)),
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: nova_core::ImageSet::default(),
            user_data: nova_core::UserData::default(),
        }
    }

    #[async_trait::async_trait]
    impl nova_provider::MediaProvider for SessionProvider {
        fn kind(&self) -> nova_core::ServerKind {
            nova_core::ServerKind::Jellyfin
        }

        fn base_url(&self) -> &str {
            "http://127.0.0.1:1"
        }

        fn server_info(&self) -> nova_core::ServerInfo {
            unreachable!("用例只走起播前刷新")
        }

        async fn public_info_as_server_info(
            &self,
        ) -> Result<nova_core::ServerInfo, nova_provider::ProviderError> {
            unreachable!("用例只走起播前刷新")
        }

        async fn public_info(
            &self,
        ) -> Result<nova_provider::PublicServerInfo, nova_provider::ProviderError> {
            unreachable!("用例只走起播前刷新")
        }

        async fn authenticate(
            &self,
            _user: &str,
            _password: &nova_secrets::SecretString,
        ) -> Result<nova_core::ServerInfo, nova_provider::ProviderError> {
            unreachable!("用例只走起播前刷新")
        }

        async fn libraries(
            &self,
        ) -> Result<Vec<nova_provider::Library>, nova_provider::ProviderError> {
            unreachable!("用例只走起播前刷新")
        }

        async fn items(
            &self,
            _query: nova_provider::ItemQuery,
        ) -> Result<nova_provider::Page<nova_core::MediaItem>, nova_provider::ProviderError>
        {
            unreachable!("用例只走起播前刷新")
        }

        async fn item(
            &self,
            id: &ItemId,
        ) -> Result<nova_core::MediaItem, nova_provider::ProviderError> {
            if self.stalling {
                // 起播前刷新有 400ms 上限：挂住就能稳定走到超时分支。
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Ok(session_item(id))
        }

        async fn children(
            &self,
            _id: &ItemId,
        ) -> Result<Vec<nova_core::MediaItem>, nova_provider::ProviderError> {
            unreachable!("用例只走起播前刷新")
        }

        async fn sources(
            &self,
            id: &ItemId,
        ) -> Result<Vec<nova_core::MediaSource>, nova_provider::ProviderError> {
            // 真实服务端行为：每取一次 `PlaybackInfo` 就给一个新标识，并把它记成条目上
            // 的"最近一次"（旧会话的标识就此被覆盖，正是审查指出的那个问题）。
            self.take_session();
            if self.interleaved {
                // 另一次 `PlaybackInfo` 紧跟着完成：调用方接下来去查“最近一次”时，
                // 条目上的记录已经是别人的标识了。
                self.take_session();
            }
            Ok(self.sources_for(id))
        }

        async fn sources_with_session(
            &self,
            id: &ItemId,
        ) -> Result<nova_provider::PlaybackSources, nova_provider::ProviderError> {
            // 真实现的原子版本：资源与标识出自同一次 `PlaybackInfo`。
            let play_session_id = self.take_session();
            if self.interleaved {
                // 响应回来了，但 player 用上标识之前又插了一次 `PlaybackInfo`
                // （详情页刷新 / 重播）：条目上的“最近一次”变成别人的标识。
                self.take_session();
            }
            Ok(nova_provider::PlaybackSources {
                sources: self.sources_for(id),
                play_session_id,
            })
        }

        fn latest_play_session_id(&self, _item: &ItemId) -> Option<String> {
            match self.latest.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            }
        }

        async fn stream_target(
            &self,
            _item: &ItemId,
            _source: &SourceId,
        ) -> Result<nova_provider::StreamTarget, nova_provider::ProviderError> {
            // 用例不看直链；失败只会让 `OpenReady` 带错误回来（`post` 是空的）。
            Err(nova_provider::ProviderError::NoToken)
        }

        async fn report_started(
            &self,
            _progress: &PlaybackProgress,
        ) -> Result<(), nova_provider::ProviderError> {
            unreachable!("上报走注入的假 sink，不经过 provider")
        }

        async fn report_stopped(
            &self,
            _progress: &PlaybackProgress,
        ) -> Result<(), nova_provider::ProviderError> {
            unreachable!("上报走注入的假 sink，不经过 provider")
        }

        async fn report_progress(
            &self,
            _progress: &PlaybackProgress,
        ) -> Result<(), nova_provider::ProviderError> {
            unreachable!("上报走注入的假 sink，不经过 provider")
        }

        async fn image(
            &self,
            _request: nova_provider::ImageRequest,
        ) -> Result<Vec<u8>, nova_provider::ProviderError> {
            unreachable!("用例只看会话标识")
        }

        async fn logout(&self) -> Result<(), nova_provider::ProviderError> {
            Ok(())
        }
    }

    /// 审查 🟡1：会话标识要跟着**会话**走，而不是跟着条目（provider 的"最近一次"）。
    ///
    /// 以前 `build_progress_at` 不写标识，靠 `sink::Progress::stamp` 在入队那一刻去查
    /// 条目上的"最近一次"。但同一部片播放期间详情页也会取 `PlaybackInfo`
    /// （`Shell::spawn_detail` → `provider.sources()`），把映射覆盖成**第三个**标识：
    /// 之后入队的 `Exit` 带的就是详情刷新那个标识，服务端关错会话。
    ///
    /// 现在 player 在起播拿到 `PlaybackInfo` 的那一刻就把标识记进本会话，每条进度
    /// 原样带走；`stamp` 只在进度没带标识时兜底。
    #[test]
    fn play_session_id_stays_bound_to_its_own_session() {
        let (_dir, local, mut player, runtime) = switch_fixture();
        // 三次 `PlaybackInfo`：起播（旧会话）、播放中详情刷新、重播（新会话）。
        let provider = Arc::new(SessionProvider::new(&[
            "sess-old",
            "sess-detail",
            "sess-new",
        ]));
        player.provider = Some(Arc::clone(&provider) as Arc<dyn nova_provider::MediaProvider>);
        let recorder = Arc::new(RecordingSink::default());
        player.progress = sink::Progress::for_test(
            &local,
            runtime.handle(),
            Arc::clone(&recorder) as Arc<dyn ProgressSink>,
        );

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);
        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        player.session.advance_position(Duration::from_secs(600));

        assert_eq!(
            provider.served(),
            ["sess-old"],
            "用例前提：起播前刷新取过一次 PlaybackInfo"
        );
        let first = player.build_progress().expect("出画后可以上报");
        assert_eq!(
            first.play_session_id.as_deref(),
            Some("sess-old"),
            "起播时拿到的标识必须记进本会话，写进每条进度"
        );
        player.report(ProgressTrigger::Seek);

        // 播放中详情页刷新：条目上的"最近一次"被覆盖，但本会话的进度不能跟着变。
        let item = ItemId::new("a");
        runtime
            .block_on(nova_provider::MediaProvider::sources(
                provider.as_ref(),
                &item,
            ))
            .expect("详情刷新");
        assert_eq!(
            nova_provider::MediaProvider::latest_play_session_id(provider.as_ref(), &item)
                .as_deref(),
            Some("sess-detail"),
            "用例前提：详情刷新会覆盖条目上的标识"
        );
        player.session.advance_position(Duration::from_secs(10));
        let after_refresh = player.build_progress().expect("出画后可以上报");
        assert_eq!(
            after_refresh.play_session_id.as_deref(),
            Some("sess-old"),
            "详情刷新不能改掉本会话的标识（否则 Exit 关错会话）"
        );
        player.report(ProgressTrigger::Seek);

        // 同一部片重播：旧会话的 `Exit` 用**旧**标识交给 worker，新会话取到新标识。
        player.begin("a", None, None, &post);
        assert!(
            wait_until(|| recorder.stop_sessions().len() == 1),
            "worker 应在期限内交出旧会话的 Exit: {:?}",
            recorder.stop_sessions()
        );
        assert_eq!(
            recorder.stop_sessions(),
            [Some("sess-old".to_owned())],
            "旧会话的 stop 必须带旧会话的标识（不是详情刷新的那个，也不是新会话的）"
        );

        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        let second = player.build_progress().expect("重播出画后可以上报");
        assert_eq!(
            second.play_session_id.as_deref(),
            Some("sess-new"),
            "重播是新会话：首报要带新会话的标识"
        );
        player.report(ProgressTrigger::Seek);

        assert!(
            wait_until(|| recorder.report_sessions().len() == 4),
            "旧会话的两条 + Exit + 新会话首报都应交给 worker: {:?}",
            recorder.report_sessions()
        );
        assert_eq!(
            recorder.report_sessions(),
            [
                Some("sess-old".to_owned()),
                Some("sess-old".to_owned()),
                Some("sess-old".to_owned()),
                Some("sess-new".to_owned()),
            ],
            "每条进度都带自己会话的标识：详情刷新（sess-detail）不能污染任何一条"
        );
    }

    /// 起播那次 `PlaybackInfo` 是**原子**的：资源与会话标识来自同一个响应。
    ///
    /// 以前 player 先 `sources()`、再 `latest_play_session_id()`：两次调用之间如果插进
    /// 一次详情页刷新，本会话就会拿到**别人的**标识（那次刷新的），退出时关错会话。
    /// 现在 `sources_with_session()` 把标识随资源一起带回，player 直接用这个值。
    #[test]
    fn startup_snapshot_uses_the_same_playback_info_response() {
        let (_dir, _local, mut player, runtime) = switch_fixture();
        // 起播那次 `PlaybackInfo` 给 `sess-own`；紧接着又插了一次（`sess-detail`）。
        let provider = Arc::new(SessionProvider::new(&["sess-own", "sess-detail"]).interleaved());
        player.provider = Some(Arc::clone(&provider) as Arc<dyn nova_provider::MediaProvider>);

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);

        assert_eq!(
            nova_provider::MediaProvider::latest_play_session_id(
                provider.as_ref(),
                &ItemId::new("a")
            )
            .as_deref(),
            Some("sess-detail"),
            "用例前提：插进来的那次 PlaybackInfo 把条目上的记录覆盖了"
        );
        assert_eq!(
            player.play_session_id.as_deref(),
            Some("sess-own"),
            "本会话的标识必须取自起播那次响应，不能回头查“最近一次”"
        );

        // 老做法（`sources()` + `latest_play_session_id()`）会拿到插进来的那个：
        // 同一个前提在另一份假 provider 上重放一遍，锁住“为什么不能再分两步查”。
        let legacy = SessionProvider::new(&["sess-own", "sess-detail"]).interleaved();
        runtime
            .block_on(nova_provider::MediaProvider::sources(
                &legacy,
                &ItemId::new("a"),
            ))
            .expect("取资源");
        assert_eq!(
            nova_provider::MediaProvider::latest_play_session_id(&legacy, &ItemId::new("a"))
                .as_deref(),
            Some("sess-detail"),
            "分两步查时拿到的就是别人的标识——正是这次要修的"
        );
    }

    /// 起播前刷新超时（400ms 上限）→ 本会话没有标识 → 入队时 `stamp` 退回条目上的
    /// "最近一次"（那里可能是**上一次会话**的旧标识，所以只当兜底）。
    ///
    /// 这条降级路径以前没有用例：改动取标识的方式（`sources_with_session`）后，
    /// 超时分支必须还能把 `None` 交给调用方，而不是把旧标识当成新会话的。
    #[test]
    fn startup_refresh_timeout_leaves_the_session_id_empty_and_stamp_falls_back() {
        let (_dir, local, mut player, runtime) = switch_fixture();
        // `item()` 挂住：起播前刷新必然撞上 400ms 上限；条目上留着旧会话的标识。
        let provider = Arc::new(
            SessionProvider::new(&[])
                .with_latest("sess-stale")
                .stalling(),
        );
        player.provider = Some(Arc::clone(&provider) as Arc<dyn nova_provider::MediaProvider>);
        let recorder = Arc::new(RecordingSink::default());
        player.progress = sink::Progress::for_test_with_provider(
            &local,
            runtime.handle(),
            Arc::clone(&recorder) as Arc<dyn ProgressSink>,
            Arc::clone(&provider) as Arc<dyn nova_provider::MediaProvider>,
        );

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);
        assert!(
            player.play_session_id.is_none(),
            "起播前刷新超时：本会话的标识必须是 None，不能拿旧标识顶替"
        );
        assert!(
            provider.served().is_empty(),
            "用例前提：超时发生在 `PlaybackInfo` 之前，没有取到新标识"
        );

        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        player.session.advance_position(Duration::from_secs(600));
        let progress = player.build_progress().expect("出画后可以上报");
        assert!(
            progress.play_session_id.is_none(),
            "player 不能自己编标识出来"
        );
        player.report(ProgressTrigger::Seek);

        assert!(
            wait_until(|| recorder.report_sessions().len() == 1),
            "上报应当到达假 sink: {:?}",
            recorder.report_sessions()
        );
        assert_eq!(
            recorder.report_sessions(),
            [Some("sess-stale".to_owned())],
            "没有标识时由 `stamp` 退回条目上的“最近一次”（旧会话）"
        );
    }

    /// 体检 R8-B1 / R8-B2：播完（`Ended`）后按播放键 / `Enter` → 重播，且从 0 开始。
    ///
    /// 以前 `Session::play` 在 `Ended` 返回 `false`，界面 toggle 完引擎一个命令都收不到；
    /// 就算走 `begin` 重开，也会沿用“播完条目仍保留的续播点”，画面直接停在片尾。
    #[test]
    fn returning_to_playback_after_end_starts_a_new_session_from_zero() {
        let (_dir, _local, mut player, _runtime, posted) = resume_fixture(true);
        let post = post_to(&posted);

        player.begin("item-1", None, None, &post);
        let first = wait_for_open(&posted).expect("首次起播请求");
        assert_eq!(
            first.resume_at,
            Duration::from_secs(2747),
            "用例前提：首次起播续在 45:47"
        );
        player.target = player.pending.take();
        let first_seq = player.session_seq;
        player.session.handle(EngineEvent::FirstFrame);
        player.session.handle(EngineEvent::Ended);
        assert_eq!(player.session.state(), SessionState::Ended);
        assert_eq!(player.session.position(), Duration::from_secs(5483));

        // 控制层的播放键 / Enter 最终都是“想要播放”这一个动作。
        player.set_playing(true);

        let replay = wait_for_open(&posted).expect("重播请求");
        assert_eq!(
            replay.resume_at,
            Duration::ZERO,
            "播完重播必须从 0 开始（不能续在片尾）"
        );
        assert_eq!(player.session.state(), SessionState::Opening);
        assert_eq!(player.session.position(), Duration::ZERO);
        assert!(
            player.session_seq > first_seq,
            "播完重播是新会话（代号递增，{} 应大于 {first_seq}）",
            player.session_seq
        );
    }

    /// 体检 R8-B3：播完后拖进度条 → 会话落到目标位置、回到 `Playing`，
    /// 引擎收到 `seek` + `play`，并按规则上报 `Seek`。
    #[test]
    fn seeking_after_end_resumes_playing_and_reports_seek() {
        let (_dir, local, mut player, runtime, _posted) = resume_fixture(true);
        let recorder = Arc::new(RecordingSink::default());
        player.progress = sink::Progress::for_test(
            &local,
            runtime.handle(),
            Arc::clone(&recorder) as Arc<dyn ProgressSink>,
        );
        let post: Post = Arc::new(|_| {});
        player.begin("item-1", None, None, &post);
        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        player.session.handle(EngineEvent::Ended);
        assert_eq!(player.session.state(), SessionState::Ended);

        player.seek(Duration::from_secs(2741));

        assert_eq!(
            player.session.position(),
            Duration::from_secs(2741),
            "拖到哪就落在哪"
        );
        assert_eq!(
            player.session.state(),
            SessionState::Playing,
            "片尾拖动 = 从新位置继续看（与 Netflix 一致）"
        );
        let trace = Arc::clone(&player.host.borrow().trace);
        assert!(
            traced_text(&trace).iter().any(|entry| entry == "seek:2741"),
            "引擎必须收到 seek：{:?}",
            traced_text(&trace)
        );
        assert!(
            traced_text(&trace).iter().any(|entry| entry == "play"),
            "EOF 上还要补一次 play（否则画面停在最后一帧）：{:?}",
            traced_text(&trace)
        );
        assert!(
            wait_until(|| recorder
                .reports()
                .contains(&("item-1".to_owned(), Duration::from_secs(2741)))),
            "seek 必须上报（否则服务端续播点停在片尾）: {:?}",
            recorder.reports()
        );
    }

    /// 等 worker 线程把命令处理完（最多 10 秒：只在失败时等满，成功路径几十毫秒内返回）。
    ///
    /// 换片路径不再阻塞等补发，界面线程只管把 `Exit` 排进 channel；上报与 `stop`
    /// 是 worker 异步做的，测试只能轮询。
    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    /// 换片不能在界面线程上等补发（回归：旧实现先 `flush()`——`block_on` + 1500ms
    /// 上限——再 `pause`，等的那段时间旧片的声音与画面一直在继续）。
    ///
    /// 断言：换片一次阻塞式 `flush()` 都不调；引擎在**上报之前**就停了；旧会话的
    /// `Exit` / `stop` 仍然照常交给 worker（不阻塞不等于不上报）。
    #[test]
    fn switching_items_stops_the_old_engine_before_reporting_progress() {
        let (_dir, local, mut player, runtime) = switch_fixture();
        let trace = player.host.borrow().trace.clone();
        player.progress = sink::Progress::for_test(
            &local,
            runtime.handle(),
            Arc::new(TracingSink {
                trace: Arc::clone(&trace),
            }) as Arc<dyn ProgressSink>,
        );

        let post: Post = Arc::new(|_| {});
        player.begin("a", None, None, &post);
        player.target = player.pending.take();
        player.session.handle(EngineEvent::FirstFrame);
        player.session.advance_position(Duration::from_secs(600));
        traced(&trace).clear();

        player.begin("b", None, None, &post);

        assert_eq!(
            player.progress.flush_count(),
            0,
            "换片路径不能调 `Progress::flush()`：那是界面线程上的 block_on"
        );
        assert!(
            wait_until(|| traced_text(&trace).iter().any(|entry| entry == "stop:600")),
            "Exit 仍必须交给 worker（否则服务端永远收不到 stop）：{:?}",
            traced_text(&trace)
        );
        let log = traced_text(&trace);
        let at = |needle: &str| log.iter().position(|entry| entry == needle);
        let pause = at("pause").expect("换片必须停掉旧引擎");
        let report = at("report:600").expect("旧会话的 Exit 进度必须上报");
        let stop = at("stop:600").expect("服务端要收到 stop");
        assert!(
            pause < report && pause < stop,
            "必须先停旧引擎、再上报进度（旧实现先 block_on 等补发）：{log:?}"
        );
    }

    /// 复审 💡5：换片时**旧引擎先停**，新引擎的 `OpenRequest` 才下去。
    ///
    /// 顺序错了就是两路声音（旧片还在放，新片已经开），或者旧引擎把新会话的
    /// 位置报上来。
    #[test]
    fn switching_items_stops_the_old_engine_before_opening_the_new_one() {
        let (_dir, _local, mut player, _runtime) = switch_fixture();
        let trace = player.host.borrow().trace.clone();
        let post: Post = Arc::new(|_| {});

        // A 已经打开并出画。
        player.begin("a", None, None, &post);
        let generation = player.generation;
        player.on_open_ready(
            generation,
            Ok(Box::new(OpenRequest::new("http://a.example/stream"))),
        );
        player.session.handle(EngineEvent::FirstFrame);
        traced(&trace).clear();

        // 换片：B 的直链还没回来，引擎必须已经被停掉。
        player.begin("b", None, None, &post);
        assert_eq!(
            traced_text(&trace).first().map(String::as_str),
            Some("pause"),
            "换片的第一件事是停旧引擎（不能先等上报补发）：{:?}",
            traced_text(&trace)
        );

        // B 的直链回来：新请求交给引擎，且仍然排在 pause 之后。
        let generation = player.generation;
        player.on_open_ready(
            generation,
            Ok(Box::new(OpenRequest::new("http://b.example/stream"))),
        );
        let log = traced_text(&trace);
        assert_eq!(
            log.last().map(String::as_str),
            Some("open:http://b.example/stream"),
            "新引擎收到的必须是 B 的 OpenRequest：{log:?}"
        );
        assert!(
            log.iter().position(|entry| entry == "pause")
                < log
                    .iter()
                    .position(|entry| entry == "open:http://b.example/stream"),
            "旧引擎先停，新引擎再 open：{log:?}"
        );
        let host = player.host.borrow();
        assert_eq!(
            host.pending_open.as_ref().map(|open| open.url.as_str()),
            Some("http://b.example/stream"),
            "排进引擎的必须是 B 的请求"
        );
        drop(host);
    }

    fn init_test_backend() {
        let backend = i_slint_backend_testing::TestingBackend::new(
            i_slint_backend_testing::TestingBackendOptions {
                mock_time: true,
                threading: false,
                renderer_name: Some("software".into()),
            },
        );
        let _ = slint::platform::set_platform(Box::new(backend));
    }

    fn test_server_info() -> nova_core::ServerInfo {
        nova_core::ServerInfo {
            id: nova_core::ServerId::new("srv-player-test"),
            kind: nova_core::ServerKind::Jellyfin,
            name: "播放测试".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            version: None,
            user_id: None,
            auth: nova_core::AuthState::Unauthorized,
            metadata_language: nova_core::Language::undetermined(),
        }
    }

    #[test]
    fn explicit_source_wins_and_falls_back_to_first() {
        let sources = vec![source("a"), source("b")];
        assert_eq!(
            pick_source(&sources, Some("b")).map(|s| s.id.as_str().to_owned()),
            Some("b".to_owned())
        );
        assert_eq!(
            pick_source(&sources, Some("missing")).map(|s| s.id.as_str().to_owned()),
            Some("a".to_owned())
        );
        assert_eq!(
            pick_source(&sources, None).map(|s| s.id.as_str().to_owned()),
            Some("a".to_owned())
        );
        assert!(pick_source(&[], None).is_none());
    }

    /// 播放页的失败文案是**桌面界面**里的文字：不能把用户推回终端敲 `nova login`，
    /// 只能指向应用内的入口（Ctrl+N / 首页「连接服务器」），与 `status.rs` 的横幅一致。
    #[test]
    fn playback_failure_copy_never_sends_the_user_to_the_terminal() {
        assert!(
            !NO_SERVER_PLAYBACK.contains("nova login"),
            "{NO_SERVER_PLAYBACK}"
        );
        assert!(!NO_SERVER_PLAYBACK.contains("终端"), "{NO_SERVER_PLAYBACK}");
        assert!(
            NO_SERVER_PLAYBACK.contains("Ctrl+N"),
            "{NO_SERVER_PLAYBACK}"
        );
        assert!(
            NO_SERVER_PLAYBACK.contains("连接服务器"),
            "{NO_SERVER_PLAYBACK}"
        );
    }

    #[test]
    fn drain_stream_returns_ready_events_only() {
        // 空流：立刻返回，不 await。
        let mut stream = futures::stream::iter(Vec::<EngineEvent>::new());
        assert!(drain_stream(&mut stream).is_empty());
    }

    /// 体检 R5-1：缓冲期（首帧之前）引擎发来的一切都属于**上一次**会话。
    ///
    /// 时序完全照真机：会话 A 真的播到 `OLD` → 退出 → 会话 B 以续播点 `Y` 打开 →
    /// 引擎在 `file-loaded` / 首帧之前发 A 的位置与 0 → 退出。断言：
    ///
    /// - `pump_events` 在本次 open 下发之前丢弃所有事件；
    /// - 下发之后、首帧之前的位置也不进会话（`Opening` 门）；
    /// - 界面显示的是本次 open 的续播点 `Y`（R5-2）；
    /// - `build_progress()`（`exit` / Tick / Pause / 切轨的唯一入口）返回 `None`：
    ///   一条进度都不发，服务端已有的续播点不可能被写成 `OLD` 或 0。
    #[test]
    fn buffering_session_never_reports_the_previous_sessions_position() {
        let (mut player, _dir) = silent_player();
        let total = Some(Duration::from_secs(7000));
        let old = Duration::from_secs(5286);
        let resume = Duration::from_millis(555_488);

        // 会话 A：真的出过画，位置推到 5286s。
        player.session.begin(Duration::ZERO, total);
        player.session.handle(EngineEvent::FirstFrame);
        player.session.handle(EngineEvent::PositionChanged(old));
        assert_eq!(
            player.session.reportable_position(),
            Some(old),
            "正常播放的会话应该可以提交"
        );

        // 退出播放页（真实路径：`exit()` 提交后 `session.reset()`）。
        player.exit(old);

        // 会话 B：续播点 555.488s，请求还没下发（取直链的网络往返中）——
        // 这段窗口里 mpv 里还是上一部片子，事件照发。
        player.session.begin(resume, total);
        player.request = None;
        player.target = Some(Target {
            item_id: ItemId::new("b"),
            source_id: nova_core::SourceId::new("src-b"),
            title: "新片 B".to_owned(),
            subtitle: None,
            duration: total,
        });
        player
            .host
            .borrow_mut()
            .push_event(EngineEvent::PositionChanged(old));
        player
            .host
            .borrow_mut()
            .push_event(EngineEvent::PositionChanged(Duration::ZERO));
        player.pump_events();
        assert_eq!(
            player.session.position(),
            resume,
            "open 请求下发之前的事件必须整条丢掉（它们属于上一部）"
        );

        // 本次 open 的请求下发了（真实路径：`on_open_ready` → `enqueue_open`），
        // 但 mpv 还在加载新文件：旧的 `time-pos` 与 0 仍会从通道里冒出来。
        player.request = Some(OpenRequest::new("http://127.0.0.1:1/Videos/b/stream"));
        player
            .host
            .borrow_mut()
            .push_event(EngineEvent::PositionChanged(old));
        player
            .host
            .borrow_mut()
            .push_event(EngineEvent::PositionChanged(Duration::ZERO));
        player.pump_events();

        assert_eq!(
            player.live_view().position,
            resume,
            "缓冲期界面显示本次 open 的续播点（R5-2），不是上一部的 {old:?}"
        );
        assert_eq!(
            player.build_progress(),
            None,
            "缓冲期 Tick / 退出都不得提交位置"
        );
        assert_eq!(player.session.reportable_position(), None);

        // 真的出画之后，位置才可信，且必须是本次会话的值。
        player.session.handle(EngineEvent::FirstFrame);
        player
            .host
            .borrow_mut()
            .push_event(EngineEvent::PositionChanged(Duration::from_millis(556_500)));
        player.pump_events();
        let progress = player.build_progress().expect("出画后应该可以上报");
        assert_eq!(progress.position, Duration::from_millis(556_500));
        assert_ne!(progress.position, old);
        assert_ne!(progress.position, Duration::ZERO);
    }

    /// 字幕安全区的单位换算：界面给的是控制块到窗口底边的逻辑像素，mpv 要的是
    /// **画面**高度的百分比（letterbox 时要先减掉下黑边）。
    #[test]
    fn subtitle_lift_is_converted_from_pixels_to_picture_percentage() {
        let video_16_9 = Some((1920, 1080));
        let video_21_9 = Some((2560, 1080));
        // 1600×900 窗口、16:9 片源：无纵向黑边，266px / 900 = 29.6% → 30%。
        assert_eq!(subtitle_lift_percent(266, (1600.0, 900.0), video_16_9), 30);
        // 800×600、16:9：画面高 450px、下黑边 75px，抬 (266-75)/450 = 42.4% → 42%。
        assert_eq!(subtitle_lift_percent(266, (800.0, 600.0), video_16_9), 42);
        // 控制块变矮（安全区 250px）：抬 (250-75)/450 = 38.9% → 39%。
        assert_eq!(subtitle_lift_percent(250, (800.0, 600.0), video_16_9), 39);
        // 21:9 片源在 1600×900 里 letterbox：画面高 675、下黑边 112.5，
        // (266-112.5)/675 = 22.7% → 23%。
        assert_eq!(subtitle_lift_percent(266, (1600.0, 900.0), video_21_9), 23);
        // 拿不到画面尺寸时按无黑边（画面铺满窗口）。
        assert_eq!(subtitle_lift_percent(266, (800.0, 600.0), None), 44);
        // 控制层隐藏时界面给 0 → 不抬（mpv 回原位）。
        assert_eq!(subtitle_lift_percent(0, (1600.0, 900.0), video_16_9), 0);
        // 读不到窗口尺寸 / 坏值：不抬也比乱抬好。
        assert_eq!(subtitle_lift_percent(266, (0.0, 0.0), video_16_9), 0);
        assert_eq!(subtitle_lift_percent(266, (f64::NAN, 900.0), video_16_9), 0);
        assert_eq!(subtitle_lift_percent(-10, (1600.0, 900.0), video_16_9), 0);
        // 安全区完全落在下黑边里：无需抬起。
        assert_eq!(
            subtitle_lift_percent(75, (800.0, 600.0), Some((1920, 1080))),
            0
        );
        // 极端值夹到 90%（再高字幕就跑到画面中间了）。
        assert_eq!(
            subtitle_lift_percent(1_000_000, (1600.0, 900.0), video_16_9),
            90
        );
    }

    /// 控制层显隐 → 下发给引擎的字幕位置（百分比）：隐藏归零、可见抬起。
    ///
    /// 走真实的 `Player::set_subtitle_margin`（`nova-ui` 的 `player-subtitle-margin`
    /// 回调落到这里），并用测试后端上的窗口尺寸换算。
    #[test]
    fn control_visibility_toggles_the_engine_subtitle_lift() {
        let (mut player, _dir) = silent_player();
        player
            .app
            .ui()
            .window()
            .set_size(slint::PhysicalSize::new(1600, 900));

        // 控制层可见（`nova-ui` 推 266px 安全区），然后隐藏。
        player.set_subtitle_margin(266);
        player.set_subtitle_margin(0);
        // 重复的同一值不重复下发。
        player.set_subtitle_margin(0);

        assert_eq!(
            player.host.borrow().subtitle_margins,
            vec![30, 0],
            "可见 → 画面高度 30%（266/900），隐藏 → 0（回原位），重复值不重复下发"
        );
    }

    /// 一个不上报进度的播放器（没有 provider），配一个空数据库。
    fn silent_player() -> (Player, tempfile::TempDir) {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = Rc::new(RefCell::new(Host::new(true)));
        let player = Player::new(
            app,
            local,
            runtime.handle().clone(),
            host,
            None,
            Rc::new(RefCell::new(None)),
        );
        (player, dir)
    }
}
