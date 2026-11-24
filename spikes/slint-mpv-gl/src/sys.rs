// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! libmpv C API 的薄封装：**整个 crate 的 `unsafe` 集中在 `sys.rs` / `gl.rs` / `glproc.rs` 三个模块里**。
//!
//! 为什么不用 safe 封装 `libmpv2 = "7.0.1"`：它的 `RenderParamApiType` 只有 `OpenGl` 一个变体、
//! `RenderParam` 里没有 `OPENGL_INIT_PARAMS`，且 `RenderContext::render()` 把参数硬编码成 FBO+FlipY，
//! 无法把 `MPV_RENDER_PARAM_OPENGL_INIT_PARAMS`（必须的 `get_proc_address`）传给 mpv。
//! 因此本 crate 直接依赖 `libmpv2-sys = "4.0.1"`（bindgen 原始绑定）。
//!
//! 与 `spikes/libmpv-proto/src/sys.rs`（SW 渲染路径）的关系：本文件是那份代码的 GL 版，
//! 并按代码审查意见修掉了 5 个问题（见文件内 `审查修复 #n` 标注）。

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use libmpv2_sys as raw;

/// 把 libmpv 的错误码转成可读字符串。
pub fn error_string(code: c_int) -> String {
    // SAFETY: mpv_error_string() 对任意 c_int 都返回指向静态 C 字符串的非空指针，
    // 不需要也不允许释放。
    let p = unsafe { raw::mpv_error_string(code) };
    // SAFETY: 见上，指针非空且以 NUL 结尾；静态数据，无生命周期问题。
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// libmpv 的约定：返回负数为错误码。
fn check(rc: c_int, what: &str) -> Result<(), String> {
    if rc < 0 {
        Err(format!("{what} 失败: {} (error {rc})", error_string(rc)))
    } else {
        Ok(())
    }
}

/// 一个 mpv core 的句柄（`mpv_handle`）。
pub struct Handle {
    ctx: *mut raw::mpv_handle,
}

/// `mpv_wait_event` 结果里本原型关心的子集；需要的字段在调用方立刻拷贝出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    StartFile,
    FileLoaded,
    EndFile {
        error: c_int,
    },
    VideoReconfig,
    PlaybackRestart,
    Idle,
    /// 一条 libmpv 日志（每行一条）。
    LogMessage {
        prefix: String,
        level: String,
        text: String,
    },
    /// `MPV_EVENT_END_FILE` 的 `data` 为空。
    ///
    /// 审查修复 #4：`ev.data` 按 client.h 约定指向 `mpv_event_end_file`，但协议层面
    /// 允许它为 NULL（例如极少数内部失败路径）。解引用空指针是 UB，所以单独成一个
    /// 变体，让调用方自己决定怎么处理。
    EndFileWithoutData,
    Other(u32),
}

impl Handle {
    pub fn create() -> Result<Self, String> {
        // SAFETY: 无入参，只分配一个新的 mpv core；返回 NULL 表示失败（OOM）。
        let ctx = unsafe { raw::mpv_create() };
        if ctx.is_null() {
            return Err("mpv_create() 返回 NULL".to_string());
        }
        Ok(Self { ctx })
    }

    pub fn as_ptr(&self) -> *mut raw::mpv_handle {
        self.ctx
    }

