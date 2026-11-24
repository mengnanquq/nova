// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 把 libmpv 的 GL 渲染接到 Slint 窗口上，并做自动验证（像素回读 + 计时统计）。

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use i_slint_core::graphics::IntSize;
use slint::{
    BorrowedOpenGLTextureBuilder, BorrowedOpenGLTextureOrigin, GraphicsAPI, RenderingState,
};

use crate::gl::{FrameSample, GlContext, GlInfo, PixelStats, VideoTarget};
use crate::glproc;
use crate::stats;
use crate::sys::{Event, GlRenderer, Session};

slint::include_modules!();

/// 命令行参数。
pub struct Options {
    pub file: PathBuf,
    pub seconds: f64,
    pub hwdec: String,
    pub size: (u32, u32),
    pub frames_out: Option<PathBuf>,
    pub verbose: bool,
}

/// 一次窗口回读的记录。
pub struct ReadbackRecord {
    pub frame: usize,
    pub stats: PixelStats,
    pub path: Option<PathBuf>,
    pub read_ns: u64,
}

/// 跑完一次之后打印的东西。
pub struct Report {
    pub file: PathBuf,
    pub hwdec_requested: String,
    pub hwdec_current: Option<String>,
    pub pixelformat: Option<String>,
    pub video_size: Option<(u32, u32)>,
    pub window_size: (u32, u32),
    pub gl: Option<GlInfo>,
    pub frames: usize,
    pub duration_s: f64,
    pub e2e_fps: f64,
    pub render_p50_us: f64,
    pub render_p95_us: f64,
    pub render_mean_us: f64,
    pub readbacks: Vec<ReadbackRecord>,
    pub readback_p50_us: f64,
    pub frame_drop: Option<i64>,
    pub decoder_frame_drop: Option<i64>,
    pub loader_fallbacks: usize,
    /// 经 Slint 的 `get_proc_address` 解析过的不同 GL 符号数。
    pub loader_symbols: usize,
    pub render_mode: &'static str,
    /// 进程 CPU 时间（user+sys），用来判断是否真的"没在 CPU 上搬像素"。
    pub cpu_seconds: f64,
    /// 进程存活的墙钟时间。
    pub wall_seconds: f64,
}

impl Report {
    /// 平均占用多少个核（CPU 时间 / 墙钟时间）。
    pub fn cpu_cores(&self) -> f64 {
        if self.wall_seconds > 0.0 {
            self.cpu_seconds / self.wall_seconds
        } else {
            f64::NAN
        }
    }
}

impl Report {
    /// “画面真的不是黑屏”的判定。
    pub fn pixels_ok(&self) -> bool {
        !self.readbacks.is_empty()
            && self
                .readbacks
                .iter()
                .filter(|r| r.stats.looks_like_video())
                .count()
                * 2
                >= self.readbacks.len()
    }
}

/// 跑一次完整的“加载 → 播放 → 自动退出”。
pub fn run(opts: Options) -> Result<Report, String> {
    let file = opts.file.clone();
    let hwdec_requested = opts.hwdec.clone();
    let window_size = opts.size;

    let ui = AppWindow::new().map_err(|e| format!("创建 Slint 窗口失败: {e}"))?;
    ui.window().set_size(slint::LogicalSize::new(
        window_size.0 as f32,
        window_size.1 as f32,
    ));

    let core = Rc::new(RefCell::new(Core::new(opts)));

    {
        let core = core.clone();
        let ui_weak = ui.as_weak();
        ui.window()
            .set_rendering_notifier(move |state, api| {
                on_rendering_state(&core, &ui_weak, state, api);
            })
            .map_err(|e| format!("set_rendering_notifier 失败: {e}"))?;
    }

    let timer = slint::Timer::default();
    {
        let core = core.clone();
        let ui_weak = ui.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(8),
            move || tick(&core, &ui_weak),
        );
    }

    ui.run().map_err(|e| format!("Slint 事件循环失败: {e}"))?;
    timer.stop();
    // 先丢掉窗口句柄：窗口销毁时 Slint 会在 GL 上下文 current 的情况下回调
    // RenderingTeardown，mpv 的 render context 在那时候释放才是安全的。
    drop(ui);

    let c = core.try_borrow_mut().map_err(|e| e.to_string())?;
    Ok(c.report(file, hwdec_requested, window_size))
}

