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
        /// `OpenRequest` 或可读的失败原因。
        request: Result<Box<OpenRequest>, String>,
    },
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
        self.pending_open = Some(request);
    }

    fn play(&mut self, runtime: &Handle) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.play())
        {
            tracing::warn!(%error, "engine.play 失败");
        }
    }

    fn pause(&mut self, runtime: &Handle) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.pause())
        {
            tracing::warn!(%error, "engine.pause 失败");
        }
    }

    fn seek(&mut self, runtime: &Handle, position: Duration) {
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
    /// 引擎枚举出的轨道（`TracksChanged` 之前用服务端资源的兜底）。
    tracks: TrackSet,
    selected_audio: Option<u32>,
    selected_subtitle: SubtitleSelection,
    /// 起播失败 / 无服务器等界面错误（不是会话状态机给的）。
    error: Option<String>,
    /// 取直链的代数（丢弃过期结果）。
    generation: u64,
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
            pending: None,
            tracks: TrackSet::default(),
            selected_audio: None,
            selected_subtitle: SubtitleSelection::Off,
            error: None,
            generation: 0,
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
    pub(crate) fn begin(&mut self, item_id: &str, source_id: Option<String>, post: &Post) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
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
        let (item, sources) = self.refresh_before_play(item, sources);
        let Some(source) = pick_source(&sources, source_id.as_deref()) else {
            self.fail("这个条目没有可播放的版本：确认服务端上文件还在");
            return;
        };

        let resume_at = resume_position_for_item(&item, &self.prefs);
        let duration = source.runtime.or(item.runtime);
        self.tracks = TrackSet::from_source(&source);
        let plan = self.selector.reapply(&self.tracks, &self.prefs);
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

        let Some(provider) = self.local.provider_dyn() else {
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
                    Err(player_error_text(&error))
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
        request: Result<Box<OpenRequest>, String>,
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
            Err(message) => {
                self.pending = None;
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
        // 否则服务端收不到 Stop，续播点永远是 0（实测）。
        if let Some(progress) = self.build_progress() {
            self.progress.submit(ProgressTrigger::Exit, progress);
        }
        if let Some(outcome) = self.progress.flush() {
            tracing::info!(
                sent = outcome.sent,
                failed = outcome.failed,
                "捕获结束：进度已提交"
            );
        }

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
                    self.session.handle(event);
                    self.report(ProgressTrigger::Pause);
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
        if self.session.seek(position) {
            let runtime = self.runtime.clone();
            self.host.borrow_mut().seek(&runtime, position);
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
    /// 界面（`nova-ui`）传来的是**逻辑像素**：它按窗口高度的 35% 算出安全区高度
    /// （`PlayerView::subtitle_margin`），控制层的真实高度由 theme 的
    /// `player-controls-block + player-gutter` 推导（900px 窗口上 ≈ 屏幕高度的 28%），
    /// 所以 35% 安全区一定盖得住它。
    ///
    /// 引擎要的是百分比（mpv `sub-pos`），所以在这里换算：窗口高度必须是**逻辑像素**
    /// （与传入的 px 同一坐标系），否则缩放变化会让字幕高低跳动。
    pub(crate) fn set_subtitle_margin(&mut self, margin_px: i64) {
        let lift = subtitle_lift_percent(margin_px, self.window_height_logical());
        if self.subtitle_margin == lift {
            return;
        }
        self.subtitle_margin = lift;
        let runtime = self.runtime.clone();
        self.host.borrow_mut().set_subtitle_margin(&runtime, lift);
    }

    /// 窗口高度（逻辑像素）；读不到时返回 0（调用方按"不抬"处理）。
    fn window_height_logical(&self) -> f64 {
        let window = self.app.ui().window();
        let scale = f64::from(window.scale_factor());
        let height = f64::from(window.size().height);
        if scale > 0.0 && height > 0.0 {
            height / scale
        } else {
            0.0
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
            // 还没拿到请求：从头再来一遍准备流程。
            let post = self.post.clone();
            let target = self.pending.clone().or_else(|| self.target.clone());
            if let (Some(post), Some(target)) = (post, target) {
                let source = Some(target.source_id.as_str().to_owned());
                self.begin(target.item_id.as_str(), source, &post);
            }
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

    /// 退出播放页：先提交一次进度并尽力补发，再停掉引擎与计时器。
    ///
    /// `_ui_position` 是界面自己的读数，**不再使用**：旧实现拿它当
    /// `session.position()` 为 0 时的兜底，结果是两边都可能被污染（界面读数基于节流
    /// 推送的视图，缓冲期它就是上一部片子的位置或 0）。现在只提交
    /// `Session::reportable_position()`：本次打开还没出过画面就一条也不发。
    pub(crate) fn exit(&mut self, _ui_position: Duration) {
        if let Some(progress) = self.build_progress() {
            self.progress.submit(ProgressTrigger::Exit, progress);
        }
        if let Some(outcome) = self.progress.flush() {
            tracing::info!(
                sent = outcome.sent,
                failed = outcome.failed,
                queued = outcome.queued,
                "退出播放页：补发进度"
            );
        }
        let runtime = self.runtime.clone();
        self.host.borrow_mut().pause(&runtime);
        self.session.reset();
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

    /// 进程退出前的收尾（落盘补发文件）。
    pub(crate) fn shutdown(&mut self) {
        if let Some(progress) = self.build_progress() {
            self.progress.submit(ProgressTrigger::Exit, progress);
        }
        self.progress.shutdown();
    }

    // ---- 内部 ----

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
    fn refresh_before_play(
        &self,
        item: nova_core::MediaItem,
        sources: Vec<nova_core::MediaSource>,
    ) -> (nova_core::MediaItem, Vec<nova_core::MediaSource>) {
        let Some(provider) = self.local.provider_dyn() else {
            return (item, sources);
        };
        let id = item.id.clone();
        let refreshed = self.runtime.block_on(async {
            // 上限必须小：这段是**同步阻塞起播**的。实测在真实 WAN 上的 Emby，
            // 2 秒超时会把首帧推到 2.001s；400ms 足够覆盖局域网，WAN 超时就退回缓存。
            match tokio::time::timeout(std::time::Duration::from_millis(400), async {
                let fresh = provider.item(&id).await?;
                let sources = provider.sources(&id).await?;
                Ok::<_, nova_provider::ProviderError>((fresh, sources))
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
            Some((fresh, fresh_sources)) if !fresh_sources.is_empty() => (fresh, fresh_sources),
            Some((fresh, _)) => (fresh, sources),
            None => (item, sources),
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

/// 字幕上抬量：窗口逻辑像素 → 屏幕高度的百分比（mpv `sub-pos` 的坐标系）。
///
/// 输入是 `nova-ui` 按布局算出的安全区高度（逻辑像素），窗口高度也是逻辑像素——
/// 同一坐标系相除，缩放因子自然消掉。读不到窗口尺寸（尺寸为 0 / 非有限值）时返回 0
/// （= 不抬，回到 mpv 原位）：宁可不抬，也不能因为一个坏读数把字幕推到画面中间。
fn subtitle_lift_percent(margin_px: i64, window_height_logical: f64) -> i64 {
    if margin_px <= 0 || !window_height_logical.is_finite() || window_height_logical <= 0.0 {
        return 0;
    }
    let percent = (margin_px as f64 * 100.0 / window_height_logical).round();
    percent.clamp(0.0, 90.0) as i64
}

/// 把 provider 的错误翻译成可行动的中文（不含 URL / 令牌）。
fn player_error_text(error: &nova_provider::ProviderError) -> String {
    match error {
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
        player.begin("999999999", None, &post);

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
        player.begin("b", None, &post);
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

    /// 字幕安全区的单位换算：界面给的是窗口逻辑像素，mpv 要的是屏幕高度百分比。
    #[test]
    fn subtitle_lift_is_converted_from_pixels_to_a_percentage() {
        // 900px 高窗口、界面按 35% 算出的 315px 安全区 → 35%。
        assert_eq!(subtitle_lift_percent(315, 900.0), 35);
        // 控制层隐藏时界面给 0 → 不抬（mpv 回原位）。
        assert_eq!(subtitle_lift_percent(0, 900.0), 0);
        // 读不到窗口尺寸 / 坏值：不抬也比乱抬好。
        assert_eq!(subtitle_lift_percent(315, 0.0), 0);
        assert_eq!(subtitle_lift_percent(315, f64::NAN), 0);
        assert_eq!(subtitle_lift_percent(-10, 900.0), 0);
        // 极端窗口尺寸夹到 90%（再高字幕就跑到画面中间了）。
        assert_eq!(subtitle_lift_percent(1_000_000, 900.0), 90);
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

        // 控制层可见（`nova-ui` 按窗口高度的 35% 算 → 315px），然后隐藏。
        player.set_subtitle_margin(315);
        player.set_subtitle_margin(0);
        // 重复的同一值不重复下发。
        player.set_subtitle_margin(0);

        assert_eq!(
            player.host.borrow().subtitle_margins,
            vec![35, 0],
            "可见 → 屏幕高度 35%，隐藏 → 0（回原位），重复值不重复下发"
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
