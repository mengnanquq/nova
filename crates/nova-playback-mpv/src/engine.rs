// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! [`MpvEngine`]：libmpv + OpenGL render API 的 [`PlaybackEngine`] 实现。
//!
//! # 线程模型
//!
//! | 线程 | 做什么 |
//! |---|---|
//! | UI / GL 线程（`MpvEngine` 所在线程） | [`MpvEngine::attach_gl`]、[`MpvEngine::render`]、[`MpvEngine::texture`]、所有控制面调用 |
//! | worker 线程（`nova-mpv`） | `mpv_wait_event`、属性观察、命令执行、事件投递 |
//!
//! libmpv 的 client API 是线程安全的，但 `mpv_render_context_*` **必须**在持有 GL 上下文的
//! 线程上调用，所以渲染只走 UI 线程；把事件循环放到独立线程是为了让事件流不依赖 UI 是否
//! 在重绘（窗口被遮挡、Wayland 锁屏时 UI 可能长时间不渲染）。
//!
//! # 本 crate 不建窗口
//!
//! 窗口、`set_rendering_notifier`、`BorrowedOpenGLTextureBuilder` 都在 `nova-app` 里。
//! 这里只负责"把 mpv 的画面渲染进一张 `GL_RGBA8` 纹理"，纹理 id 通过
//! [`MpvEngine::texture`] 交出去。
//!
//! # 首帧判定（实测结论写在 [`MpvEngine::render`] 的注释里）
//!
//! `MPV_EVENT_VIDEO_RECONFIG` 只说明"视频链重新配置过"，`MPV_EVENT_PLAYBACK_RESTART`
//! 只说明"时钟恢复走"（纯音频文件也会来），两者都**不能**证明有画面。唯一可靠的证据是
//! `mpv_render_context_update()` 给出 `MPV_RENDER_UPDATE_FRAME` 并且
//! `mpv_render_context_render()` 真的把一帧画进了我们的 FBO。所以首帧 = 第一次
//! `render()` 成功。前面两个事件只用来给首帧耗时打时间戳（[`MpvEngine::start_trace`]）。

use std::pin::Pin;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::Stream;
use nova_playback::{
    EngineDiagnostics, EngineEvent, OpenRequest, PlaybackEngine, PlaybackError, SubtitleSelection,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::ffi::{Handle, RenderContext};
use crate::gl::{GlFunctions, VideoTarget};
use crate::options::EngineOptions;
use crate::subtitles::SubtitleCache;
use crate::worker::{self, Command, EventSink, SharedState, SubtitleChoice, Worker};

/// watchdog 阈值：距离上次 `render()` 超过这么久、且处于播放态，就要求上层主动驱动一帧。
///
/// ADR-0001 坑 2：Wayland 锁屏 / 窗口遮挡时合成器不发 `wl_surface.frame` 回调，
/// `request_redraw()` 变成空操作；只靠合成器驱动的渲染会静默停住。
pub const REDRAW_WATCHDOG: Duration = Duration::from_millis(500);

/// 事件流：worker 线程投递、上层轮询的 [`EngineEvent`] 队列。
///
/// 基于 tokio 的 unbounded channel：发送端不需要额外唤醒逻辑，接收端 `poll_recv`
/// 自带 waker 注册。
pub struct EventStream {
    rx: UnboundedReceiver<EngineEvent>,
}

impl Stream for EventStream {
    type Item = EngineEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.rx.poll_recv(cx)
    }
}

impl Unpin for EventStream {}

/// libmpv 播放引擎。
///
/// 生命周期：`new` → `attach_gl`（GL 上下文 current）→ `open` → 每帧 `render` →
/// `close`（或 `Drop`，同样要求 GL 上下文 current）。**一个实例对应一个 GL 上下文**：
/// 上下文重建（窗口重建、GPU 切换）时先 `close()`，再新建引擎。
pub struct MpvEngine {
    options: EngineOptions,
    shared: Arc<Mutex<SharedState>>,
    handle: Option<Arc<Handle>>,
    render: Option<RenderContext>,
    gl: Option<GlFunctions>,
    target: Option<VideoTarget>,
    /// 纹理被重建的次数（上层可以据此决定要不要重做 Slint 的 `Image`）。
    texture_serial: u64,
    commands: Option<Sender<Command>>,
    worker: Option<std::thread::JoinHandle<()>>,
    events_rx: EventStream,
    events: EventSink,
    subtitles: SubtitleCache,
    closed: bool,
    frames: u64,
}

