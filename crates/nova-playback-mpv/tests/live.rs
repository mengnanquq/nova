// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 真机集成测试：真实 Jellyfin 直链 + 真实 GPU（EGL surfaceless 上下文）。
//!
//! 这些用例需要：
//!
//! - 一个跑着的 Jellyfin：`scripts/jellyfin-dev.sh up`，然后
//!   `export NOVA_JELLYFIN_URL=http://127.0.0.1:18096`（另外两个变量有默认值）。
//! - 一块能做 VAAPI 的 GPU（没有也能跑，只是 `hwdec-current` 会是 `no`）。
//! - **不需要**显示会话：GL 上下文用 `libEGL` 的 surfaceless 平台创建（`dlopen` + `dlsym`，
//!   不链接 libEGL/libGL）。
//!
//! 运行：
//!
//! ```bash
//! ./scripts/jellyfin-dev.sh up
//! export NOVA_JELLYFIN_URL=http://127.0.0.1:18096
//! cargo test -p nova-playback-mpv --test live -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 没有 `NOVA_JELLYFIN_URL` 时用例直接跳过（不失败），与 `docs/testing.md` 的策略一致。
//! 必须 `--test-threads=1`：同一 DeviceId 再次登录会吊销旧令牌（Jellyfin 12 的行为）。
//!
//! 整个测试的音轨输出固定为 `ao=null`：**绝不发出声音**。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use nova_playback::{EngineEvent, OpenRequest, PlaybackEngine, SubtitleSelection};
use nova_playback_mpv::{EngineOptions, GlFunctions, MpvEngine};

/// 登录用的设备标识（固定，避免每次跑都创建新的会话）。
const DEVICE_ID: &str = "nova-playback-mpv-live";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// 服务端环境；没有 `NOVA_JELLYFIN_URL` 就返回 `None`（用例跳过）。
struct Server {
    base: String,
    user: String,
    password: String,
}

impl Server {
    fn from_env() -> Option<Self> {
        Some(Self {
            base: env("NOVA_JELLYFIN_URL")?,
            user: env("NOVA_JELLYFIN_USER").unwrap_or_else(|| "nova".to_string()),
            password: env("NOVA_JELLYFIN_PASS").unwrap_or_else(|| "nova-dev-pass".to_string()),
        })
    }

    fn authorization(&self) -> String {
        format!(
            "MediaBrowser Client=\"NovaLive\", Device=\"test\", DeviceId=\"{DEVICE_ID}\", Version=\"0.1.0\""
        )
    }

    fn authenticate(&self) -> Result<String, String> {
        let body = format!(
            "{{\"Username\":\"{}\",\"Pw\":\"{}\"}}",
            self.user, self.password
        );
        let (status, response) = http(
            &self.base,
            "POST",
            "/Users/AuthenticateByName",
            &[
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Authorization".to_string(), self.authorization()),
            ],
            Some(&body),
        )?;
        if status != 200 {
            return Err(format!("登录失败: HTTP {status}"));
        }
        json_field(&response, "AccessToken").ok_or_else(|| "登录响应里没有 AccessToken".to_string())
    }

    /// 找一个带音轨的 1080p60 素材（没有就退回第一个条目）。
    fn pick_item(&self, token: &str) -> Result<(String, String), String> {
        let (status, response) = http(
            &self.base,
            "GET",
            "/Items?Recursive=true&IncludeItemTypes=Movie&Fields=MediaSources",
            &[("Authorization".to_string(), token_header(token))],
            None,
        )?;
        if status != 200 {
            return Err(format!("列条目失败: HTTP {status}"));
        }
        // 响应形如 {"Items":[{"Name":"testsrc_1080p60_5s",…,"Id":"…"},…]}。
        // 逐条对象抽取（Jellyfin 的字段顺序不保证），不做完整 JSON 解析。
        let mut pairs: Vec<(String, String)> = Vec::new();
        for chunk in response.split("},") {
            if let (Some(name), Some(id)) = (json_field(chunk, "Name"), json_field(chunk, "Id")) {
                pairs.push((id, name));
            }
        }
        if let Some(found) = pairs.iter().find(|(_, name)| name.contains("1080p60")) {
            return Ok(found.clone());
        }
        let fallback = pairs.into_iter().next();
        fallback.ok_or_else(|| "服务端没有任何条目（先跑 scripts/jellyfin-dev.sh up）".to_string())
    }