/// 内部状态。所有字段都在主线程（winit 的事件循环线程）上访问。
struct Core {
    opts: Options,
    start: Instant,
    gl: Option<Rc<GlContext>>,
    gl_info: Option<GlInfo>,
    session: Option<Rc<Session>>,
    target: Option<VideoTarget>,
    render_ns: Vec<u64>,
    readback_ns: Vec<u64>,
    readbacks: Vec<ReadbackRecord>,
    frames: usize,
    first_frame_at: Option<Instant>,
    last_frame_at: Option<Instant>,
    pending_report_swap: bool,
    pending_readback: Option<usize>,
    hwdec_current: Option<String>,
    pixelformat: Option<String>,
    video_size: Option<(u32, u32)>,
    loaded: bool,
    eof: bool,
    frame_drop: Option<i64>,
    decoder_frame_drop: Option<i64>,
    shutdown_requested_at: Option<Instant>,
    teardown_done: bool,
    quit_sent: bool,
    error: Option<String>,
    last_overlay: String,
    /// 渲染由谁驱动：合成器的帧回调（正常）还是 `Window::take_snapshot()`（兜底）。
    mode: RenderMode,
    cpu_at_start: f64,
    /// 最近一次 BeforeRendering 的时间，用来判断“合成器还给不给帧回调”。
    last_render_at: Instant,
    pending_snapshot_readback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    /// 正常路径：`request_redraw()` → 合成器帧回调 → winit RedrawRequested → 渲染整窗。
    Window,
    /// 兜底路径：会话锁屏 / 窗口不可见时合成器不再发帧回调，
    /// 改用 `slint::Window::take_snapshot()` 主动驱动一次完整渲染（渲染到离屏 FBO）。
    Snapshot,
}

/// 在第几帧做窗口回读（4K 2s 素材只有 60 帧，所以取样点要早）。
const READBACK_PLAN: [usize; 6] = [2, 10, 20, 32, 48, 70];

impl Core {
    fn new(opts: Options) -> Self {
        Self {
            opts,
            start: Instant::now(),
            gl: None,
            gl_info: None,
            session: None,
            target: None,
            render_ns: Vec::new(),
            readback_ns: Vec::new(),
            readbacks: Vec::new(),
            frames: 0,
            first_frame_at: None,
            last_frame_at: None,
            pending_report_swap: false,
            pending_readback: None,
            hwdec_current: None,
            pixelformat: None,
            video_size: None,
            loaded: false,
            eof: false,
            frame_drop: None,
            decoder_frame_drop: None,
            shutdown_requested_at: None,
            teardown_done: false,
            quit_sent: false,
            error: None,
            last_overlay: String::new(),
            mode: if std::env::var_os("SLINT_MPV_GL_FORCE_SNAPSHOT").is_some() {
                RenderMode::Snapshot
            } else {
                RenderMode::Window
            },
            cpu_at_start: cpu_seconds(),
            last_render_at: Instant::now(),
            pending_snapshot_readback: false,
        }
    }