    /// 设置 option。必须在 [`Handle::initialize`] 之前调用（之后同名项变成属性）。
    pub fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let cvalue = CString::new(value).map_err(|e| e.to_string())?;
        // SAFETY: 两个指针都指向本次调用期间存活的 NUL 结尾字符串；
        // 初始化前设置的是 option，libmpv 会复制其内容，不保留指针。
        check(
            unsafe { raw::mpv_set_option_string(self.ctx, cname.as_ptr(), cvalue.as_ptr()) },
            &format!("mpv_set_option_string({name}={value})"),
        )
    }

    /// 让 libmpv 把日志以 `MPV_EVENT_LOG_MESSAGE` 事件发过来（libmpv 默认不发）。
    pub fn request_log_messages(&self, min_level: &str) -> Result<(), String> {
        let level = CString::new(min_level).map_err(|e| e.to_string())?;
        // SAFETY: level 是本次调用期间存活的 NUL 结尾字符串；libmpv 复制其内容。
        check(
            unsafe { raw::mpv_request_log_messages(self.ctx, level.as_ptr()) },
            "mpv_request_log_messages",
        )
    }

    pub fn initialize(&self) -> Result<(), String> {
        // SAFETY: self.ctx 来自 mpv_create()，且只在 Drop 里 destroy，此处必然有效。
        check(unsafe { raw::mpv_initialize(self.ctx) }, "mpv_initialize")
    }

    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args
            .iter()
            .map(|a| CString::new(*a))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null()); // libmpv 要求 NULL 结尾的 char* 数组
        // SAFETY: ptrs 以 NULL 结尾，其余元素指向 owned 里的 NUL 结尾字符串；
        // owned 活到调用结束，libmpv 不会保留这些指针。
        check(
            unsafe { raw::mpv_command(self.ctx, ptrs.as_mut_ptr()) },
            "mpv_command",
        )
    }

    pub fn set_property_string(&self, name: &str, value: &str) -> Result<(), String> {
        let name = CString::new(name).map_err(|e| e.to_string())?;
        let value = CString::new(value).map_err(|e| e.to_string())?;
        // SAFETY: 同上，两个 NUL 结尾字符串在调用期间有效。
        check(
            unsafe { raw::mpv_set_property_string(self.ctx, name.as_ptr(), value.as_ptr()) },
            "mpv_set_property_string",
        )
    }

    /// 读属性并转成字符串；属性不存在或无法转换时返回 `None`。
    pub fn get_string(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        // SAFETY: name 是存活的 NUL 结尾字符串；返回 NULL 表示属性不存在/不可转换。
        let p = unsafe { raw::mpv_get_property_string(self.ctx, name.as_ptr()) };
        if p.is_null() {
            return None;
        }
        // SAFETY: 非空时 libmpv 返回堆分配的 NUL 结尾字符串，所有权交给调用方。
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        // SAFETY: 该内存由 mpv_get_property_string 分配，必须用 mpv_free 释放一次，
        // 释放后不再使用 p。
        unsafe { raw::mpv_free(p as *mut c_void) };
        Some(s)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let name = CString::new(name).ok()?;
        let mut out: i64 = 0;
        // SAFETY: MPV_FORMAT_INT64 要求 data 指向 i64；out 在调用期间有效且可写。
        let rc = unsafe {
            raw::mpv_get_property(
                self.ctx,
                name.as_ptr(),
                raw::mpv_format_MPV_FORMAT_INT64,
                &mut out as *mut i64 as *mut c_void,
            )
        };
        (rc >= 0).then_some(out)
    }

    /// 取一个事件；`timeout` 秒内没有事件时返回 `None`（内部是 `MPV_EVENT_NONE`）。
    pub fn next_event(&self, timeout: f64) -> Option<Event> {
        // SAFETY: self.ctx 有效；返回值由 libmpv 拥有，只在下次 mpv_wait_event 前有效，
        // 所以这里立刻把需要的字段拷贝出来，不保存指针。
        let ev = unsafe { raw::mpv_wait_event(self.ctx, timeout) };
        if ev.is_null() {
            return None;
        }
        // SAFETY: 非空指针指向 libmpv 拥有的 mpv_event，在本函数内保持有效。
        let ev = unsafe { &*ev };
        Some(match ev.event_id {
            raw::mpv_event_id_MPV_EVENT_START_FILE => Event::StartFile,
            raw::mpv_event_id_MPV_EVENT_FILE_LOADED => Event::FileLoaded,
            raw::mpv_event_id_MPV_EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
            raw::mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
            raw::mpv_event_id_MPV_EVENT_IDLE => Event::Idle,
            raw::mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                if ev.data.is_null() {
                    return Some(Event::Other(ev.event_id));
                }
                // SAFETY: LOG_MESSAGE 的 data 指向 mpv_event_log_message；
                // 三个字符串字段都由 libmpv 拥有，只在本次事件有效期内可读，
                // 所以立刻拷成 String。
                let m = unsafe { &*(ev.data as *const raw::mpv_event_log_message) };
                let text = |p: *const c_char| -> String {
                    if p.is_null() {
                        String::new()
                    } else {
                        // SAFETY: 见上，非空指针指向 NUL 结尾的字符串。
                        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
                    }
                };
                Event::LogMessage {
                    prefix: text(m.prefix),
                    level: text(m.level),
                    text: text(m.text),
                }
            }
            raw::mpv_event_id_MPV_EVENT_NONE => return None,
            raw::mpv_event_id_MPV_EVENT_END_FILE => {
                if ev.data.is_null() {
                    // 审查修复 #4：解引用前检查非空。
                    return Some(Event::EndFileWithoutData);
                }
                // SAFETY: END_FILE 事件的 data 按 client.h 约定指向 mpv_event_end_file；
                // 只读取 error 字段，指针不越出本次事件的有效期。
                let ef = unsafe { &*(ev.data as *const raw::mpv_event_end_file) };
                Event::EndFile { error: ef.error }
            }
            other => Event::Other(other),
        })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        // SAFETY: ctx 由 mpv_create() 创建且尚未销毁；terminate_destroy 会阻塞到 core 退出。
        // render context 必须在调用本函数之前释放（由 Session::drop 显式保证）。
        unsafe { raw::mpv_terminate_destroy(self.ctx) };
        self.ctx = ptr::null_mut();
    }
}