    fn stream_url(&self, item: &str) -> String {
        format!("{}/Videos/{item}/stream?static=true", self.base)
    }
}

fn token_header(token: &str) -> String {
    format!("MediaBrowser Token=\"{token}\"")
}

/// 极简 HTTP/1.1 客户端（测试用；避免为了两个请求引入依赖）。
fn http(
    base: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> Result<(u16, String), String> {
    let trimmed = base.trim_start_matches("http://");
    let host_port = trimmed.split('/').next().unwrap_or(trimmed);
    let addr = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:80")
    };
    let mut stream =
        TcpStream::connect(&addr).map_err(|error| format!("连接 {addr} 失败: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        request.push_str("Content-Type: application/json\r\n");
        request.push_str("\r\n");
        request.push_str(body);
    } else {
        request.push_str("\r\n");
    }
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("写请求失败: {error}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|error| format!("读响应失败: {error}"))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "响应状态行无法解析".to_string())?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

/// 取一个 JSON 字符串字段（够用就行的解析：不引入 serde）。
fn json_field(json: &str, name: &str) -> Option<String> {
    let needle = format!("\"{name}\":\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// EGL surfaceless 上下文（不需要显示会话）
// ---------------------------------------------------------------------------

/// 本文件里唯一的 unsafe 集中区：`dlopen`/`dlsym` 与 EGL 调用。
mod egl {
    use std::ffi::{CString, c_char, c_int, c_void};

    const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
    const EGL_OPENGL_API: u32 = 0x30A2;
    const EGL_CONTEXT_MAJOR_VERSION: c_int = 0x3098;
    const EGL_NONE: c_int = 0x3038;

    type EglGetProcAddressRaw = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type EglGetPlatformDisplay =
        unsafe extern "C" fn(u32, *mut c_void, *const c_int) -> *mut c_void;
    type EglInitialize = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> u32;
    type EglBindApi = unsafe extern "C" fn(u32) -> u32;
    type EglCreateContext =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_int) -> *mut c_void;
    type EglMakeCurrent =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32;

    /// 一个活着的无窗口 GL 上下文；`Drop` 里不做清理（测试进程直接退出）。
    pub struct HeadlessGl {
        /// 给 `GlFunctions` 用的 `eglGetProcAddress`（真实函数地址，不链接 libEGL）。
        pub get_proc_address: unsafe extern "C" fn(*const c_char) -> *mut c_void,
        _lib: *mut c_void,
        _display: *mut c_void,
        _context: *mut c_void,
    }

    fn symbol(lib: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let name = CString::new(name).map_err(|error| error.to_string())?;
        // SAFETY: lib 来自 dlopen（非 NULL），name 是 NUL 结尾字符串；dlsym 返回符号地址或
        // NULL，不转移所有权。
        let ptr = unsafe { libc::dlsym(lib, name.as_ptr()) };
        if ptr.is_null() {
            return Err(format!(
                "符号 {} 解析失败: {}",
                name.to_string_lossy(),
                dlerror()
            ));
        }
        Ok(ptr)
    }

    fn dlerror() -> String {
        // SAFETY: dlerror 返回线程局部的静态字符串或 NULL。
        let ptr = unsafe { libc::dlerror() };
        if ptr.is_null() {
            return "无错误信息".to_string();
        }
        // SAFETY: 非空指针指向 NUL 结尾的静态字符串。
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    /// 建一个 EGL surfaceless + OpenGL 的上下文并 make current。
    pub fn create() -> Result<HeadlessGl, String> {
        // SAFETY: dlopen 加载已安装的 libEGL；失败返回 NULL。
        let lib =
            unsafe { libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if lib.is_null() {
            return Err(format!("dlopen(libEGL.so.1) 失败: {}", dlerror()));
        }
        let gpa_ptr = symbol(lib, "eglGetProcAddress")?;
        let initialize_ptr = symbol(lib, "eglInitialize")?;
        let bind_api_ptr = symbol(lib, "eglBindAPI")?;
        let create_context_ptr = symbol(lib, "eglCreateContext")?;
        let make_current_ptr = symbol(lib, "eglMakeCurrent")?;

        // SAFETY: 符号来自 libEGL，签名按 EGL 1.5 规范（EGLAPIENTRY = default calling
        // convention on Unix）。
        let get_proc_address: EglGetProcAddressRaw = unsafe { std::mem::transmute(gpa_ptr) };
        // SAFETY: 同上。
        let initialize: EglInitialize = unsafe { std::mem::transmute(initialize_ptr) };
        // SAFETY: 同上。
        let bind_api: EglBindApi = unsafe { std::mem::transmute(bind_api_ptr) };
        // SAFETY: 同上。
        let create_context: EglCreateContext = unsafe { std::mem::transmute(create_context_ptr) };
        // SAFETY: 同上。
        let make_current: EglMakeCurrent = unsafe { std::mem::transmute(make_current_ptr) };

        // eglGetPlatformDisplayEXT 不是 libEGL 的导出符号，只能通过 eglGetProcAddress 拿。
        // SAFETY: get_proc_address 是刚解析出来的 eglGetProcAddress；参数是静态 C 字符串。
        let gpd_ptr = unsafe { get_proc_address(c"eglGetPlatformDisplayEXT".as_ptr()) };
        if gpd_ptr.is_null() {
            return Err("拿不到 eglGetPlatformDisplayEXT（Mesa EGL 才有）".to_string());
        }
        // SAFETY: 见上，签名按 EGL_EXT_platform_base。
        let get_platform_display: EglGetPlatformDisplay = unsafe { std::mem::transmute(gpd_ptr) };

        // SAFETY: 三个参数都是规范允许的值；EGL_DEFAULT_DISPLAY = NULL。
        let display = unsafe {
            get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if display.is_null() {
            return Err("eglGetPlatformDisplayEXT 返回 NULL".to_string());
        }
        let mut major: c_int = 0;
        let mut minor: c_int = 0;
        // SAFETY: display 非空，major/minor 是可写的栈变量。
        if unsafe { initialize(display, &mut major, &mut minor) } != 1 {
            return Err("eglInitialize 失败".to_string());
        }
        // SAFETY: 只要常量参数。
        if unsafe { bind_api(EGL_OPENGL_API) } != 1 {
            return Err("eglBindAPI(EGL_OPENGL_API) 失败".to_string());
        }
        let attributes = [EGL_CONTEXT_MAJOR_VERSION, 3, EGL_NONE];
        // surfaceless 平台没有 config，用 EGL_NO_CONFIG_KHR（NULL）+ EGL_EXT_no_config_context。
        // SAFETY: 属性数组以 EGL_NONE 结尾；config 传 NULL（该扩展允许）。
        let context = unsafe {
            create_context(
                display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                attributes.as_ptr(),
            )
        };
        if context.is_null() {
            return Err("eglCreateContext 失败".to_string());
        }
        // SAFETY: 上下文已创建；EGL_NO_SURFACE = NULL 表示不绑定任何 surface。
        if unsafe { make_current(display, std::ptr::null_mut(), std::ptr::null_mut(), context) }
            != 1
        {
            return Err("eglMakeCurrent 失败".to_string());
        }
        Ok(HeadlessGl {
            get_proc_address,
            _lib: lib,
            _display: display,
            _context: context,
        })
    }
}

/// 建无窗口 GL 上下文并装载入口点。
///
/// 返回值里的 `HeadlessGl` 必须活到测试结束：mpv 的 render context 一直在用它。
fn gl_functions() -> (GlFunctions, egl::HeadlessGl) {
    let headless = egl::create().expect("EGL surfaceless 上下文创建失败");
    let functions = GlFunctions::from_egl_get_proc_address(headless.get_proc_address)
        .expect("装载 GL 入口点失败");
    (functions, headless)
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 驱动一个 engine future。
///
/// 用 tokio 的 current-thread runtime：`http` 外挂字幕下载走 `tokio::net`，必须在
/// tokio reactor 里跑（真实产品里 `nova-app` 本来就是 tokio 运行时）。
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("建 tokio runtime 失败")
        .block_on(future)
}

/// 把事件流里已有的东西全部取出来（非阻塞）。
fn drain(engine: &mut MpvEngine) -> Vec<EngineEvent> {
    let waker = futures::task::noop_waker();
    let mut context = Context::from_waker(&waker);
    let stream = engine.events();
    let mut events = Vec::new();
    while let Poll::Ready(Some(event)) = Pin::new(&mut *stream).poll_next(&mut context) {
        events.push(event);
    }
    let _ = stream; // 借用结束后才能再借用 engine
    events
}

#[test]
#[ignore = "需要 NOVA_JELLYFIN_URL 与真实 GPU；见文件头的运行命令"]
fn live_first_frame_hwdec_tracks_seek() {
    let Some(server) = Server::from_env() else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL");
        return;
    };
    let token = server.authenticate().expect("登录失败");
    let (item, name) = server.pick_item(&token).expect("找条目失败");
    eprintln!("素材：{name}（{item}）");
    let url = server.stream_url(&item);
    // 故意用含逗号的完整 Authorization 形式：这正是 Jellyfin 文档里的写法，也顺带验证
    // http-header-fields 的逗号转义。
    let authorization = format!(
        "MediaBrowser Client=\"NovaLive\", Device=\"test\", DeviceId=\"{DEVICE_ID}\", Version=\"0.1.0\", Token=\"{token}\""
    );

    let (functions, _gl) = gl_functions();
    let mut engine = MpvEngine::new(EngineOptions::new().audio_output("null")).expect("建引擎失败");
    engine.attach_gl(functions).expect("attach_gl 失败");

    let started = Instant::now();
    block_on(
        engine.open(
            OpenRequest::new(url)
                .with_header("Authorization", authorization)
                .with_subtitle(SubtitleSelection::Off),
        ),
    )
    .expect("open 失败");

    let mut first_frame: Option<Instant> = None;
    let mut tracks_video = 0usize;
    let mut tracks_audio = 0usize;
    let mut max_position = Duration::ZERO;
    let mut dropped = 0u64;
    let mut frames = 0u64;
    let play_deadline = Duration::from_secs(3);
    let timeout = Instant::now() + Duration::from_secs(25);

    while Instant::now() < timeout {
        for event in drain(&mut engine) {
            match event {
                EngineEvent::FirstFrame if first_frame.is_none() => {
                    first_frame = Some(Instant::now());
                    eprintln!(
                        "首帧到达：open 之后 {:.1} ms",
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                }
                EngineEvent::TracksChanged(set) => {
                    tracks_video = set.videos().count();
                    tracks_audio = set.audio().count();
                    eprintln!(
                        "轨道：视频 {} 条、音频 {} 条、字幕 {} 条",
                        tracks_video,
                        tracks_audio,
                        set.subtitles().count()
                    );
                }
                EngineEvent::PositionChanged(position) => {
                    max_position = max_position.max(position);
                }
                EngineEvent::DroppedFrames(count) => dropped = count,
                EngineEvent::Buffering(state) => eprintln!("缓冲：{state}"),
                EngineEvent::Ended => eprintln!("播放到结尾"),
                EngineEvent::Error(error) => panic!("引擎报错: {error:?}"),
                _ => {}
            }
        }
        if engine.needs_redraw() {
            match engine.render(0, 1280, 720) {
                Ok(true) => frames += 1,
                Ok(false) => {}
                Err(error) => panic!("render 失败: {error:?}"),
            }
        }
        if first_frame.is_some() && max_position >= play_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    assert!(first_frame.is_some(), "20 秒内没有首帧");
    assert!(frames > 1, "只渲染了 {frames} 帧");
    assert_eq!(tracks_video, 1, "视频轨数量不对");
    assert!(
        max_position >= Duration::from_millis(2500),
        "位置没有推进：{max_position:?}"
    );
    assert!(engine.texture().is_some(), "纹理不可用");
    let latency = engine.first_frame_latency().expect("首帧耗时缺失");
    let diagnostics = EngineDiagnosticsView::new(engine.diagnostics());
    eprintln!("首帧耗时：{:.1} ms", latency.as_secs_f64() * 1000.0);
    eprintln!("渲染帧数：{frames}，累计丢帧：{dropped}");
    eprintln!("起播时序：{:?}", engine.start_trace());
    eprintln!(
        "诊断：hwdec-current={:?} decoder={:?} backend={:?}",
        diagnostics.active_hwdec, diagnostics.video_decoder, diagnostics.render_backend
    );
    // ADR-0001 坑 1：本机应该真的是 vaapi（渲染上下文里传了 DRM render node）。
    if cfg!(target_os = "linux") {
        assert_eq!(
            diagnostics.active_hwdec.as_deref(),
            Some("vaapi"),
            "硬解没有生效：hwdec-current={:?}（ADR-0001 坑 1：DRM render node 没传到？）",
            diagnostics.active_hwdec
        );
        assert!(!diagnostics.is_hwdec_degraded(), "诊断判定为硬解降级");
    }
    assert!(tracks_audio >= 1, "这个素材应该有音轨（用来验证音轨枚举）");

    // 外挂字幕：写一个真的 srt，`sub-add` 之后轨道集里必须出现一条字幕轨。
    let subtitle_dir = tempfile::Builder::new()
        .prefix("nova-live-subs-")
        .tempdir()
        .expect("建临时目录失败");
    let srt = subtitle_dir.path().join("live.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:02,000\n外挂字幕测试\n").expect("写 srt 失败");
    block_on(engine.select_subtitle(SubtitleSelection::External {
        path: srt.to_string_lossy().into_owned(),
    }))
    .expect("加载外挂字幕失败");
    let mut external_subtitle_id: Option<u32> = None;
    let subtitle_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < subtitle_deadline {
        for event in drain(&mut engine) {
            if let EngineEvent::TracksChanged(set) = event {
                if let Some(track) = set.subtitles().find(|track| track.is_external) {
                    external_subtitle_id = Some(track.index);
                }
            }
        }
        if engine.needs_redraw() {
            let _ = engine.render(0, 1280, 720);
        }
        if external_subtitle_id.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    let external_subtitle_id = external_subtitle_id.expect("sub-add 之后轨道集里没有外挂字幕");
    eprintln!("外挂字幕轨 id={external_subtitle_id}");
    // 用内嵌/已加载轨的序号走 sid（不重启播放）；再关掉。
    block_on(engine.select_subtitle(SubtitleSelection::Track(external_subtitle_id)))
        .expect("按序号选字幕失败");
    block_on(engine.select_subtitle(SubtitleSelection::Off)).expect("关字幕失败");
    block_on(engine.set_subtitle_delay(Duration::from_millis(500))).expect("设置字幕延迟失败");
    engine
        .set_subtitle_delay_seconds(-0.25)
        .expect("负字幕延迟失败");

    // http 外挂字幕：验证真的能下载 + 落盘 + 交给 mpv（内容是 JSON 不是字幕，
    // mpv 会拒绝解析，这不是本用例的断言点；断言点是"下载没有把错误抛给上层"）。
    block_on(engine.select_subtitle(SubtitleSelection::External {
        path: format!("{}/System/Info/Public", server.base),
    }))
    .expect("http 外挂字幕下载失败");

    // 帧 watchdog（ADR-0001 坑 2）：播放中 700 ms 不调 render()，needs_redraw 必须自己变 true。
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        engine.needs_redraw(),
        "播放中超过 500ms 没有渲染，needs_redraw 仍为假（合成器帧回调缺失时会卡死）"
    );

    // seek 到中点（素材 5 秒）：位置必须真的跳回中点附近（此前已经播到 ~3s）。
    // 注意 mpv 对 http 流做绝对 seek 会重开 demuxer，位置在 ~100–300 ms 之后才更新，
    // 所以这里等"落在 2.4–2.8s 之间的位置事件"，而不是"位置变了"。
    let before = max_position;
    block_on(engine.seek(Duration::from_millis(2500))).expect("seek 失败");
    let seek_deadline = Instant::now() + Duration::from_secs(10);
    let mut landed: Option<Duration> = None;
    while Instant::now() < seek_deadline {
        for event in drain(&mut engine) {
            if let EngineEvent::PositionChanged(position) = event {
                if (Duration::from_millis(2400)..Duration::from_millis(2800)).contains(&position) {
                    landed = Some(position);
                }
            }
        }
        if engine.needs_redraw() {
            let _ = engine.render(0, 1280, 720);
        }
        if landed.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    eprintln!("seek 目标 2500ms：seek 前 {before:?}，seek 后落到 {landed:?}");
    assert!(
        landed.is_some(),
        "seek 之后 10 秒内没有出现中点附近的位置事件（seek 前 {before:?}）"
    );

    block_on(engine.close()).expect("close 失败");
}

/// `Drop` 路径：不调用 `close()` 直接丢弃引擎，不允许 panic、不允许留下 mpv 线程。
#[test]
#[ignore = "需要 NOVA_JELLYFIN_URL 与真实 GPU；见文件头的运行命令"]
fn live_drop_without_close_is_clean() {
    let Some(server) = Server::from_env() else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL");
        return;
    };
    let token = server.authenticate().expect("登录失败");
    let (item, _name) = server.pick_item(&token).expect("找条目失败");
    let url = server.stream_url(&item);

    {
        let (functions, _gl) = gl_functions();
        let mut engine =
            MpvEngine::new(EngineOptions::new().audio_output("null")).expect("建引擎失败");
        engine.attach_gl(functions).expect("attach_gl 失败");
        block_on(
            engine.open(OpenRequest::new(url).with_header("Authorization", token_header(&token))),
        )
        .expect("open 失败");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let _ = drain(&mut engine);
            if engine.needs_redraw() {
                let _ = engine.render(0, 640, 360);
            }
            if engine.texture().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        assert!(engine.texture().is_some(), "10 秒内没有出画面");
        // 故意不 close()：Drop 里必须按 render context → worker → mpv core 的顺序释放。
    }

    // 给 mpv 的 core 线程一点退出时间；如果 Drop 里顺序错了，这里通常会崩或卡住。
    std::thread::sleep(Duration::from_millis(300));
    assert!(std::thread::available_parallelism().is_ok());
}

/// ADR-0001 坑 1 的端到端验证：请求值与 `hwdec-current` 不一致时，诊断必须自己说出来。
///
/// 这里故意请求 `auto`（mpv 在本机实际给出 `vaapi`），于是 `is_hwdec_degraded()` 必须为真、
/// `hwdec_notice()` 必须给出界面文案。真实"静默退回软解"的情形（请求 vaapi、实际 no）
/// 需要把 DRM 参数拿掉才能复现，那种情况走的是同一条比对逻辑。
#[test]
#[ignore = "需要 NOVA_JELLYFIN_URL 与真实 GPU；见文件头的运行命令"]
fn live_hwdec_mismatch_is_reported() {
    let Some(server) = Server::from_env() else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL");
        return;
    };
    let token = server.authenticate().expect("登录失败");
    let (item, _name) = server.pick_item(&token).expect("找条目失败");
    let url = server.stream_url(&item);

    let (functions, _gl) = gl_functions();
    let mut engine = MpvEngine::new(EngineOptions::new().audio_output("null").hwdec("auto"))
        .expect("建引擎失败");
    engine.attach_gl(functions).expect("attach_gl 失败");
    block_on(engine.open(OpenRequest::new(url).with_header("Authorization", token_header(&token))))
        .expect("open 失败");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut first_frame = false;
    while Instant::now() < deadline {
        for event in drain(&mut engine) {
            if matches!(event, EngineEvent::FirstFrame) {
                first_frame = true;
            }
        }
        if engine.needs_redraw() {
            let _ = engine.render(0, 640, 360);
        }
        if first_frame && engine.diagnostics().active_hwdec.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    let diagnostics = engine.diagnostics();
    eprintln!(
        "请求 {:?}，实际 {:?}，降级={}，提示={:?}",
        diagnostics.requested_hwdec,
        diagnostics.active_hwdec,
        diagnostics.is_hwdec_degraded(),
        diagnostics.hwdec_notice()
    );
    assert!(first_frame, "没有首帧");
    assert!(
        diagnostics.is_hwdec_degraded(),
        "请求 auto、实际 {:?}，诊断应该判定为降级",
        diagnostics.active_hwdec
    );
    let notice = diagnostics.hwdec_notice().expect("降级时必须给出界面提示");
    assert!(notice.contains("硬件解码未生效"), "{notice}");
    let _ = block_on(engine.close());
}

/// 诊断信息的展示辅助（测试里读字段用）。
struct EngineDiagnosticsView {
    active_hwdec: Option<String>,
    video_decoder: Option<String>,
    render_backend: Option<String>,
    degraded: bool,
}

impl EngineDiagnosticsView {
    fn new(diagnostics: nova_playback::EngineDiagnostics) -> Self {
        Self {
            active_hwdec: diagnostics.active_hwdec.clone(),
            video_decoder: diagnostics.video_decoder.clone(),
            render_backend: diagnostics.render_backend.clone(),
            degraded: diagnostics.is_hwdec_degraded(),
        }
    }

    fn is_hwdec_degraded(&self) -> bool {
        self.degraded
    }
}

/// 一次 `open` 之后必须真的开始播放：**会话 1 退出（`pause`）→ 会话 2 起播**不能停在暂停。
///
/// 复现序列与产品一致（体检 R4-1）：`open` → 出帧 → `Player::exit` 会 `pause`（离开播放页
/// 就该停声，引擎不销毁）→ 再 `open` 同一素材的续播点。`pause` 是 mpv 的 **core 级**属性，
/// `loadfile` 不会复位它：修复前第二次 `open` 之后位置冻结在续播点、`core-idle` 恒为 yes、
/// 再也拿不到帧（`FirstFrame` 不放，上层 15 秒后报卡死），按一次 Space×2 才救活。
///
/// 素材用 ffmpeg 在临时目录里现生成，不需要 Jellyfin；但仍然 `#[ignore]`：它要真实 libmpv +
/// 真实 GPU（EGL surfaceless 上下文）与 ffmpeg，CI / 容器里都不具备（同 `docs/testing.md`
/// 的"真机用例默认跳过"策略）。
#[test]
#[ignore = "需要真实 libmpv + GPU（EGL surfaceless）与 ffmpeg；不需要 Jellyfin"]
fn live_reopen_after_exit_pause_starts_playing() {
    let dir = tempfile::Builder::new()
        .prefix("nova-reopen-")
        .tempdir()
        .expect("建临时目录失败");
    let media = dir.path().join("clip.mp4");
    let generated = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=8:size=320x240:rate=15",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&media)
        .status();
    if !matches!(generated, Ok(status) if status.success()) {
        eprintln!("跳过：没有可用的 ffmpeg，无法生成测试素材");
        return;
    }
    let url = media.to_string_lossy().into_owned();

    let (functions, _gl) = gl_functions();
    let mut engine = MpvEngine::new(EngineOptions::new().audio_output("null")).expect("建引擎失败");
    engine.attach_gl(functions).expect("attach_gl 失败");

    // 会话 1：正常起播（第一次会话里 mpv 的 pause 初值是 no）。
    block_on(engine.open(OpenRequest::new(url.clone()))).expect("第一次 open 失败");
    let mut first = Pumped::default();
    pump(&mut engine, &mut first, Duration::from_secs(20), |state| {
        state.first_frames > 0 && state.max_position >= Duration::from_millis(800)
    });
    eprintln!("会话 1：{first:?}");
    assert!(first.first_frames > 0, "会话 1 没有出帧：{first:?}");
    assert!(
        first.max_position >= Duration::from_millis(800),
        "会话 1 位置没有推进：{first:?}"
    );

    // 会话 1 结束：产品里 `Player::exit` 就是这么做的（引擎与渲染上下文都留着）。
    block_on(engine.pause()).expect("pause 失败");
    let mut paused = Pumped::default();
    pump(&mut engine, &mut paused, Duration::from_millis(500), |_| {
        false
    });
    let paused_at = paused.max_position;
    pump(&mut engine, &mut paused, Duration::from_millis(500), |_| {
        false
    });
    eprintln!("暂停：{:?} → {:?}", paused_at, paused.max_position);
    assert!(
        paused.max_position <= paused_at + Duration::from_millis(100),
        "pause 之后位置还在推进：{paused_at:?} → {:?}",
        paused.max_position
    );

    // 会话 2：同一个素材 + 续播点，走 `Player::begin` → `open` 的同一条路径。
    block_on(
        engine.open(OpenRequest::new(url.clone()).with_resume_at(Duration::from_millis(1000))),
    )
    .expect("第二次 open 失败");
    let mut second = Pumped::default();
    pump(&mut engine, &mut second, Duration::from_secs(15), |state| {
        state.first_frames > 0 && state.max_position >= Duration::from_millis(1600)
    });
    eprintln!("会话 2（续播点 1000ms）：{second:?}");
    assert!(second.errors.is_empty(), "会话 2 引擎报错：{second:?}");
    assert!(
        second.first_frames > 0,
        "第二次 open 之后一帧都没出（上一会话的 pause 还留在 mpv 里）：{second:?}"
    );
    assert!(
        second.max_position >= Duration::from_millis(1600),
        "第二次 open 之后位置冻结在 {:?}（续播点 1000ms，期望推进到 ≥1600ms）：{second:?}",
        second.max_position
    );

    let _ = block_on(engine.close());
}

/// 泵事件 + 按需渲染一帧。
fn pump(
    engine: &mut MpvEngine,
    state: &mut Pumped,
    timeout: Duration,
    mut done: impl FnMut(&Pumped) -> bool,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for event in drain(engine) {
            match event {
                EngineEvent::FirstFrame => state.first_frames += 1,
                EngineEvent::PositionChanged(position) => {
                    state.max_position = state.max_position.max(position);
                }
                EngineEvent::Error(error) => state.errors.push(format!("{error:?}")),
                EngineEvent::FramePresented => {}
                _ => {}
            }
        }
        if engine.needs_redraw() {
            match engine.render(0, 640, 360) {
                Ok(true) => state.frames += 1,
                Ok(false) => {}
                Err(error) => state.errors.push(format!("render: {error:?}")),
            }
        }
        if done(state) {
            return;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// `pump` 的观测结果。
#[derive(Debug, Default)]
struct Pumped {
    first_frames: usize,
    frames: u64,
    max_position: Duration,
    errors: Vec<String>,
}