    fn mode_label(&self) -> &'static str {
        match self.mode {
            RenderMode::Window => "window",
            RenderMode::Snapshot => "snapshot",
        }
    }

    fn report(&self, file: PathBuf, hwdec_requested: String, window_size: (u32, u32)) -> Report {
        let mut render_ns = self.render_ns.clone();
        let pct = stats::percentiles(&mut render_ns, &[50.0, 95.0]);
        let mut readback_ns = self.readback_ns.clone();
        let readback_pct = stats::percentiles(&mut readback_ns, &[50.0]);
        let duration_s = match (self.first_frame_at, self.last_frame_at) {
            (Some(a), Some(b)) if b > a => (b - a).as_secs_f64(),
            _ => 0.0,
        };
        let e2e_fps = if duration_s > 0.0 && self.frames > 1 {
            (self.frames - 1) as f64 / duration_s
        } else {
            0.0
        };
        // 计数在播放过程中采集（session 在 AfterRendering 里就被释放了，事后读不到）。
        let (frame_drop, decoder_frame_drop) = (self.frame_drop, self.decoder_frame_drop);
        Report {
            file,
            hwdec_requested,
            hwdec_current: self.hwdec_current.clone(),
            pixelformat: self.pixelformat.clone(),
            video_size: self.video_size,
            window_size,
            gl: self.gl_info.clone(),
            frames: self.frames,
            duration_s,
            e2e_fps,
            render_p50_us: pct[0] / 1000.0,
            render_p95_us: pct[1] / 1000.0,
            render_mean_us: stats::mean(&self.render_ns) / 1000.0,
            readbacks: self
                .readbacks
                .iter()
                .map(|r| ReadbackRecord {
                    frame: r.frame,
                    stats: r.stats,
                    path: r.path.clone(),
                    read_ns: r.read_ns,
                })
                .collect(),
            readback_p50_us: readback_pct[0] / 1000.0,
            frame_drop,
            decoder_frame_drop,
            loader_fallbacks: glproc::fallback_count(),
            loader_symbols: glproc::resolved_count(),
            render_mode: self.mode_label(),
            cpu_seconds: cpu_seconds() - self.cpu_at_start,
            wall_seconds: self.start.elapsed().as_secs_f64(),
        }
    }

    /// 排空 mpv 事件队列（不阻塞）。
    fn drain_events(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let player = session.player();
        while let Some(ev) = player.next_event(0.0) {
            match ev {
                Event::FileLoaded => self.loaded = true,
                Event::EndFile { error: 0 } => self.eof = true,
                Event::EndFile { error } => {
                    self.set_error(format!("播放中断: {}", crate::sys::error_string(error)));
                }
                Event::LogMessage {
                    prefix,
                    level,
                    text,
                } => {
                    if self.opts.verbose {
                        eprint!("[mpv/{prefix}][{level}] {text}");
                    }
                }
                Event::EndFileWithoutData => {
                    self.set_error("收到 MPV_EVENT_END_FILE 但 data 为空".to_string());
                }
                // idle=yes 下 EOF 之后还会来一个 idle（加载开始前也会来一个，不能当成 EOF）。
                Event::Idle if self.loaded => self.eof = true,
                _ => {}
            }
        }
    }

    /// 每次 tick 刷新那几个"事后要报出来"的属性。
    fn refresh_props(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let player = session.player();
        self.hwdec_current = player.get_string("hwdec-current");
        if let Some(v) = player.get_i64("frame-drop-count") {
            self.frame_drop = Some(v);
        }
        if let Some(v) = player.get_i64("decoder-frame-drop-count") {
            self.decoder_frame_drop = Some(v);
        }
        self.pixelformat = player.get_string("video-params/pixelformat");
        self.video_size = video_params_size(player).or(self.video_size);
    }

    fn set_error(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(message);
        }
    }

    fn overlay_text(&self) -> String {
        let hwdec = self.hwdec_current.as_deref().unwrap_or("-");
        let px = self.pixelformat.as_deref().unwrap_or("-");
        let size = match self.video_size {
            Some((w, h)) => format!("{w}x{h}"),
            None => "-".to_string(),
        };
        let last = self.readbacks.last().map(|r| r.stats);
        let pixels = match last {
            Some(s) => format!(
                "luma={:.0} var={:.0} nonblack={:.0}%",
                s.mean_luma,
                s.variance,
                s.non_black_ratio * 100.0
            ),
            None => "luma=- var=- nonblack=-".to_string(),
        };
        format!(
            "mpv+Slint  hwdec={hwdec}  pixfmt={px}  {size}  frames={}  {pixels}",
            self.frames
        )
    }

    /// 决定这一帧要不要做窗口回读。
    fn readback_due(&self, frame_index: usize) -> bool {
        self.readbacks.len() < READBACK_PLAN.len() && READBACK_PLAN.contains(&frame_index)
    }
}