/// `mpv_render_context_set_update_callback` 的唤醒信号。不触碰任何 mpv API。
///
/// 与 SW 版本不同，这里不需要阻塞等待（渲染在 Slint 的渲染线程上做，主线程只轮询），
/// 所以内部只是一个 `AtomicBool`：`raise()` 置位，`take()` 消费式读取。
#[derive(Default)]
pub struct FrameSignal {
    ready: AtomicBool,
}

impl FrameSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费式读取“是否有新帧”标记。
    pub fn take(&self) -> bool {
        self.ready.swap(false, Ordering::AcqRel)
    }

    fn raise(&self) {
        self.ready.store(true, Ordering::Release);
    }
}

/// libmpv 的渲染更新回调。
///
/// 这段代码运行在 mpv 的内部线程上，因此：不调用任何 mpv API、不让 panic 穿过 FFI 边界。
/// 加锁用 `unwrap_or_else(|e| e.into_inner())` 处理毒化，避免在 C 调用栈里 panic。
///
/// 审查修复 #3：入口先检查 `data` 非空。空指针在协议里不该出现，但一旦出现，
/// `&*(null)` 是 UB，而“静默丢弃一次唤醒”最多让下一帧晚一次刷新。
unsafe extern "C" fn update_callback(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: data 是 GlRenderer 持有的 Arc<FrameSignal> 的裸指针，从
    // set_update_callback() 到 set_update_callback(None) 期间一直存活（见 Drop）。
    let signal = unsafe { &*(data as *const FrameSignal) };
    signal.raise();
}

/// mpv 的渲染目标：我们自己创建的 FBO（颜色附件是一张 GL_RGBA8 纹理）。
///
/// 审查修复 #5：字段私有化 + 构造时 `checked_mul` 校验尺寸，render 入口再校验一次，
/// 避免“尺寸来自视频属性（外部输入）”时算出错的字节数或在 `as c_int` 时溢出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlTarget {
    fbo: u32,
    width: u32,
    height: u32,
}

impl GlTarget {
    /// GL 的 `GL_RGBA8`（0x8058）；作为 `mpv_opengl_fbo::internal_format` 告知 mpv
    /// 目标纹理的内部格式（影响 dithering 决定）。
    pub const GL_RGBA8: i32 = 0x8058;