impl std::fmt::Debug for MpvEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpvEngine")
            .field("attached", &self.render.is_some())
            .field("frames", &self.frames)
            .field("texture_serial", &self.texture_serial)
            .field("closed", &self.closed)
            .finish()
    }
}

impl MpvEngine {
    /// 建引擎：创建 mpv core、设 option、初始化，并起 worker 线程。
    ///
    /// 此时**还没有** render context（GL 上下文还不存在），要先 [`MpvEngine::attach_gl`]。
    ///
    /// # Errors
    /// mpv 创建/初始化失败、option 不被接受时返回 [`PlaybackError::engine`]。
    pub fn new(options: EngineOptions) -> Result<Self, PlaybackError> {
        let handle = Handle::create().map_err(PlaybackError::engine)?;
        for (name, value) in options.mpv_options() {
            match handle.set_option(&name, &value) {
                Ok(()) => {}
                Err(message) => match crate::options::fallback_for(&name, &value) {
                    // 调研清单里有两个取值在 mpv 0.42 上不被接受（实测），用等价的合法取值
                    // 顶上并明确告警——不允许静默改配置。
                    Some(fallback) => {
                        tracing::warn!(
                            target: "nova_playback_mpv",
                            "mpv 不接受 {name}={value}（{message}），改用 {name}={fallback}"
                        );
                        handle.set_option(&name, fallback).map_err(|message| {
                            PlaybackError::engine(format!("mpv option {name}: {message}"))
                        })?;
                    }
                    None => {
                        return Err(PlaybackError::engine(format!(
                            "mpv option {name}: {message}"
                        )));
                    }
                },
            }
        }
        handle.initialize().map_err(PlaybackError::engine)?;
        handle
            .request_log_messages(options.log_level_value())
            .map_err(PlaybackError::engine)?;
        worker::observe_properties(&handle).map_err(PlaybackError::engine)?;

        let handle = Arc::new(handle);
        let shared = Arc::new(Mutex::new(SharedState::new(options.requested_hwdec())));
        let (events_tx, events_rx): (UnboundedSender<EngineEvent>, UnboundedReceiver<EngineEvent>) =
            tokio::sync::mpsc::unbounded_channel();
        let events = EventSink::new(events_tx);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let worker_state = Worker::new(
            Arc::clone(&handle),
            events.clone(),
            Arc::clone(&shared),
            options.start_volume(),
        );
        let join = std::thread::Builder::new()
            .name("nova-mpv".to_string())
            .spawn(move || worker_state.run(cmd_rx))
            .map_err(|error| PlaybackError::engine(format!("启动 mpv 线程失败: {error}")))?;

        Ok(Self {
            options,
            shared,
            handle: Some(handle),
            render: None,
            gl: None,
            target: None,
            texture_serial: 0,
            commands: Some(cmd_tx),
            worker: Some(join),
            events_rx: EventStream { rx: events_rx },
            events,
            subtitles: SubtitleCache::new(),
            closed: false,
            frames: 0,
        })
    }