/// `video-params/w` × `video-params/h`。
fn video_params_size(player: &crate::sys::Handle) -> Option<(u32, u32)> {
    let w = player.get_i64("video-params/w")?;
    let h = player.get_i64("video-params/h")?;
    if w <= 0 || h <= 0 {
        return None;
    }
    Some((w as u32, h as u32))
}

fn api_loader<'a>(api: &'a GraphicsAPI<'a>) -> Result<glproc::SlintLoader<'a>, String> {
    match api {
        GraphicsAPI::NativeOpenGL { get_proc_address } => Ok(*get_proc_address),
        other => Err(format!("Slint 后端不是 OpenGL（GraphicsAPI={other:?}）")),
    }
}

fn on_rendering_state(
    core: &Rc<RefCell<Core>>,
    ui: &slint::Weak<AppWindow>,
    state: RenderingState,
    api: &GraphicsAPI<'_>,
) {
    let result = match state {
        RenderingState::RenderingSetup => setup_gl_and_mpv(core, api),
        RenderingState::BeforeRendering => render_mpv_frame(core, ui, api),
        RenderingState::AfterRendering => after_rendering(core, ui),
        RenderingState::RenderingTeardown => {
            teardown(core, api);
            Ok(())
        }
        // RenderingState 是 #[non_exhaustive]
        _ => Ok(()),
    };
    if let Err(err) = result {
        let mut c = core.borrow_mut();
        c.set_error(err);
        c.quit_sent = true;
        drop(c);
        let _ = slint::quit_event_loop();
    }
}

/// 进程累计 CPU 时间（user+sys），秒。失败时返回 NaN。
fn cpu_seconds() -> f64 {
    // SAFETY: getrusage 写满一个 rusage 结构体是它的契约；struct 已零初始化。
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: 指针指向本函数栈上、调用期间有效的 rusage。
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return f64::NAN;
    }
    let user = usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1e6;
    let sys = usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1e6;
    user + sys
}

/// 打开一个 DRM render node（VAAPI zero-copy interop 要把它交给 mpv）。
///
/// 拿不到不是致命错误：软解、`vaapi-copy` 都不需要它，只是 `hwdec=vaapi` 会退回软解。
fn open_render_node() -> Result<Option<std::os::fd::OwnedFd>, String> {
    // 优先 RENDER_NODE 环境变量（方便在多 GPU 机器上指定），否则按 128..136 顺序试。
    let candidates: Vec<String> = match std::env::var("SLINT_MPV_GL_RENDER_NODE") {
        Ok(path) if !path.is_empty() => vec![path],
        _ => (128..136).map(|n| format!("/dev/dri/renderD{n}")).collect(),
    };
    for path in candidates {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => {
                eprintln!("[init] 用 {path} 作为 VAAPI interop 的 render node");
                return Ok(Some(std::os::fd::OwnedFd::from(file)));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                eprintln!("[init] 打不开 {path}: {err}");
                continue;
            }
        }
    }
    eprintln!("[init] 没有可用的 /dev/dri/renderD*（hwdec=vaapi 会退回软解）");
    Ok(None)
}