    pub fn new(fbo: u32, width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("渲染目标尺寸非法: {width}x{height}"));
        }
        if width > c_int::MAX as u32 || height > c_int::MAX as u32 {
            return Err(format!("渲染目标尺寸超出 c_int: {width}x{height}"));
        }
        // 4 字节/像素的整帧字节数必须能算出来（同时挡住 u32 溢出的乘法链）。
        let _bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| format!("渲染目标 {width}x{height} 的字节数溢出 usize"))?;
        if fbo > c_int::MAX as u32 {
            return Err(format!("FBO 名 {fbo} 超出 c_int"));
        }
        Ok(Self { fbo, width, height })
    }

    pub fn fbo(&self) -> u32 {
        self.fbo
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

/// OpenGL 渲染（`MPV_RENDER_API_TYPE_OPENGL`）上下文。
pub struct GlRenderer {
    ctx: *mut raw::mpv_render_context,
    /// 审查修复 #2：用 `Option` 持有，Drop 里显式 `take()`，让“先注销回调、再释放
    /// context、最后才 drop Arc”这个顺序在代码里看得见，而不是依赖字段声明顺序。
    signal: Option<Arc<FrameSignal>>,
    /// 传给 mpv 的 DRM render node fd（VAAPI interop 用）。render context 释放后再关。
    render_fd: Option<std::os::fd::OwnedFd>,
}

impl GlRenderer {
    /// 创建 GL 渲染上下文。必须在第一个 VO 被创建之前调用
    /// （render.h "Context and handle lifecycle"），且调用时 GL 上下文必须 current。
    /// `render_fd` 是 DRM render node（VAAPI zero-copy interop 需要）；
    /// 传 `None` 表示不做 interop（软解 / `vaapi-copy` 用不到）。
    pub fn create(
        handle: &Handle,
        render_fd: Option<std::os::fd::OwnedFd>,
    ) -> Result<Self, String> {
        let signal = Arc::new(FrameSignal::new());
        let mut init = raw::mpv_opengl_init_params {
            // mpv 会把这个回调保存下来，之后在 mpv_render_* 调用期间随时用它取函数指针，
            // 所以不能把 Slint 回调里那个只在本帧有效的 `&dyn Fn` 直接交给它。
            // 实现见 glproc.rs：优先走 Slint 的 loader（用 thread-local 限定生命周期），
            // 取不到时退回 eglGetProcAddress/dlsym，并计数。
            get_proc_address: Some(crate::glproc::mpv_get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut ctx: *mut raw::mpv_render_context = ptr::null_mut();
        // MPV_RENDER_API_TYPE_OPENGL 是静态的 NUL 结尾字节串 b"opengl\0"，
        // render.h 规定该参数的类型是 char*（指向该字符串的指针）。
        let api_type = raw::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void;
        // VAAPI 零拷贝（hwdec=vaapi，非 -copy）在 libmpv 模式下**必须**由应用把
        // “GPU 侧的原生句柄”交给 mpv：mpv 的 hwdec_vaapi 通过
        // `ra_get_native_resource(ra, "drm_params_v2")` / `"wl"` / `"x11"` 找 VA display，
        // 而 libmpv_gpu.c 只在 `mpv_render_context_create()` 收到
        // MPV_RENDER_PARAM_{DRM_DISPLAY_V2,WL_DISPLAY,X11_DISPLAY} 时才注册这些资源。
        // 不给的话 hwdec 驱动会打印 "Could not create a VA display." 并静默退回软解
        // （实测：hwdec-current 从 vaapi 变成 no）。
        let mut drm = raw::mpv_opengl_drm_params_v2 {
            fd: -1,
            crtc_id: -1,
            connector_id: -1,
            atomic_request_ptr: ptr::null_mut(),
            render_fd: -1,
        };
        let mut params: Vec<raw::mpv_render_param> = vec![
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                data: api_type,
            },
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&mut init as *mut raw::mpv_opengl_init_params).cast(),
            },
        ];
        if let Some(fd) = &render_fd {
            use std::os::fd::AsRawFd;
            drm.render_fd = fd.as_raw_fd();
            params.push(raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_DRM_DISPLAY_V2,
                data: (&mut drm as *mut raw::mpv_opengl_drm_params_v2).cast(),
            });
        }
        // 参数数组必须以 MPV_RENDER_PARAM_INVALID (=0) 结尾。
        params.push(raw::mpv_render_param {
            type_: 0,
            data: ptr::null_mut(),
        });
        // SAFETY: params 以 INVALID 结尾，只在调用期间被读；api_type 指向 'static 数据；
        // init 指向本栈帧上的结构，mpv 在 create 期间只读它；ctx 是输出参数，
        // 返回码检查通过后才使用其中的指针。
        let rc = unsafe {
            raw::mpv_render_context_create(&mut ctx, handle.as_ptr(), params.as_mut_ptr())
        };
        check(rc, "mpv_render_context_create(MPV_RENDER_API_TYPE_OPENGL)")?;
        // SAFETY: ctx 已创建成功；回调指针是 'static fn；回调上下文是 Arc 的裸指针，
        // 只要 GlRenderer 活着（即直到 Drop 里清回调）就有效。
        unsafe {
            raw::mpv_render_context_set_update_callback(
                ctx,
                Some(update_callback),
                Arc::as_ptr(&signal) as *mut c_void,
            );
        }
        Ok(Self {
            ctx,
            signal: Some(signal),
            render_fd,
        })
    }

    /// 更新回调触发的唤醒信号（主线程用它决定要不要再请求一次重绘）。
    pub fn signal(&self) -> Option<&FrameSignal> {
        self.signal.as_deref()
    }

    /// 是否有新帧/需要重绘；返回值是 `mpv_render_update_flag` 位集。
    pub fn update(&self) -> u64 {
        // SAFETY: self.ctx 由 create() 成功返回，直到 Drop 前一直有效。
        unsafe { raw::mpv_render_context_update(self.ctx) }
    }

    /// 是否有新帧可渲染。
    pub fn has_new_frame(flags: u64) -> bool {
        flags & u64::from(raw::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME) != 0
    }

    /// 把当前帧渲染进 `target`（我们自己的 FBO）。
    ///
    /// 这是 GL 后端的零拷贝路径：解码结果（vaapi 时是 GPU 上的 surface）经 EGL interop
    /// 直接由 GPU 采样进目标纹理，中间**没有**“下载到系统内存再上传”的往返。
    pub fn render_into(&self, target: &GlTarget) -> Result<(), String> {
        // 审查修复 #5：入口再校验一次（GlTarget 的字段虽然私有，但来自属性驱动的尺寸，
        // 这里守住最后一道关）。
        if target.width() == 0 || target.height() == 0 {
            return Err("render_into: 目标尺寸为 0".to_string());
        }
        let mut fbo = raw::mpv_opengl_fbo {
            fbo: target.fbo() as c_int,
            w: target.width() as c_int,
            h: target.height() as c_int,
            internal_format: GlTarget::GL_RGBA8,
        };
        // 0 = 正常方向。mpv 的注释说的是“渲染到 OpenGL 默认帧缓冲时需要翻转”
        // （默认帧缓冲的原点在左下），我们渲染进的是自己的 FBO/纹理，
        // 纹理在采样时按 Slint 的 `BorrowedOpenGLTextureOrigin::BottomLeft` 解释。
        let mut flip: c_int = 0;
        // 0 = 不阻塞到目标显示时间。渲染节奏由 Slint 的重绘驱动，
        // 阻塞在 UI 线程上只会让界面卡在视频时钟上。
        let mut block: c_int = 0;
        let mut params = [
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&mut fbo as *mut raw::mpv_opengl_fbo).cast(),
            },
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                data: (&mut flip as *mut c_int).cast(),
            },
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME,
                data: (&mut block as *mut c_int).cast(),
            },
            raw::mpv_render_param {
                type_: 0,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: 参数类型与 render.h/render_gl.h 一致：
        //   OPENGL_FBO -> mpv_opengl_fbo*，FLIP_Y -> int*，BLOCK_FOR_TARGET_TIME -> int*。
        // 它们都指向本函数栈上的、调用期间有效且可写的内存；fbo 指向的 GL 对象由调用方
        // 保证存在（GlTarget 与 VideoTexture 一一对应）。
        check(
            unsafe { raw::mpv_render_context_render(self.ctx, params.as_mut_ptr()) },
            "mpv_render_context_render",
        )
    }

    pub fn report_swap(&self) {
        // SAFETY: self.ctx 有效（同 update()）。
        unsafe { raw::mpv_render_context_report_swap(self.ctx) }
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        // 审查修复 #2：先把 Arc 从字段里取出来（此时回调仍可能被调用，所以**不能**先 drop），
        // 再注销回调，最后才释放 context 并让 Arc 在本函数末尾 drop。
        // 顺序：注销回调 → 释放 context → Arc 落盘。
        let signal = self.signal.take();
        // SAFETY: 先清掉更新回调，libmpv 之后（mpv_render_context_free 返回后）保证
        // 不会再调用 update_callback，因此 signal 里的 Arc 不会在回调中被访问；
        // 然后释放 render context。必须在 mpv_terminate_destroy 之前完成（render.h 要求）。
        unsafe {
            raw::mpv_render_context_set_update_callback(self.ctx, None, ptr::null_mut());
            raw::mpv_render_context_free(self.ctx);
        }
        self.ctx = ptr::null_mut();
        drop(signal);
        // mpv 不持有 fd 的所有权（它只 memdup 了那个结构体里的整数），
        // render context 释放之后再关。
        drop(self.render_fd.take());
    }
}