    /// 接入 GL：创建 mpv 的 render context。
    ///
    /// **必须由 UI 在 `RenderingState::RenderingSetup`（或任何 GL 上下文 current 的时刻）
    /// 调用，并且在第一次 `open` 之前**。Linux 上会自动把 DRM render node 交给 mpv，
    /// 这是 `hwdec=vaapi` 零拷贝生效的前提（ADR-0001 坑 1）。
    ///
    /// # Errors
    /// 已有 render context（同一实例只能接一个 GL 上下文）、或 mpv 创建 render context 失败。
    pub fn attach_gl(&mut self, gl: GlFunctions) -> Result<(), PlaybackError> {
        if self.render.is_some() {
            return Err(PlaybackError::engine(
                "渲染上下文已接入；重建 GL 上下文时请先 close() 并新建引擎",
            ));
        }
        let handle = self
            .handle
            .clone()
            .ok_or_else(|| PlaybackError::engine("引擎已关闭"))?;
        let render_fd = open_render_node(&self.options);
        if render_fd.is_none()
            && cfg!(target_os = "linux")
            && self.options.requested_hwdec() == "vaapi"
        {
            // 不是致命错误（软解也能播），但必须说出来：VAAPI 会静默退回软解。
            tracing::warn!(
                target: "nova_playback_mpv",
                "找不到可用的 DRM render node（/dev/dri/renderD*）；hwdec=vaapi 会退回软解"
            );
        }
        let render = RenderContext::create(&handle, render_fd).map_err(PlaybackError::engine)?;
        let renderer = gl.renderer();
        tracing::debug!(target: "nova_playback_mpv", "GL 实现: {renderer}");
        {
            let mut shared = self.lock_shared();
            shared.renderer = renderer.clone();
            shared.diagnostics.render_backend =
                Some(format!("libmpv OpenGL render API（{renderer}）"));
        }
        self.gl = Some(gl);
        self.render = Some(render);
        Ok(())
    }

    /// 渲染一帧：把 mpv 的新画面画进引擎自己的 `GL_RGBA8` 纹理。
    ///
    /// `fbo` / `width` / `height` 是**调用方当前绑定的帧缓冲**（Slint 的 back buffer，
    /// 也就是默认帧缓冲 `0`；`take_snapshot()` 时是离屏 FBO）与它的像素尺寸。
    /// mpv 会改掉 viewport/scissor/blend/clear color/纹理绑定，所以渲染完必须恢复成调用方
    /// 原来的样子；这里用参数拿到的 fbo 与尺寸做恢复，省掉一次 `glGetIntegerv`。
    ///
    /// 返回 `true` 表示这一帧真的有新画面（`MPV_RENDER_UPDATE_FRAME` + render 成功）。
    ///
    /// # 首帧判据
    ///
    /// 实测（mpv 0.42，`docs/research/player.md` §六.5 未决项）：起播时的事件顺序是
    /// `START_FILE` → `FILE_LOADED` → `VIDEO_RECONFIG` → `PLAYBACK_RESTART`，之后
    /// render update 才第一次给出 `MPV_RENDER_UPDATE_FRAME`。`VIDEO_RECONFIG` 在还没有
    /// 任何解码帧时就到了，`PLAYBACK_RESTART` 在纯音频文件上也会到，**两者都不能证明有画面**。
    /// 所以 [`EngineEvent::FirstFrame`] 只在"本函数真的把一帧渲染进纹理"时发出，这是唯一
    /// 与用户看到的画面一一对应的判据。前两个事件的时间戳留在
    /// [`MpvEngine::start_trace`] 里，用于诊断起播慢在哪一段。
    ///
    /// # Errors
    /// 渲染目标尺寸非法、FBO 不完整、mpv 渲染失败。
    pub fn render(&mut self, fbo: u32, width: u32, height: u32) -> Result<bool, PlaybackError> {
        let Some(render) = self.render.as_ref() else {
            return Ok(false);
        };
        let Some(gl) = self.gl else {
            return Ok(false);
        };
        self.lock_shared().last_render = Instant::now();

        // render.h：ADVANCED_CONTROL 下 update() 返回的 FRAME 位才是"有新帧可画"的信号。
        let flags = render.update();
        if !RenderContext::has_new_frame(flags) {
            return Ok(false);
        }
        let Some((video_width, video_height)) = self.lock_shared().video_size else {
            // 尺寸还不知道：没有可渲染的目标，下一帧再来。
            return Ok(false);
        };

        if self.target.as_ref().map(VideoTarget::size) != Some((video_width, video_height)) {
            if let Some(previous) = self.target.take() {
                gl.destroy_target(previous);
            }
            self.target = Some(
                gl.create_target(video_width, video_height)
                    .map_err(PlaybackError::engine)?,
            );
            self.texture_serial = self.texture_serial.saturating_add(1);
        }
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| PlaybackError::engine("视频渲染目标不可用"))?;