/// `RenderingSetup`：GL 上下文刚建好（current）。取 GL 身份、建 mpv 会话与 render context。
///
/// render context 必须在任何 VO 创建之前建好，所以 `loadfile` 必须排在这之后。
fn setup_gl_and_mpv(core: &Rc<RefCell<Core>>, api: &GraphicsAPI<'_>) -> Result<(), String> {
    let loader = api_loader(api)?;
    let scope = glproc::LoaderScope::new(loader);
    let gl = GlContext::load(loader)?;
    let info = gl.info();

    let (file, hwdec, verbose) = {
        let c = core.borrow();
        (c.opts.file.clone(), c.opts.hwdec.clone(), c.opts.verbose)
    };
    let mut extra: Vec<(&str, &str)> = Vec::new();
    let log_level = if verbose { "v" } else { "error" };
    if verbose {
        extra.push(("terminal", "yes"));
        extra.push(("msg-level", "all=v"));
    }
    let render_fd = open_render_node()?;
    let session = Session::open(&hwdec, &extra, log_level, render_fd)?;
    session.load(Path::new(&file))?;

    let mut c = core.borrow_mut();
    c.gl_info = Some(info);
    c.gl = Some(Rc::new(gl));
    c.session = Some(Rc::new(session));
    drop(scope);
    Ok(())
}

/// `BeforeRendering`：GL 上下文 current、back buffer 已清屏、Slint 场景还没画。
/// 在这里把 mpv 的新帧渲染进我们的纹理（尺寸变了就重建）。
fn render_mpv_frame(
    core: &Rc<RefCell<Core>>,
    ui: &slint::Weak<AppWindow>,
    api: &GraphicsAPI<'_>,
) -> Result<(), String> {
    let loader = api_loader(api)?;
    let _scope = glproc::LoaderScope::new(loader);

    let mut c = core.borrow_mut();
    if c.teardown_done {
        return Ok(());
    }
    c.last_render_at = Instant::now();
    let (Some(session), Some(gl)) = (c.session.clone(), c.gl.clone()) else {
        return Ok(());
    };
    // 只在新帧时渲染：render.h 明确说 update() 的 FRAME 位才是"有新帧"的信号，
    // 盲目每帧 render 会把同一帧重复渲染，也会让 p50 失去意义。
    let flags = session.renderer().update();
    if !GlRenderer::has_new_frame(flags) {
        return Ok(());
    }
    let Some((video_w, video_h)) = video_params_size(session.player()) else {
        return Ok(());
    };

    let recreate = c
        .target
        .as_ref()
        .is_none_or(|t| t.width() != video_w || t.height() != video_h);
    if recreate {
        if let Some(old) = c.target.take() {
            gl.destroy_video_target(old);
        }
        let target = gl.create_video_target(video_w, video_h)?;
        let texture_id =
            NonZeroU32::new(target.texture_id()).ok_or_else(|| "纹理名为 0".to_string())?;
        // SAFETY: 纹理由同一 GL 上下文创建，且只在本窗口（同一上下文）里使用。
        let image = unsafe {
            BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                texture_id,
                IntSize::new(video_w, video_h),
            )
            // mpv 渲染进 FBO 时是"正常方向"，作为纹理采样时原点在左下。
            .origin(BorrowedOpenGLTextureOrigin::BottomLeft)
            .build()
        };
        c.target = Some(target);
        if let Some(ui) = ui.upgrade() {
            ui.set_frame(image);
        }
    }
    let target = c.target.as_ref().ok_or("纹理没建起来")?;

    let saved = gl.save_state();
    let started = Instant::now();
    let result = session.renderer().render_into(&target.sys_target()?);
    let elapsed = started.elapsed();
    gl.restore_state(&saved);
    result?;

    let now = Instant::now();
    if c.first_frame_at.is_none() {
        c.first_frame_at = Some(now);
    }
    c.last_frame_at = Some(now);
    c.frames += 1;
    c.render_ns.push(elapsed.as_nanos() as u64);
    c.pending_report_swap = true;
    let frame_index = c.frames;
    if c.readback_due(frame_index) {
        c.pending_readback = Some(frame_index);
    }
    Ok(())
}