/// 一个“libmpv + GL 渲染”会话：mpv core + GL 渲染上下文。
///
/// 审查修复 #1：Drop 里**显式**按 renderer → player 的顺序释放，不依赖字段声明顺序
/// （render.h 要求 render context 必须在 mpv core 销毁之前释放）。
pub struct Session {
    renderer: Option<GlRenderer>,
    player: Option<Handle>,
}

impl Session {
    /// `hwdec` 是 `--hwdec` 的值；`extra` 是额外的 mpv option（初始化前设置）。
    pub fn open(
        hwdec: &str,
        extra: &[(&str, &str)],
        log_level: &str,
        render_fd: Option<std::os::fd::OwnedFd>,
    ) -> Result<Self, String> {
        let player = Handle::create()?;
        for (name, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("terminal", "no"),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("audio", "no"),
            ("vo", "libmpv"),
            ("hwdec", hwdec),
        ] {
            player.set_option(name, value)?;
        }
        for (name, value) in extra {
            player.set_option(name, value)?;
        }
        player.request_log_messages(log_level)?;
        player.initialize()?;
        // render context 必须在第一个 VO 创建之前建好，否则视频初始化会失败
        // 或退回到自己开窗的 VO（render.h "Context and handle lifecycle"）。
        let renderer = GlRenderer::create(&player, render_fd)?;
        Ok(Self {
            renderer: Some(renderer),
            player: Some(player),
        })
    }

    pub fn player(&self) -> &Handle {
        self.player.as_ref().expect("player 已被释放")
    }

    pub fn renderer(&self) -> &GlRenderer {
        self.renderer.as_ref().expect("renderer 已被释放")
    }

    /// 开始播放（在 render context 建好之后调用）。
    pub fn load(&self, file: &std::path::Path) -> Result<(), String> {
        let path = file.to_string_lossy().into_owned();
        self.player().command(&["loadfile", &path, "replace"])
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 审查修复 #1：显式顺序，renderer 一定先于 player 释放。
        if let Some(renderer) = self.renderer.take() {
            drop(renderer);
        }
        if let Some(player) = self.player.take() {
            drop(player);
        }
    }
}
