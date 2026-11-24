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
mod gl;
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
}

impl Host {
    /// 新建（还没有引擎；`RenderingSetup` 里才建）。
    pub(crate) fn new(silent: bool) -> Self {
        Self {
            silent,
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

    /// 字幕安全区：控制层可见时把字幕往上抬（`sub-margin-y`）。
    fn set_subtitle_margin(&mut self, runtime: &Handle, y: i64) {
        if let Some(engine) = self.engine.as_mut()
            && let Err(error) = runtime.block_on(engine.set_subtitle_margin(y))
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
    fn request_capture(&mut self) {
        if self.capture_requested || self.shot.is_some() {
            return;
        }
        self.capture_requested = true;
    }

    fn take_shot(&mut self) -> Option<(gl::Frame, Option<String>)> {
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
            .origin(slint::BorrowedOpenGLTextureOrigin::BottomLeft)
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
    /// 当前字幕抬升量（`sub-margin-y`，px）：避免重复下发。
    subtitle_margin: i64,
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
        self.post = Some(Arc::clone(post));
        self.session.reset();
        self.selector.clear();
        self.selected_audio = None;
        self.selected_subtitle = SubtitleSelection::Off;
        self.tracks = TrackSet::default();
        self.dirty = true;
        self.subtitle_margin = 0;

        let Some(server) = self.local.server_id().cloned() else {
            self.fail("还没有连接服务器：先在终端运行 `nova login`");
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
        let mut view = self.app.player_view();
        view.title = title;
        view.duration = duration;
        view.position = resume_at;
        view.buffering = false;
        view.playing = true;
        self.pending_view = Some(view);

        let Some(provider) = self.local.provider_dyn() else {
            self.fail("没有可用的服务器连接：先在终端运行 `nova login`");
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

    /// 字幕安全区：控制层可见时把字幕抬起，隐藏时归零（只在数值变化时下发）。
    pub(crate) fn set_subtitle_margin(&mut self, y: i64) {
        if self.subtitle_margin == y {
            return;
        }
        self.subtitle_margin = y;
        let runtime = self.runtime.clone();
        self.host.borrow_mut().set_subtitle_margin(&runtime, y);
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
    pub(crate) fn exit(&mut self, ui_position: Duration) {
        // 界面传进来的是它自己的插值位置，未必跟上了引擎；进度必须以引擎播放时钟为准，
        // 否则退出上报会写成 0（实测：服务端日志 `stopped playback ... at 0ms`）。
        let position = if self.session.position().is_zero() {
            ui_position
        } else {
            self.session.position()
        };
        if let Some(progress) = self.build_progress_at(position) {
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
        self.capture = None;
        self.request = None;
        self.target = None;
        self.pending = None;
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
        self.build_progress_at(self.session.position())
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

        let mut view = self.app.player_view();
        self.fill_view(&mut view);
        self.pending_view = Some(view);
    }

    /// 取走待提交的视图（`Shell` 放下借用后调用 `App::set_player`）。
    pub(crate) fn take_pending_view(&mut self) -> Option<PlayerView> {
        self.pending_view.take()
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

    #[test]
    fn drain_stream_returns_ready_events_only() {
        // 空流：立刻返回，不 await。
        let mut stream = futures::stream::iter(Vec::<EngineEvent>::new());
        assert!(drain_stream(&mut stream).is_empty());
    }
}