/// `AfterRendering`：Slint 已经把整帧画进 back buffer（还没 swap）。
/// 这里做两件事：告诉 mpv 这一帧已经上屏；按计划回读窗口像素做验证。
fn after_rendering(core: &Rc<RefCell<Core>>, ui: &slint::Weak<AppWindow>) -> Result<(), String> {
    let mut c = core.borrow_mut();
    if c.pending_report_swap {
        if let Some(session) = c.session.clone() {
            session.renderer().report_swap();
        }
        c.pending_report_swap = false;
    }
    if c.pending_snapshot_readback {
        // take_snapshot() 模式下像素已经在 AfterRendering 里读完了，这里只清标记。
        c.pending_snapshot_readback = false;
    }
    if let Some(frame_index) = c.pending_readback.take() {
        let gl = c.gl.clone().ok_or("回读时 GL 已释放")?;
        let ui = ui.upgrade().ok_or("回读时窗口已销毁")?;
        let physical = ui.window().size();
        let started = Instant::now();
        let sample: FrameSample =
            gl.read_bound_framebuffer(physical.width, physical.height, true)?;
        let read_ns = started.elapsed().as_nanos() as u64;
        let path = match &c.opts.frames_out {
            Some(dir) => {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
                let path = dir.join(format!(
                    "frame-{frame_index:04}-{}x{}.png",
                    sample.width, sample.height
                ));
                crate::gl::write_png(&path, sample.width, sample.height, &sample.rgba)?;
                Some(path)
            }
            None => None,
        };
        eprintln!(
            "[readback] 第 {frame_index} 帧 {}x{}: luma={:.1} var={:.0} nonblack={:.1}% 回读耗时={:.2}ms",
            sample.width,
            sample.height,
            sample.stats.mean_luma,
            sample.stats.variance,
            sample.stats.non_black_ratio * 100.0,
            read_ns as f64 / 1e6,
        );
        c.readback_ns.push(read_ns);
        c.readbacks.push(ReadbackRecord {
            frame: frame_index,
            stats: sample.stats,
            path,
            read_ns,
        });
    }
    // 退出前在 GL 上下文还 current 的时候把 mpv render context 和 GL 对象放掉。
    if c.shutdown_requested_at.is_some() && !c.teardown_done {
        release_gpu_resources(&mut c);
    }
    Ok(())
}

fn release_gpu_resources(c: &mut Core) {
    if let Some(session) = c.session.take() {
        drop(session);
    }
    if let Some(target) = c.target.take() {
        if let Some(gl) = c.gl.clone() {
            gl.destroy_video_target(target);
        }
    }
    c.gl = None;
    c.teardown_done = true;
}

/// `RenderingTeardown`：窗口销毁 / 上下文即将销毁，但此刻仍是 current。
fn teardown(core: &Rc<RefCell<Core>>, api: &GraphicsAPI<'_>) {
    let scope = api_loader(api).ok().map(glproc::LoaderScope::new);
    let mut c = core.borrow_mut();
    release_gpu_resources(&mut c);
    drop(c);
    drop(scope);
}

