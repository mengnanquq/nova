// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! libmpv C API 的薄封装：**整个 crate 的 `unsafe` 只出现在这个模块里**。
//!
//! 为什么不用 safe 封装 `libmpv2 = "7.0.1"`：
//! - 它的 `render` 模块只定义了一个 `RenderParamApiType::OpenGl` 变体
//!   （`libmpv2-7.0.1/src/mpv/render.rs:83`），`RenderParam` 枚举里也没有任何 `SW_*` 变体；
//! - `RenderContext::render()`（同文件）把参数硬编码成 `FBO`(type 3) + `FlipY`(type 4)。
//!
//! 而 software 渲染要求 `MPV_RENDER_API_TYPE_SW "sw"` + `SW_SIZE`/`SW_FORMAT`/`SW_STRIDE`/
//! `SW_POINTER`（`/usr/include/mpv/render.h`），这些在 safe 封装里根本无法表达，
//! 因此本 crate 直接依赖 `libmpv2-sys`，用一层薄封装把指针/生命周期约定讲清楚。
//!
//! 每个 `unsafe` 块都附带 SAFETY 注释，说明：指针从哪来、谁拥有内存、
//! 以及“调用期间有效”这类生命周期假设。

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    StartFile,
    FileLoaded,
    EndFile { error: c_int },
    VideoReconfig,
    PlaybackRestart,
    Idle,
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

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let name = CString::new(name).ok()?;
        let mut out: f64 = 0.0;
        // SAFETY: MPV_FORMAT_DOUBLE 要求 data 指向 f64（c_double），libmpv 会做类型转换；
        // out 在调用期间有效且可写。
        let rc = unsafe {
            raw::mpv_get_property(
                self.ctx,
                name.as_ptr(),
                raw::mpv_format_MPV_FORMAT_DOUBLE,
                &mut out as *mut f64 as *mut c_void,
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
            raw::mpv_event_id_MPV_EVENT_NONE => return None,
            raw::mpv_event_id_MPV_EVENT_END_FILE => {
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
        // render context 必须在调用本函数之前释放（由 Session 的字段顺序保证）。
        unsafe { raw::mpv_terminate_destroy(self.ctx) };
        self.ctx = ptr::null_mut();
    }
}

/// 渲染更新回调与渲染线程之间的唤醒信号。不触碰任何 mpv API。
#[derive(Default)]
pub struct FrameSignal {
    ready: Mutex<bool>,
    cv: Condvar,
}

impl FrameSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// 等待“有新帧”的唤醒；`timeout` 内没有则返回 false。唤醒是消费式的。
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.ready.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = self
            .cv
            .wait_timeout(guard, timeout)
            .unwrap_or_else(|e| e.into_inner());
        let ready = *guard;
        *guard = false;
        ready
    }

    fn raise(&self) {
        let mut guard = self.ready.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        self.cv.notify_all();
    }
}

/// libmpv 的渲染更新回调。
///
/// 这段代码运行在 mpv 的内部线程上，因此：不调用任何 mpv API、不让 panic 穿过 FFI 边界。
/// 加锁用 `unwrap_or_else(|e| e.into_inner())` 处理毒化，避免在 C 调用栈里 panic。
unsafe extern "C" fn update_callback(data: *mut c_void) {
    // SAFETY: data 是 SwRenderer 持有的 Arc<FrameSignal> 的裸指针，从
    // set_update_callback() 到 set_update_callback(None) 期间一直存活（见 Drop）。
    let signal = unsafe { &*(data as *const FrameSignal) };
    signal.raise();
}

/// SW 渲染目标的像素格式（`MPV_RENDER_PARAM_SW_FORMAT`）。
///
/// render.h 只保证 `rgb0` / `bgr0` / `0rgb` / `0bgr`（每像素 4 字节，首地址分量见名字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceFormat {
    /// 内存顺序 R,G,B,X
    Rgb0,
    /// 内存顺序 B,G,R,X
    Bgr0,
}

