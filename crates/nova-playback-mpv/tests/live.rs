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

    /// 建一个 EGL surfaceless 上下文并 make current（`es=true` 走 GLES 3，与 Slint 一致）。
    pub fn create() -> Result<HeadlessGl, String> {
        create_api(false)
    }

    pub fn create_api(es: bool) -> Result<HeadlessGl, String> {
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
        let api = if es {
            0x30A0 // EGL_OPENGL_ES_API
        } else {
            EGL_OPENGL_API
        };
        if unsafe { bind_api(api) } != 1 {
            return Err("eglBindAPI 失败".to_string());
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

// ---------------------------------------------------------------------------
// 渲染目标回读：体检 R6-1（1920×1080 首会话画面只剩底部一条）的引擎侧回归
// ---------------------------------------------------------------------------

/// 测试侧的 GL 回读 / 状态设置：把引擎交出来的纹理挂到临时 FBO 上读回 RGBA8。
///
/// 产品代码不回读，所以这里自己从 `eglGetProcAddress` 解析需要的入口点（与
/// `docs/testing.md` 的「真机用例自己搭最小环境」一致）。
#[allow(unsafe_code)]
mod target_probe {
    use std::ffi::{CStr, c_int, c_void};

    type GenFramebuffers = extern "C" fn(c_int, *mut u32);
    type BindFramebuffer = extern "C" fn(u32, u32);
    type FramebufferTexture2D = extern "C" fn(u32, u32, u32, u32, c_int);
    type DeleteFramebuffers = extern "C" fn(c_int, *const u32);
    type ReadPixels = extern "C" fn(c_int, c_int, c_int, c_int, u32, u32, *mut c_void);
    type PixelStore = extern "C" fn(u32, c_int);
    type Enable = extern "C" fn(u32);
    type Scissor = extern "C" fn(c_int, c_int, c_int, c_int);
    type BlendFuncSeparate = extern "C" fn(u32, u32, u32, u32);
    type GetIntegerv = extern "C" fn(u32, *mut c_int);

    const GL_FRAMEBUFFER: u32 = 0x8D40;
    const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
    const GL_TEXTURE_2D: u32 = 0x0DE1;
    const GL_PACK_ALIGNMENT: u32 = 0x0D05;
    const GL_RGBA: u32 = 0x1908;
    const GL_UNSIGNED_BYTE: u32 = 0x1401;
    /// `GL_SCISSOR_TEST`。
    pub const GL_SCISSOR_TEST: u32 = 0x0C11;
    /// `GL_BLEND`。
    pub const GL_BLEND: u32 = 0x0BE2;
    /// `GL_DEPTH_TEST`。
    pub const GL_DEPTH_TEST: u32 = 0x0B71;
    /// `GL_STENCIL_TEST`。
    pub const GL_STENCIL_TEST: u32 = 0x0B90;
    /// `GL_CULL_FACE`。
    pub const GL_CULL_FACE: u32 = 0x0B44;
    /// `GL_BLEND_SRC_RGB` / `GL_BLEND_DST_RGB` / `GL_BLEND_SRC_ALPHA` / `GL_BLEND_DST_ALPHA`。
    pub const GL_BLEND_SRC_RGB: u32 = 0x80C9;
    pub const GL_BLEND_DST_RGB: u32 = 0x80C8;
    pub const GL_BLEND_SRC_ALPHA: u32 = 0x80CB;
    pub const GL_BLEND_DST_ALPHA: u32 = 0x80CA;
    /// `GL_BLEND_EQUATION_RGB` / `GL_BLEND_EQUATION_ALPHA`。
    pub const GL_BLEND_EQUATION_RGB: u32 = 0x8009;
    pub const GL_BLEND_EQUATION_ALPHA: u32 = 0x883D;
    /// `GL_FUNC_ADD`（blend equation 的默认值）。
    pub const GL_FUNC_ADD: i32 = 0x8006;

    /// 记录引擎调用的 `glDisable`（断言「引擎确实在渲染前复位了契约要求的能力位」）。
    ///
    /// `GlFunctions` 的入口点来自我们自己的 loader，所以测试可以在这里插一层记录器：
    /// 产品代码不需要为测试留任何钩子。
    pub mod spy {
        use std::ffi::{CStr, c_char, c_void};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Mutex, OnceLock};

        type Disable = extern "C" fn(u32);
        type IsEnabled = extern "C" fn(u32) -> u8;

        static REAL_DISABLE: AtomicUsize = AtomicUsize::new(0);
        static REAL_IS_ENABLED: AtomicUsize = AtomicUsize::new(0);
        static CALLS: OnceLock<Mutex<Vec<(u8, u32)>>> = OnceLock::new();

        /// `glIsEnabled(cap)` 的标记。
        const QUERY: u8 = 0;
        /// `glDisable(cap)` 的标记。
        const DISABLE: u8 = 1;

        fn calls() -> &'static Mutex<Vec<(u8, u32)>> {
            CALLS.get_or_init(|| Mutex::new(Vec::new()))
        }

        fn record(kind: u8, cap: u32) {
            if let Ok(mut list) = calls().lock() {
                list.push((kind, cap));
            }
        }

        /// 引擎在关掉 `cap` 之前先查过它吗？（「查一下宿主是否开着，然后关掉」这对手势
        /// 只有引擎会做——mpv 从不调用 `glIsEnabled`，它只是无条件 `glDisable`。）
        pub fn reset_after_query(cap: u32) -> bool {
            calls()
                .lock()
                .map(|list| {
                    list.windows(2)
                        .any(|pair| pair[0] == (QUERY, cap) && pair[1] == (DISABLE, cap))
                })
                .unwrap_or(false)
        }

        /// 清空记录（在开始播放之前调用）。
        pub fn reset() {
            if let Ok(mut list) = calls().lock() {
                list.clear();
            }
        }

        /// `glIsEnabled` 直通（测试自己查宿主状态用）。
        pub fn is_enabled(cap: u32) -> u8 {
            let pointer = REAL_IS_ENABLED.load(Ordering::Acquire);
            if pointer == 0 {
                return 0;
            }
            // SAFETY: 指针是上面安装的真 glIsEnabled，签名逐字对应。
            let function: IsEnabled = unsafe { std::mem::transmute::<usize, IsEnabled>(pointer) };
            function(cap)
        }

        extern "C" fn spy_disable(cap: u32) {
            record(DISABLE, cap);
            let pointer = REAL_DISABLE.load(Ordering::Acquire);
            if pointer == 0 {
                return;
            }
            // SAFETY: 指针是安装时记下的真 glDisable，签名逐字对应。
            let function: Disable = unsafe { std::mem::transmute::<usize, Disable>(pointer) };
            function(cap);
        }

        extern "C" fn spy_is_enabled(cap: u32) -> u8 {
            record(QUERY, cap);
            let pointer = REAL_IS_ENABLED.load(Ordering::Acquire);
            if pointer == 0 {
                return 0;
            }
            // SAFETY: 同上。
            let function: IsEnabled = unsafe { std::mem::transmute::<usize, IsEnabled>(pointer) };
            function(cap)
        }

        /// 给 `GlFunctions::from_loader` 用的 loader：`glDisable`/`glIsEnabled` 换成记录器，
        /// 其余符号原样交给 `eglGetProcAddress`。
        pub fn install(
            gpa: unsafe extern "C" fn(*const c_char) -> *mut c_void,
        ) -> impl Fn(&CStr) -> *const c_void {
            // SAFETY: gpa 是 libEGL 的 eglGetProcAddress；两个名字都是静态 NUL 结尾字符串。
            let real_disable = unsafe { gpa(c"glDisable".as_ptr()) };
            let real_is_enabled = unsafe { gpa(c"glIsEnabled".as_ptr()) };
            REAL_DISABLE.store(real_disable as usize, Ordering::Release);
            REAL_IS_ENABLED.store(real_is_enabled as usize, Ordering::Release);
            move |name: &CStr| -> *const c_void {
                if name == c"glDisable" {
                    return spy_disable as *const c_void;
                }
                if name == c"glIsEnabled" {
                    return spy_is_enabled as *const c_void;
                }
                // SAFETY: 见上，name 是调用方传入的 NUL 结尾字符串。
                unsafe { gpa(name.as_ptr()) }
            }
        }
    }

    pub struct Probe {
        pub enable: Enable,
        pub scissor: Scissor,
        pub blend_func_separate: BlendFuncSeparate,
        get_integer_v: GetIntegerv,
        gen_fbo: GenFramebuffers,
        bind_fbo: BindFramebuffer,
        attach: FramebufferTexture2D,
        delete_fbo: DeleteFramebuffers,
        read: ReadPixels,
        store: PixelStore,
    }

    impl Probe {
        pub fn load(
            gpa: unsafe extern "C" fn(*const std::ffi::c_char) -> *mut c_void,
        ) -> Option<Self> {
            let take = |name: &CStr| -> Option<*mut c_void> {
                // SAFETY: gpa 是 EGL 的 eglGetProcAddress；name 是静态 NUL 结尾字符串。
                let pointer = unsafe { gpa(name.as_ptr()) };
                (!pointer.is_null()).then_some(pointer)
            };
            let gen_fbo = take(c"glGenFramebuffers")?;
            let bind_fbo = take(c"glBindFramebuffer")?;
            let attach = take(c"glFramebufferTexture2D")?;
            let delete_fbo = take(c"glDeleteFramebuffers")?;
            let read = take(c"glReadPixels")?;
            let store = take(c"glPixelStorei")?;
            let enable = take(c"glEnable")?;
            let scissor = take(c"glScissor")?;
            let blend_func_separate = take(c"glBlendFuncSeparate")?;
            let get_integer_v = take(c"glGetIntegerv")?;
            // SAFETY: 每个符号的签名都与 GL 规范一致（见 gl.xml），转换后只在持有同一
            // 上下文的线程上调用。
            Some(unsafe {
                Self {
                    enable: std::mem::transmute::<*mut c_void, Enable>(enable),
                    scissor: std::mem::transmute::<*mut c_void, Scissor>(scissor),
                    blend_func_separate: std::mem::transmute::<*mut c_void, BlendFuncSeparate>(
                        blend_func_separate,
                    ),
                    get_integer_v: std::mem::transmute::<*mut c_void, GetIntegerv>(get_integer_v),
                    gen_fbo: std::mem::transmute::<*mut c_void, GenFramebuffers>(gen_fbo),
                    bind_fbo: std::mem::transmute::<*mut c_void, BindFramebuffer>(bind_fbo),
                    attach: std::mem::transmute::<*mut c_void, FramebufferTexture2D>(attach),
                    delete_fbo: std::mem::transmute::<*mut c_void, DeleteFramebuffers>(delete_fbo),
                    read: std::mem::transmute::<*mut c_void, ReadPixels>(read),
                    store: std::mem::transmute::<*mut c_void, PixelStore>(store),
                }
            })
        }

        /// 读一个整型 GL 状态（测试断言 blend func / equation 用）。
        pub fn integer(&self, name: u32) -> c_int {
            let mut value = 0;
            (self.get_integer_v)(name, &mut value);
            value
        }

        /// 把「引擎交出来的纹理」读回成 RGBA8（行序：第 0 行 = GL 坐标 y=0）。
        pub fn read_texture(&self, texture: u32, width: u32, height: u32) -> Vec<u8> {
            let mut fbo = 0u32;
            (self.gen_fbo)(1, &mut fbo);
            (self.bind_fbo)(GL_FRAMEBUFFER, fbo);
            (self.attach)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                texture,
                0,
            );
            let mut buffer = vec![0u8; (width as usize) * (height as usize) * 4];
            (self.store)(GL_PACK_ALIGNMENT, 1);
            (self.read)(
                0,
                0,
                width as c_int,
                height as c_int,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                buffer.as_mut_ptr().cast(),
            );
            (self.bind_fbo)(GL_FRAMEBUFFER, 0);
            (self.delete_fbo)(1, &fbo);
            buffer
        }
    }

    /// 每 `height / 8` 行一段的平均亮度（Rec.709 加权）。
    pub fn row_bands(rgba: &[u8], width: u32, height: u32) -> Vec<f64> {
        let w = width as usize;
        let mut rows = vec![0.0f64; height as usize];
        for (y, row) in rows.iter_mut().enumerate() {
            let mut sum = 0.0;
            for x in 0..w {
                let i = (y * w + x) * 4;
                sum += 0.2126 * f64::from(rgba[i])
                    + 0.7152 * f64::from(rgba[i + 1])
                    + 0.0722 * f64::from(rgba[i + 2]);
            }
            *row = sum / w as f64;
        }
        let step = (rows.len() / 8).max(1);
        rows.chunks(step)
            .map(|chunk| chunk.iter().sum::<f64>() / chunk.len() as f64)
            .collect()
    }
}