        let saved = gl.save_state();
        // mpv 的 GL 状态契约（render_gl.h 46–59 行「OpenGL state」）：进来之前
        // `GL_SCISSOR_TEST` / `GL_BLEND` / `GL_DEPTH_TEST` / `GL_STENCIL_TEST` /
        // `GL_CULL_FACE` 必须是默认的关闭状态（其余例外状态由 save/restore 自己处理）。
        // 宿主（Slint/femtovg）画完一帧总是开着 premultiplied 混合，不恢复它，mpv 的
        //「output to screen」pass 就会在混合状态下落盘——体检 R6-1 的 1920×1080 黑屏
        // 就是这么来的（详见 `MPV_REQUIRED_GL_DEFAULTS`）。
        // 执行顺序：先 `save_state`（记下宿主的状态），再复位，最后 `restore_state` 还回去。
        let violations = gl.restore_mpv_state_defaults();
        if !violations.is_empty() {
            tracing::debug!(
                target: "nova_playback_mpv",
                "宿主遗留了 mpv 不接受的 GL 状态，已复位：{violations:?}"
            );
        }
        let result = render.render_into(
            target.fbo(),
            video_width,
            video_height,
            self.options.block(),
        );
        gl.restore_state(&saved, fbo, width, height);
        result.map_err(PlaybackError::engine)?;