impl SurfaceFormat {
    pub fn mpv_name(self) -> &'static CStr {
        match self {
            SurfaceFormat::Rgb0 => c"rgb0",
            SurfaceFormat::Bgr0 => c"bgr0",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SurfaceFormat::Rgb0 => "rgb0",
            SurfaceFormat::Bgr0 => "bgr0",
        }
    }
}

/// software 渲染（`MPV_RENDER_API_TYPE_SW`）上下文。
pub struct SwRenderer {
    ctx: *mut raw::mpv_render_context,
    signal: Arc<FrameSignal>,
    /// 是否让 `mpv_render_context_render()` 阻塞到该帧的“显示时间”。
    /// 测吞吐时必须关掉，否则测到的是帧间隔（见 render.h 里
    /// MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME 的说明）。
    /// 用 `Cell` 是为了让 `&SwRenderer` 也能翻转这个开关（渲染线程只有一份所有权）。
    block_for_target_time: std::cell::Cell<bool>,
}

impl SwRenderer {
    /// 创建 SW 渲染上下文。必须在第一个 VO 被创建之前调用
    /// （render.h "Context and handle lifecycle"）。
    pub fn create(handle: &Handle) -> Result<Self, String> {
        let signal = Arc::new(FrameSignal::new());
        let mut ctx: *mut raw::mpv_render_context = ptr::null_mut();
        // MPV_RENDER_API_TYPE_SW 是静态的 NUL 结尾字节串 b"sw\0"，
        // render.h 规定该参数的类型是 char*（指向该字符串的指针）。
        let api_type = raw::MPV_RENDER_API_TYPE_SW.as_ptr() as *mut c_void;
        let mut params = [
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                data: api_type,
            },
            // 参数数组必须以 MPV_RENDER_PARAM_INVALID (=0) 结尾。
            raw::mpv_render_param {
                type_: 0,
                data: ptr::null_mut(),
            },
        ];
        // SAFETY: params 以 INVALID 结尾，只在调用期间被读；api_type 指向 'static 数据；
        // ctx 是输出参数，返回码检查通过后才使用其中的指针。
        let rc = unsafe {
            raw::mpv_render_context_create(&mut ctx, handle.as_ptr(), params.as_mut_ptr())
        };
        check(rc, "mpv_render_context_create(MPV_RENDER_API_TYPE_SW)")?;
        // SAFETY: ctx 已创建成功；回调指针是 'static fn；回调上下文是 Arc 的裸指针，
        // 只要 SwRenderer 活着（即直到 Drop 里清回调）就有效。
        unsafe {
            raw::mpv_render_context_set_update_callback(
                ctx,
                Some(update_callback),
                Arc::as_ptr(&signal) as *mut c_void,
            );
        }
        Ok(Self {
            ctx,
            signal,
            block_for_target_time: std::cell::Cell::new(true),
        })
    }

    /// 控制 `mpv_render_context_render()` 是否等到目标显示时间再返回。
    /// 默认 `true`（与 libmpv 默认一致）；跑基准时应设为 `false`。
    pub fn set_block_for_target_time(&self, block: bool) {
        self.block_for_target_time.set(block);
    }

    /// 更新回调触发的唤醒信号（渲染线程用它等新帧）。
    pub fn signal(&self) -> &FrameSignal {
        &self.signal
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

    /// 把当前帧渲染进调用方提供的内存。
    ///
    /// 这是 SW 后端的天然零拷贝路径：mpv 直接把最终像素写进 `img`，
    /// 不存在“mpv 内部截图 → 再拷贝出来”的中间步骤。
    pub fn render_into(&self, format: SurfaceFormat, img: &mut AlignedImage) -> Result<(), String> {
        let mut size: [c_int; 2] = [img.width() as c_int, img.height() as c_int];
        let mut stride: usize = img.stride();
        let mut block: c_int = i32::from(self.block_for_target_time.get());
        // render.h: SW_FORMAT 的类型是 char*，即“指向格式名字符串的指针”（不是 char**）。
        let format_name = format.mpv_name().as_ptr() as *mut c_void;
        let pointer: *mut c_void = img.as_mut_ptr().cast();
        let mut params = [
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr().cast(),
            },
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT,
                data: format_name,
            },
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE,
                data: (&mut stride as *mut usize).cast(),
            },
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER,
                data: pointer,
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
        // SAFETY: 四个 SW 参数的类型与 render.h 一致：
        //   SW_SIZE -> int[2]，SW_FORMAT -> char*，SW_STRIDE -> size_t*，SW_POINTER -> void*，
        //   BLOCK_FOR_TARGET_TIME -> int*（bool 在 C 里是 int）。
        // 它们都指向本函数栈上或 img 内部的、调用期间有效且可写（img）的内存，
        // 且 size/stride/pointer 与 img 的实际布局一致（AlignedImage 保证
        // stride*height 字节可写）。
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