/// 每 8ms 跑一次的“主循环”：排空事件、请求重绘、更新覆盖文字、判断退出。
fn tick(core: &Rc<RefCell<Core>>, ui: &slint::Weak<AppWindow>) {
    let Some(ui) = ui.upgrade() else {
        return;
    };

    // 先在借用里做决策，动作（重绘 / 快照 / 退出）留到借用结束后再执行：
    // take_snapshot() 会同步跑一整轮渲染（含 notifier），notifier 里还要 borrow_mut，
    // 不能和这里同时持有可变借用。
    #[derive(Default)]
    struct Actions {
        redraw: bool,
        snapshot: bool,
        quit: bool,
    }
    let mut actions = Actions::default();
    {
        let mut c = core.borrow_mut();
        if c.quit_sent {
            return;
        }
        if let Some(err) = c.error.clone() {
            eprintln!("错误: {err}");
            c.quit_sent = true;
            drop(c);
            let _ = slint::quit_event_loop();
            return;
        }
        c.drain_events();
        c.refresh_props();

        // mpv 的 update 回调置位 → 请求一次重绘，让 BeforeRendering 去渲染新帧。
        let raised = c
            .session
            .as_ref()
            .and_then(|s| s.renderer().signal().map(|signal| signal.take()))
            .unwrap_or(false);
        if raised {
            actions.redraw = true;
        }

        let overlay = c.overlay_text();
        let overlay_changed = overlay != c.last_overlay;
        if overlay_changed {
            c.last_overlay = overlay.clone();
            ui.set_overlay(overlay.into());
        }

        // 合成器（KWin/Wayland）在会话锁屏、窗口不可见等情况下不再发帧回调，
        // 于是 request_redraw() 永远等不到 RedrawRequested。检测到“请求了但长时间没有任何
        // 一次渲染”就切到 take_snapshot() 主动驱动，保证 Gate 数据仍然可采。
        // 注意 `c.frames > 0`：启动时第一帧还没渲染出来，`last_render_at` 自然已经"超时"，
        // 缺这个条件会在开窗慢的桌面上误判成"合成器不给回调"，然后永久锁在 snapshot 模式。
        if c.mode == RenderMode::Window
            && c.shutdown_requested_at.is_none()
            && c.frames > 0
            && c.start.elapsed() > Duration::from_millis(500)
            && c.last_render_at.elapsed() > Duration::from_millis(400)
        {
            eprintln!(
                "[warn] 500ms+ 内合成器没有给出任何一帧回调（会话锁屏？窗口不可见？），\
                 改用 take_snapshot() 驱动渲染"
            );
            c.mode = RenderMode::Snapshot;
        }
        if c.mode == RenderMode::Snapshot
            && c.shutdown_requested_at.is_none()
            && (raised
                || overlay_changed
                || c.frames == 0
                || c.last_render_at.elapsed() > Duration::from_millis(100))
        {
            actions.snapshot = true;
        }

        let elapsed = c.start.elapsed().as_secs_f64();
        let eof_reached_early = c.eof && elapsed >= 1.0;
        if c.shutdown_requested_at.is_none() && (elapsed >= c.opts.seconds || eof_reached_early) {
            c.shutdown_requested_at = Some(Instant::now());
            actions.redraw = true;
            // 窗口模式拿不到帧回调时，退出前的资源释放要靠快照/兜底路径触发。
            actions.snapshot = c.mode == RenderMode::Snapshot;
        }
        let finished = c.teardown_done && c.shutdown_requested_at.is_some();
        // 兜底：请求退出后 2 秒还没走到 teardown（例如窗口不再重绘），也要退出去，
        // 不能留下挂死的进程。
        let stuck = c
            .shutdown_requested_at
            .is_some_and(|at| at.elapsed() > Duration::from_secs(2));
        if finished || stuck {
            c.quit_sent = true;
            actions.quit = true;
        }
    }

    if actions.redraw {
        ui.window().request_redraw();
    }
    if actions.snapshot && !actions.quit {
        // 主动渲染一轮（渲染到离屏 FBO），notifier 会照常跑
        // RenderingSetup / BeforeRendering / AfterRendering。
        core.borrow_mut().pending_snapshot_readback = true;
        if let Err(err) = ui.window().take_snapshot() {
            let mut c = core.borrow_mut();
            c.pending_snapshot_readback = false;
            c.set_error(format!("take_snapshot 失败: {err}"));
            c.quit_sent = true;
            drop(c);
            let _ = slint::quit_event_loop();
            return;
        }
        core.borrow_mut().pending_snapshot_readback = false;
    }
    if actions.quit {
        let _ = slint::quit_event_loop();
    }
}