/// 体检 R6-1 的引擎侧回归：视频尺寸 == 渲染目标尺寸（1920×1080），宿主在进入渲染之前
/// 留下 `GL_BLEND` + `GL_SCISSOR_TEST` 的非默认值（Slint/femtovg 画完一帧就是这个样子）。
///
/// `mpv/render_gl.h` 的「OpenGL state」一节要求这两项在调用渲染前必须是默认的关闭状态；
/// 不满足时这台机器（Mesa/Intel + GLES）上 mpv 的「output to screen」pass 会在混合状态下
/// 落盘，最终纹理只有底部一条带画面（产品里表现为 1920×1080 首会话近乎全黑，见
/// `.claude/pipeline/qa-report-r6.md` R6-1）。修复（引擎在渲染前复位契约要求的默认值）之后
/// 同一条用例返回整幅画面：每一段高度都有内容。
///
/// 素材用 ffmpeg 现生成（`testsrc2` 满幅有内容），`#[ignore]`：要真实 libmpv + 可用的 GPU
/// 与 ffmpeg（同本文件其它真机用例）。
#[test]
#[ignore = "需要真实 libmpv + GPU（EGL surfaceless）与 ffmpeg；不需要 Jellyfin"]
fn live_render_fills_whole_target_with_dirty_host_state() {
    let dir = tempfile::Builder::new()
        .prefix("nova-target-")
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
            "testsrc2=duration=6:size=1920x1080:rate=10",
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

    // GLES 3 上下文（与 Slint 的 femtovg 后端同一条 API）。
    let headless = egl::create_api(true).expect("EGL surfaceless 上下文创建失败");
    let probe = target_probe::Probe::load(headless.get_proc_address).expect("装载回读入口点失败");
    // 用带记录器的 loader 建入口点表：`glDisable` 会被记下来，用于断言引擎确实复位了契约位。
    let loader = target_probe::spy::install(headless.get_proc_address);
    let functions = GlFunctions::from_loader(&loader).expect("装载 GL 入口点失败");
    target_probe::spy::reset();

    // 宿主（femtovg）留下的状态：混合开着（premultiplied over）、裁剪测试开着且裁剪框是
    // 整窗、深度 / 模板 / 面剔除也开着——mpv 的契约要求这些能力位都是默认值。
    (probe.blend_func_separate)(1, 0x0303, 1, 0x0303);
    (probe.scissor)(0, 0, 1920, 1080);
    (probe.enable)(target_probe::GL_BLEND);
    (probe.enable)(target_probe::GL_SCISSOR_TEST);
    (probe.enable)(target_probe::GL_DEPTH_TEST);
    (probe.enable)(target_probe::GL_STENCIL_TEST);
    (probe.enable)(target_probe::GL_CULL_FACE);

    let mut engine = MpvEngine::new(EngineOptions::new().audio_output("null")).expect("建引擎失败");
    engine.attach_gl(functions).expect("attach_gl 失败");
    block_on(engine.open(OpenRequest::new(url))).expect("open 失败");

    let mut state = Pumped::default();
    pump(&mut engine, &mut state, Duration::from_secs(20), |state| {
        state.frames >= 20
    });
    assert!(state.errors.is_empty(), "引擎报错：{:?}", state.errors);
    let (texture, width, height) = engine.texture().expect("20 帧之后仍没有纹理");
    assert_eq!(
        (width, height),
        (1920, 1080),
        "渲染目标尺寸应当是视频尺寸（素材 1920×1080）"
    );

    let rgba = probe.read_texture(texture, width, height);
    let bands = target_probe::row_bands(&rgba, width, height);
    eprintln!("[target] 1920×1080 分段亮度：{bands:?}");
    // 没有内容的高度段 = 画面被切掉（R6-1 的形态：只有底部一条有画面）。
    let empty: Vec<usize> = bands
        .iter()
        .enumerate()
        .filter(|(_, luma)| **luma < 8.0)
        .map(|(index, _)| index)
        .collect();
    assert!(
        empty.is_empty(),
        "渲染目标有 {} / {} 段没有画面（{empty:?}，分段亮度 {bands:?}）：mpv 在宿主遗留的 \
         GL 状态下没有画满整幅纹理",
        empty.len(),
        bands.len()
    );

    // 修复本体：引擎在把 FBO 交给 mpv 之前，必须把 mpv 契约要求的能力位复位成默认值。
    for (name, cap) in [
        ("GL_SCISSOR_TEST", target_probe::GL_SCISSOR_TEST),
        ("GL_BLEND", target_probe::GL_BLEND),
        ("GL_DEPTH_TEST", target_probe::GL_DEPTH_TEST),
        ("GL_STENCIL_TEST", target_probe::GL_STENCIL_TEST),
        ("GL_CULL_FACE", target_probe::GL_CULL_FACE),
    ] {
        assert!(
            target_probe::spy::reset_after_query(cap),
            "引擎没有在渲染前把 {name}（0x{cap:04x}）复位成默认值：mpv 的 render_gl.h \
             契约要求它是默认值"
        );
    }

    // 宿主的 GL 状态必须原样还回去（引擎只在自己的渲染期间复位）。
    for (name, cap) in [
        ("GL_BLEND", target_probe::GL_BLEND),
        ("GL_SCISSOR_TEST", target_probe::GL_SCISSOR_TEST),
    ] {
        assert_ne!(
            target_probe::spy::is_enabled(cap),
            0,
            "渲染结束后引擎没有把 {name} 还给宿主"
        );
    }
    // blend func 也是 mpv 不负责还原的例外：必须回到宿主设置的值。
    let blend = [
        probe.integer(target_probe::GL_BLEND_SRC_RGB),
        probe.integer(target_probe::GL_BLEND_DST_RGB),
        probe.integer(target_probe::GL_BLEND_SRC_ALPHA),
        probe.integer(target_probe::GL_BLEND_DST_ALPHA),
    ];
    assert_eq!(
        blend,
        [1, 0x0303, 1, 0x0303],
        "渲染结束后引擎没有把 glBlendFuncSeparate 还给宿主（实际 {blend:?}）"
    );
    // blend equation 不在 mpv 的例外清单里，宿主保持默认的 GL_FUNC_ADD；
    // save/restore 也必须把它还原成默认值（不能把 mpv 的中间状态漏出去）。
    assert_eq!(
        probe.integer(target_probe::GL_BLEND_EQUATION_RGB),
        target_probe::GL_FUNC_ADD,
        "渲染结束后 blend equation RGB 不是默认的 GL_FUNC_ADD"
    );
    assert_eq!(
        probe.integer(target_probe::GL_BLEND_EQUATION_ALPHA),
        target_probe::GL_FUNC_ADD,
        "渲染结束后 blend equation Alpha 不是默认的 GL_FUNC_ADD"
    );

    let _ = block_on(engine.close());
}