impl Drop for SwRenderer {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        // SAFETY: 先清掉更新回调，libmpv 之后不会再调用 update_callback，
        // 之后 Arc<FrameSignal> 才被 drop（字段 drop 顺序），回调里的裸指针不会悬垂；
        // 然后释放 render context。必须在 mpv_terminate_destroy 之前完成（render.h 要求）。
        unsafe {
            raw::mpv_render_context_set_update_callback(self.ctx, None, ptr::null_mut());
            raw::mpv_render_context_free(self.ctx);
        }
        self.ctx = ptr::null_mut();
    }
}

/// 像素缓冲区：行跨度（stride）按 64 字节对齐。
///
/// render.h 对 `MPV_RENDER_PARAM_SW_STRIDE` 的说明：stride 和 pointer 都应是 64 的倍数，
/// 否则可能走慢路径，最坏情况下 libmpv 会把整帧再拷一次。这里用“多分配 + 手动对齐”
/// 满足要求，不引入额外依赖。
pub struct AlignedImage {
    buf: Vec<u8>,
    offset: usize,
    width: u32,
    height: u32,
    stride: usize,
}

/// 对齐粒度：64 字节（render.h 的建议值）。
pub const ALIGN: usize = 64;

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

impl AlignedImage {
    /// 4 字节/像素，stride 向上对齐到 64。
    pub fn new(width: u32, height: u32) -> Self {
        let stride = align_up(width as usize * 4, ALIGN);
        let len = stride * height as usize;
        let buf = vec![0u8; len + ALIGN];
        let offset = (ALIGN - buf.as_ptr() as usize % ALIGN) % ALIGN;
        Self {
            buf,
            offset,
            width,
            height,
            stride,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    /// 整块可写区域（含行尾 padding）。
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[self.offset..self.offset + self.stride * self.height as usize]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let start = self.offset;
        let end = start + self.stride * self.height as usize;
        &mut self.buf[start..end]
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.as_mut_slice().as_mut_ptr()
    }

    /// 第 `y` 行的有效像素（不含行尾 padding）。
    pub fn row(&self, y: usize) -> &[u8] {
        let start = y * self.stride;
        &self.as_slice()[start..start + self.width as usize * 4]
    }

    /// 把第 4 个字节（mpv 的 `rgb0`/`bgr0` 里那个 "0"）统一写成 `value`。
    ///
    /// mpv 的 software 渲染不产出有意义的 alpha（实测写 0），而消费端
    /// （例如 Slint 的 `Rgba8Pixel`）通常要求 alpha=255，这一步省不掉。
    pub fn fill_alpha(&mut self, value: u8) {
        let stride = self.stride;
        let width = self.width as usize;
        for y in 0..self.height as usize {
            let row = &mut self.as_mut_slice()[y * stride..y * stride + width * 4];
            for px in row.chunks_exact_mut(4) {
                px[3] = value;
            }
        }
    }

    /// 指针与 stride 是否满足 64 字节对齐建议。
    pub fn is_aligned(&self) -> bool {
        self.stride % ALIGN == 0 && self.as_slice().as_ptr() as usize % ALIGN == 0
    }
}
