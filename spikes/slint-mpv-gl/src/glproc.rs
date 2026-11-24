// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! OpenGL 函数指针的获取。
//!
//! 硬约束：**不链 `libGL`/`libEGL`**，函数指针只能从 Slint 拿到
//! （`GraphicsAPI::NativeOpenGL { get_proc_address }`）。
//!
//! 问题是 Slint 只给一个 `&dyn Fn(&CStr) -> *const c_void`，它的生命周期被绑在
//! `set_rendering_notifier` 回调那一次调用上；而 mpv 的
//! `MPV_RENDER_PARAM_OPENGL_INIT_PARAMS.get_proc_address` 是个长期存在的 C 回调，
//! mpv 会在之后的每次 `mpv_render_context_render()` 里随时调用它。
//!
//! 处理方式：
//! 1. 在 notifier 回调期间用 [`LoaderScope`] 把 Slint 的 loader 记录在 thread-local 里
//!    （scoped：离开回调就清空，所以不存在悬垂引用）；
//! 2. 所有解析结果（函数指针）进进程级缓存，指针本身与上下文同寿命，可以长期保存；
//! 3. mpv 在 notifier 之外（或 thread-local 为空时）来要函数，退回
//!    `eglGetProcAddress` → `dlsym(RTLD_DEFAULT)` 兜底，并计数；
//!    兜底次数会在程序结束时报出来（正常应恒为 0：mpv 只在 `mpv_render_*` 调用期间取指针，
//!    而那些调用全部发生在 notifier 回调里）。

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Slint 给的回调类型（`GraphicsAPI::NativeOpenGL::get_proc_address`）。
pub type SlintLoader<'a> = &'a (dyn Fn(&CStr) -> *const c_void + 'a);

thread_local! {
    /// 当前 notifier 回调里 Slint 给的 loader；`None` 表示“不在回调里”。
    ///
    /// 生命周期在这里被擦成 `'static`：作用域由 [`LoaderScope`] 的 RAII 保证
    /// （只有回调期间是 `Some`），出作用域立刻变回 `None`，所以不会真的拿到悬垂引用。
    static ACTIVE: Cell<Option<SlintLoader<'static>>> = const { Cell::new(None) };
}

/// 只有在 notifier 回调期间非空；`Drop` 里恢复上一个值（支持嵌套）。
pub struct LoaderScope {
    previous: Option<SlintLoader<'static>>,
}

impl LoaderScope {
    pub fn new(loader: SlintLoader<'_>) -> Self {
        // SAFETY: 生命周期被暂时延长到 `'static`，但解引用只发生在作用域内
        // （`resolve()` 里从 ACTIVE 取出后立即调用），作用域结束时 `Drop` 会清空。
        let current: SlintLoader<'static> =
            unsafe { std::mem::transmute::<SlintLoader<'_>, SlintLoader<'static>>(loader) };
        let previous = ACTIVE.with(|c| c.replace(Some(current)));
        Self { previous }
    }
}

impl Drop for LoaderScope {
    fn drop(&mut self) {
        ACTIVE.with(|c| c.set(self.previous));
    }
}

fn cache() -> &'static std::sync::Mutex<HashMap<Vec<u8>, usize>> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<Vec<u8>, usize>>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

static FALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static RESOLVED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// 一共解析过多少个不同的 GL 符号（glow 自己的 + mpv 要的）。
pub fn resolved_count() -> usize {
    RESOLVED_COUNT.load(Ordering::Relaxed)
}

/// 有多少个函数指针是靠 `eglGetProcAddress`/`dlsym` 兜底拿到的（正常应为 0）。
pub fn fallback_count() -> usize {
    FALLBACK_COUNT.load(Ordering::Relaxed)
}

/// 解析一个 GL 函数名。返回 NULL 表示解析失败。
pub fn resolve(name: &CStr) -> *const c_void {
    let key = name.to_bytes();
    if let Some(&ptr) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(key) {
        return ptr as *const c_void;
    }
    let active = ACTIVE.with(|c| c.get());
    let resolved = match active {
        None => fallback(name),
        Some(loader) => {
            let ptr = loader(name);
            if ptr.is_null() { fallback(name) } else { ptr }
        }
    };
    if !resolved.is_null() {
        let inserted = cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_vec(), resolved as usize);
        if inserted.is_none() {
            RESOLVED_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
    resolved
}

/// 兜底路径：先 `eglGetProcAddress`（与 glutin/Slint 在 Wayland/EGL 下的实现一致），
/// 再 `dlsym(RTLD_DEFAULT)`（覆盖 GL 1.x 核心函数）。
fn fallback(name: &CStr) -> *const c_void {
    FALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
    // SAFETY: name 是有效的 NUL 结尾字符串；dlsym/eglGetProcAddress 返回的指针
    // 属于已加载的驱动库，与上下文同寿命。找不到时返回 NULL。
    unsafe {
        let egl = egl_get_proc_address();
        if let Some(f) = egl {
            let p = f(name.as_ptr());
            if !p.is_null() {
                return p;
            }
        }
        libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) as *const c_void
    }
}

type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;

/// `eglGetProcAddress` 从已加载的 libEGL 里取（winit/glutin 已经把它载入进程）。
unsafe fn egl_get_proc_address() -> Option<EglGetProcAddress> {
    static EGL_GPA: OnceLock<usize> = OnceLock::new();
    let addr = *EGL_GPA.get_or_init(|| {
        // SAFETY: RTLD_DEFAULT 在已加载对象里查符号；找不到返回 NULL。
        let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"eglGetProcAddress".as_ptr()) };
        p as usize
    });
    if addr == 0 {
        return None;
    }
    // SAFETY: 符号名固定，签名按 EGL 1.5 规范（EGLAPIENTRY = extern "C"）。
    Some(unsafe { std::mem::transmute::<usize, EglGetProcAddress>(addr) })
}

/// 交给 mpv 的 `get_proc_address`（`mpv_opengl_init_params` 的字段类型）。
///
/// # Safety
/// 由 libmpv 调用：`name` 必须是有效的 NUL 结尾字符串（或 NULL）。
pub unsafe extern "C" fn mpv_get_proc_address(
    _ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: 见函数文档，name 非空且以 NUL 结尾。
    let name = unsafe { CStr::from_ptr(name) };
    resolve(name) as *mut c_void
}