        self.frames = self.frames.saturating_add(1);
        let first_frame = {
            let mut shared = self.lock_shared();
            if shared.first_frame_at.is_none() {
                shared.first_frame_at = Some(Instant::now());
                shared.trace("first-frame-rendered");
                true
            } else {
                false
            }
        };
        if first_frame {
            self.events.emit(EngineEvent::FirstFrame);
        }
        self.events.emit(EngineEvent::FramePresented);
        Ok(true)
    }

    /// 当前视频帧纹理：`(texture_id, width, height)`。
    ///
    /// 交给 `nova-app` 的 `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(..)
    /// .origin(BorrowedOpenGLTextureOrigin::TopLeft)`。纹理 id 会在视频尺寸变化
    /// （`texture_serial` 变化）后失效。
    pub fn texture(&self) -> Option<(u32, u32, u32)> {
        let target = self.target.as_ref()?;
        let (width, height) = target.size();
        let texture = target.texture_id();
        if texture == 0 {
            return None;
        }
        Some((texture, width, height))
    }

    /// 纹理代号：每次重建（尺寸变化）自增。上层用它判断要不要重做 Slint 的 `Image`。
    pub fn texture_serial(&self) -> u64 {
        self.texture_serial
    }

    /// 是否需要上层主动驱动一帧。
    ///
    /// 两种情况下为真：
    ///
    /// 1. mpv 的 update 回调已经置位（有新帧等着画）；
    /// 2. **帧回调 watchdog**（ADR-0001 坑 2）：处于播放态且距离上次 `render()` 超过
    ///    [`REDRAW_WATCHDOG`]。Wayland 锁屏 / 窗口遮挡时合成器不发帧回调，只靠
    ///    `request_redraw()` 会让画面静默卡死；上层应把本函数当定时器用。
    pub fn needs_redraw(&self) -> bool {
        if let Some(signal) = self.render.as_ref().and_then(RenderContext::signal) {
            if signal.pending() {
                return true;
            }
        }
        let shared = self.lock_shared();
        let playing = !shared.paused && !shared.eof && shared.has_video;
        playing && shared.last_render.elapsed() >= REDRAW_WATCHDOG
    }

    /// 诊断信息（硬解实际值、解码器、渲染后端）。
    ///
    /// 硬解被静默降级时，[`EngineDiagnostics::hwdec_notice`] 会给出界面文案。
    /// 丢帧数不在这个结构体里（`nova-playback` 没有这个字段），用
    /// [`MpvEngine::dropped_frames`] 取。
    pub fn diagnostics(&self) -> EngineDiagnostics {
        self.lock_shared().diagnostics.clone()
    }

    /// 本次打开以来 mpv 报告的累计丢帧数。
    pub fn dropped_frames(&self) -> u64 {
        self.lock_shared().dropped_frames
    }

    /// mpv 是否已经到达片尾（`eof-reached` 属性，**实时**播放状态）。
    ///
    /// 与 [`MpvEngine::diagnostics`] 分开：诊断是起播期的快照，而这个是随播放推进
    /// 变化的状态。宿主用它兜底“停在片尾”的判定：元数据里的时长可能比真实文件长
    /// （差几十毫秒到几秒），这时位置永远贴不到时长，只看位置判不出播完。
    ///
    /// `eof-reached` 由 worker 的属性观察落到共享状态（`keep-open=yes` 下 mpv 到片尾
    /// 只送这条、不送 `end-file`）；新文件 / 往回拖之后 mpv 会送 `eof-reached=false`。
    pub fn eof_reached(&self) -> bool {
        self.lock_shared().eof
    }

    /// 首帧耗时（从 `open` 被接受，到第一帧真的渲染进纹理）。
    pub fn first_frame_latency(&self) -> Option<Duration> {
        let shared = self.lock_shared();
        let first = shared.first_frame_at?;
        Some(first.saturating_duration_since(shared.opened_at))
    }

    /// 起播阶段的事件时序（`open` → `file-loaded` → `video-reconfig` →
    /// `playback-restart` → `first-frame-rendered`），相对 `open` 的时刻。
    ///
    /// 这是"首帧判定用哪个事件"的实测依据来源，也是排查起播慢的第一手数据。
    pub fn start_trace(&self) -> Vec<(String, Duration)> {
        self.lock_shared()
            .trace
            .iter()
            .map(|(label, at)| ((*label).to_string(), *at))
            .collect()
    }

    /// 字幕延迟（秒，**可负**）。
    ///
    /// [`PlaybackEngine::set_subtitle_delay`] 的签名是 `Duration`（只有非负），
    /// 而 mpv 的 `sub-delay` 允许负值（字幕提前出现）。这个固有方法补上负值。
    pub fn set_subtitle_delay_seconds(&mut self, seconds: f64) -> Result<(), PlaybackError> {
        self.send(Command::SetSubtitleDelay(seconds))
    }

    fn send(&self, command: Command) -> Result<(), PlaybackError> {
        let sender = self
            .commands
            .as_ref()
            .ok_or_else(|| PlaybackError::engine("引擎已关闭"))?;
        sender
            .send(command)
            .map_err(|_| PlaybackError::engine("mpv 线程已退出"))
    }

    fn lock_shared(&self) -> std::sync::MutexGuard<'_, SharedState> {
        self.shared
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// 把 `SubtitleSelection` 归一化：外挂字幕先落盘。
    async fn subtitle_choice(
        &mut self,
        selection: SubtitleSelection,
        headers: &[(String, String)],
    ) -> Result<SubtitleChoice, PlaybackError> {
        Ok(match selection {
            SubtitleSelection::Off => SubtitleChoice::Off,
            SubtitleSelection::Track(index) => SubtitleChoice::Track(index),
            SubtitleSelection::External { path } => {
                SubtitleChoice::External(self.subtitles.resolve(&path, headers).await?)
            }
        })
    }

    /// 关闭：先释放 render context，再让 worker 退出，最后才销毁 mpv core。
    ///
    /// 顺序是 render.h 的硬要求（render context 必须在 `mpv_terminate_destroy` 之前释放），
    /// 且 render context 的释放要求 GL 上下文 current —— 所以本方法（以及 `Drop`）
    /// **必须在 GL 线程上、上下文 current 时调用**。
    fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(target) = self.target.take() {
            if let Some(gl) = self.gl {
                gl.destroy_target(target);
            }
        }
        // 1) render context 先释放（仍在 GL 线程上）。
        self.render = None;
        // 2) 让 worker 退出：它 drop 自己那份 Arc<Handle>。
        if let Some(sender) = self.commands.take() {
            let _ = sender.send(Command::Quit);
        }
        if let Some(join) = self.worker.take() {
            let _ = join.join();
        }
        // 3) 最后一份 Arc<Handle> 在这里 drop → mpv_terminate_destroy。
        self.handle = None;
        self.subtitles.clear();
    }
}

impl Drop for MpvEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 打开 DRM render node（VAAPI 零拷贝要把它交给 mpv）。
///
/// 拿不到不是致命错误：软解、`vaapi-copy` 都不需要它，但 `hwdec=vaapi` 会退回软解。
fn open_render_node(options: &EngineOptions) -> Option<std::os::fd::OwnedFd> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    for path in options.render_node_candidates() {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => {
                tracing::debug!(target: "nova_playback_mpv", "VAAPI interop render node: {}", path.display());
                return Some(std::os::fd::OwnedFd::from(file));
            }
            Err(error) => {
                tracing::debug!(
                    target: "nova_playback_mpv",
                    "打不开 render node {}: {error}",
                    path.display()
                );
            }
        }
    }
    None
}

#[async_trait]
impl PlaybackEngine for MpvEngine {
    async fn open(&mut self, request: OpenRequest) -> Result<(), PlaybackError> {
        if self.render.is_none() {
            return Err(PlaybackError::engine(
                "渲染上下文未接入：先调用 attach_gl（GL 上下文 current 时）",
            ));
        }
        if request.url.trim().is_empty() {
            return Err(PlaybackError::engine("播放地址为空"));
        }
        let subtitle = self
            .subtitle_choice(request.subtitle.clone(), &request.headers)
            .await?;
        self.frames = 0;
        self.send(Command::Open {
            url: request.url,
            headers: request.headers,
            start: request.resume_at,
            audio: request.audio,
            subtitle,
        })
    }

    async fn play(&mut self) -> Result<(), PlaybackError> {
        self.send(Command::Play)
    }

    async fn pause(&mut self) -> Result<(), PlaybackError> {
        self.send(Command::Pause)
    }

    async fn seek(&mut self, position: Duration) -> Result<(), PlaybackError> {
        self.send(Command::Seek(position))
    }

    async fn set_volume(&mut self, volume: f32) -> Result<(), PlaybackError> {
        self.send(Command::SetVolume(volume))
    }

    async fn set_muted(&mut self, muted: bool) -> Result<(), PlaybackError> {
        self.send(Command::SetMuted(muted))
    }

    async fn select_audio(&mut self, index: u32) -> Result<(), PlaybackError> {
        self.send(Command::SelectAudio(index))
    }

    async fn select_subtitle(&mut self, selection: SubtitleSelection) -> Result<(), PlaybackError> {
        // 外挂字幕可能会下载：先落盘再切，不重启播放。
        let choice = self.subtitle_choice(selection, &[]).await?;
        self.send(Command::SelectSubtitle(choice))
    }

    async fn set_subtitle_delay(&mut self, delay: Duration) -> Result<(), PlaybackError> {
        self.send(Command::SetSubtitleDelay(delay.as_secs_f64()))
    }

    async fn set_subtitle_delay_seconds(&mut self, seconds: f64) -> Result<(), PlaybackError> {
        // mpv 的 `sub-delay` 本身允许负值，直接透传（trait 默认实现会把负值丢掉）。
        self.send(Command::SetSubtitleDelay(seconds))
    }

    async fn set_subtitle_margin(&mut self, y: i64) -> Result<(), PlaybackError> {
        self.send(Command::SetSubtitleMargin(y))
    }

    async fn set_rate(&mut self, rate: f32) -> Result<(), PlaybackError> {
        self.send(Command::SetRate(rate))
    }

    async fn close(&mut self) -> Result<(), PlaybackError> {
        self.shutdown();
        Ok(())
    }

    fn events(&mut self) -> &mut (dyn Stream<Item = EngineEvent> + Unpin + Send) {
        &mut self.events_rx
    }

    fn diagnostics(&self) -> EngineDiagnostics {
        MpvEngine::diagnostics(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::EngineOptions;

    #[test]
    fn options_are_the_product_defaults() {
        let options = EngineOptions::new();
        assert_eq!(options.requested_hwdec(), crate::options::default_hwdec());
        assert!(options.block());
    }

    #[test]
    fn watchdog_threshold_is_500ms() {
        assert_eq!(REDRAW_WATCHDOG, Duration::from_millis(500));
    }
}
