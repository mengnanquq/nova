// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! **本 crate 唯一允许 `unsafe` 的模块。**
//!
//! 里面装三样东西：
//!
//! 1. [`Handle`]：`mpv_handle` 的薄封装（option / property / command / 事件）。
//! 2. [`RenderContext`]：`mpv_render_context`（OpenGL render API）的封装与生命周期。
//! 3. GL 函数指针的装载（[`load_gl_functions`]）与 mpv 要的 `get_proc_address` 回调
//!    （[`mpv_get_proc_address`]）。**不链接** `libGL`/`libEGL`：所有入口点都从宿主给的
//!    loader（Slint 的 `GraphicsAPI::NativeOpenGL::get_proc_address`）解析。
//!
//! 为什么不直接用 safe 封装 `libmpv2 = "7.0.1"`：它的 `RenderParamApiType` 只有 `OpenGl`、
//! `RenderParam` 里没有 `OPENGL_INIT_PARAMS`，且 `render()` 把目标参数硬编码成 FBO+FlipY，
//! 无法把 `DRM_DISPLAY_V2` 交给 mpv（VAAPI 零拷贝必需）。
//!
//! # 生命周期契约（ADR-0001「复用前必须加固」逐条落地）
//!
//! - [`RenderContext`] 用显式 `impl Drop`：**先**注销 update 回调、**再**释放 context、
//!   最后才让 `Arc<FrameSignal>` 落地；不依赖字段声明顺序。
//! - [`Handle`] 的 `Drop` 只做 `mpv_terminate_destroy`，调用方（`engine`）保证 render
//!   context 已经在此之前释放（render.h 要求）。
//! - 所有来自 libmpv 的指针在用之前判空；`mpv_event_*`/`mpv_node` 的数据只在事件有效期内
//!   读取并立刻拷贝成 Rust 拥有所有权的类型。
//! - 尺寸 → 字节数的乘法一律 `checked_mul`（见 `gl` 模块），这里对 `c_int` 转换做上界检查。

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub(crate) use libmpv2_sys as raw;

/// 把 libmpv 的错误码转成可读字符串（纯诊断用，绝不进界面文案）。
pub(crate) fn error_string(code: c_int) -> String {
    // SAFETY: mpv_error_string() 对任意 c_int 都返回指向静态 C 字符串的非空指针，
    // 不需要也不允许释放。
    let p = unsafe { raw::mpv_error_string(code) };
    if p.is_null() {
        return format!("error {code}");
    }
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

/// 一条 libmpv 日志（已拷贝成 Rust 字符串）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogLine {
    pub prefix: String,
    pub level: String,
    pub text: String,
}

/// `MPV_EVENT_*` 里本引擎关心的子集，字段已全部拷贝出 libmpv 的所有权范围。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MpvEvent {
    StartFile,
    FileLoaded,
    VideoReconfig,
    PlaybackRestart,
    Idle,
    EndFile {
        reason: EndFileReason,
        /// `MPV_END_FILE_REASON_ERROR` 时是 `MPV_ERROR_*` 码，其余为 0。
        error: c_int,
    },
    PropertyChange {
        id: u64,
        value: PropValue,
    },
    Log(LogLine),
    Other(u32),
}

/// `mpv_end_file_reason`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndFileReason {
    /// 正常播放到结尾。
    Eof,
    /// 被 `loadfile` / `stop` 打断（换片、关闭）。
    Stop,
    /// 播放中途出错。
    Error,
    /// 其他（重定向、quit 等）。
    Other,
}

/// 被观察属性的值（已转换成本 crate 自己的类型）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PropValue {
    /// 属性当前不可用（mpv 给 `MPV_FORMAT_NONE`）。
    Unavailable,
    Flag(bool),
    Double(f64),
    Int(i64),
    String(String),
    /// `track-list` 之类的 NODE：这里只解析成轨道列表。
    Tracks(Vec<RawTrack>),
}

/// 轨道类型（mpv 的 `track-list[i].type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

/// `track-list` 里的一条轨道（已拷贝成 Rust 类型，不含任何指针）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawTrack {
    pub id: u32,
    pub kind: TrackKind,
    pub codec: String,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_external: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub channels: Option<u16>,
    pub sample_rate: Option<u32>,
    pub frame_rate: Option<f32>,
}

/// 一个 mpv core 的句柄（`mpv_handle`）。
///
/// libmpv 的 client API 是线程安全的（client.h「Threading」）：除 `mpv_wait_event` 之外的
/// 调用可以来自任意线程。本 crate 只让 worker 线程调 `wait_event`，GL 线程只用它创建/释放
/// render context，因此这个 `Send + Sync` 是有依据的。
pub(crate) struct Handle {
    ctx: *mut raw::mpv_handle,
}

// SAFETY: 见类型文档。mpv_handle 由 libmpv 内部加锁，本 crate 保证同一时刻只有一个线程
// 在 wait_event（worker 线程），其他调用（property/command/render context 创建）都只是
// 一次性的 FFI 调用。
unsafe impl Send for Handle {}
// SAFETY: 同上。
unsafe impl Sync for Handle {}

impl Handle {
    pub(crate) fn create() -> Result<Self, String> {
        // SAFETY: 无入参，只分配一个新的 mpv core；返回 NULL 表示失败（OOM）。
        let ctx = unsafe { raw::mpv_create() };
        if ctx.is_null() {
            return Err("mpv_create() 返回 NULL".to_string());
        }
        Ok(Self { ctx })
    }

    /// 设置 option。必须在 [`Handle::initialize`] 之前调用（之后同名项变成属性）。
    pub(crate) fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let cname = CString::new(name).map_err(|e| format!("option 名非法: {e}"))?;
        let cvalue = CString::new(value).map_err(|e| format!("option 值非法: {e}"))?;
        // SAFETY: 两个指针都指向本次调用期间存活的 NUL 结尾字符串；初始化前设置的是 option，
        // libmpv 会复制其内容，不保留指针。
        check(
            unsafe { raw::mpv_set_option_string(self.ctx, cname.as_ptr(), cvalue.as_ptr()) },
            &format!("mpv_set_option_string({name})"),
        )
    }

    pub(crate) fn initialize(&self) -> Result<(), String> {
        // SAFETY: self.ctx 来自 mpv_create()，且只在 Drop 里 destroy，此处必然有效。
        check(unsafe { raw::mpv_initialize(self.ctx) }, "mpv_initialize")
    }

    /// 让 libmpv 把日志以 `MPV_EVENT_LOG_MESSAGE` 发过来（libmpv 默认不发）。
    pub(crate) fn request_log_messages(&self, min_level: &str) -> Result<(), String> {
        let level = CString::new(min_level).map_err(|e| format!("日志级别非法: {e}"))?;
        // SAFETY: level 是本次调用期间存活的 NUL 结尾字符串；libmpv 复制其内容。
        check(
            unsafe { raw::mpv_request_log_messages(self.ctx, level.as_ptr()) },
            "mpv_request_log_messages",
        )
    }

    /// 执行一条命令（参数用 `&str`，内部转成 NUL 结尾数组）。
    pub(crate) fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args
            .iter()
            .map(|a| CString::new(*a).map_err(|e| format!("命令参数非法: {e}")))
            .collect::<Result<_, _>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null()); // libmpv 要求 NULL 结尾的 char* 数组
        // SAFETY: ptrs 以 NULL 结尾，其余元素指向 owned 里的 NUL 结尾字符串；
        // owned 活到调用结束，libmpv 不会保留这些指针。
        check(
            unsafe { raw::mpv_command(self.ctx, ptrs.as_mut_ptr()) },
            "mpv_command",
        )
    }

    /// 设属性（字符串形式；mpv 自己按属性的类型解析）。
    pub(crate) fn set_property_string(&self, name: &str, value: &str) -> Result<(), String> {
        let name = CString::new(name).map_err(|e| format!("属性名非法: {e}"))?;
        let value = CString::new(value).map_err(|e| format!("属性值非法: {e}"))?;
        // SAFETY: 两个 NUL 结尾字符串在调用期间有效；libmpv 复制属性值。
        check(
            unsafe { raw::mpv_set_property_string(self.ctx, name.as_ptr(), value.as_ptr()) },
            &format!(
                "mpv_set_property_string({name_str})",
                name_str = name.to_string_lossy()
            ),
        )
    }

    /// 读字符串属性；属性不存在或无法转换时返回 `None`。
    pub(crate) fn get_string(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        // SAFETY: name 是存活的 NUL 结尾字符串；返回 NULL 表示属性不存在/不可转换。
        let p = unsafe { raw::mpv_get_property_string(self.ctx, name.as_ptr()) };
        if p.is_null() {
            return None;
        }
        // SAFETY: 非空时 libmpv 返回堆分配的 NUL 结尾字符串，所有权交给调用方。
        let text = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        // SAFETY: 该内存由 mpv_get_property_string 分配，必须用 mpv_free 释放一次，
        // 释放后不再使用 p。
        unsafe { raw::mpv_free(p as *mut c_void) };
        Some(text)
    }

    fn get_num(&self, name: &str, format: raw::mpv_format, out: *mut c_void) -> Option<()> {
        let name = CString::new(name).ok()?;
        // SAFETY: `out` 指向调用方栈上的 i64/f64，与 `format` 匹配；name 是存活的 C 字符串。
        let rc = unsafe { raw::mpv_get_property(self.ctx, name.as_ptr(), format, out) };
        (rc >= 0).then_some(())
    }

    pub(crate) fn get_i64(&self, name: &str) -> Option<i64> {
        let mut out: i64 = 0;
        self.get_num(
            name,
            raw::mpv_format_MPV_FORMAT_INT64,
            &mut out as *mut i64 as *mut c_void,
        )?;
        Some(out)
    }

    /// 观察一个属性；`id` 会作为事件的 `reply_userdata` 回来。
    pub(crate) fn observe(
        &self,
        id: u64,
        name: &str,
        format: raw::mpv_format,
    ) -> Result<(), String> {
        let name = CString::new(name).map_err(|e| format!("属性名非法: {e}"))?;
        // SAFETY: name 是存活的 NUL 结尾字符串；libmpv 复制属性名。
        check(
            unsafe { raw::mpv_observe_property(self.ctx, id, name.as_ptr(), format) },
            &format!(
                "mpv_observe_property({name_str})",
                name_str = name.to_string_lossy()
            ),
        )
    }

    /// 读整个 `track-list`（`MPV_FORMAT_NODE`），解析成 Rust 类型后立刻释放 node 内容。
    pub(crate) fn track_list(&self) -> Option<Vec<RawTrack>> {
        let name = CString::new("track-list").ok()?;
        // mpv_node 是 POD：全零等于 MPV_FORMAT_NONE（=0），是合法的初值。
        let mut node: raw::mpv_node = raw::mpv_node {
            u: raw::mpv_node__bindgen_ty_1 { int64: 0 },
            format: raw::mpv_format_MPV_FORMAT_NONE,
        };
        // SAFETY: node 在调用期间有效且可写；MPV_FORMAT_NODE 要求 data 指向 mpv_node。
        let rc = unsafe {
            raw::mpv_get_property(
                self.ctx,
                name.as_ptr(),
                raw::mpv_format_MPV_FORMAT_NODE,
                &mut node as *mut raw::mpv_node as *mut c_void,
            )
        };
        if rc < 0 {
            return None;
        }
        // SAFETY: 上面拿到的 node 由 mpv 分配内部存储，只在 mpv_free_node_contents 之前有效；
        // parse_track_list 只读它并在返回前把每个字段拷贝成 String/u32。
        let tracks = unsafe { parse_track_list(&node) };
        // SAFETY: node 由 mpv_get_property(MPV_FORMAT_NODE) 填充，必须且只能释放一次；
        // 之后不再触碰 node。
        unsafe { raw::mpv_free_node_contents(&mut node) };
        Some(tracks)
    }

    /// 取一个事件；`timeout` 秒内没有事件时返回 `None`。
    pub(crate) fn wait_event(&self, timeout: f64) -> Option<MpvEvent> {
        // SAFETY: self.ctx 有效；返回值由 libmpv 拥有，只在下次 mpv_wait_event 前有效，
        // 所以这里立刻把需要的字段拷贝出来，不保存指针。
        let ev = unsafe { raw::mpv_wait_event(self.ctx, timeout) };
        if ev.is_null() {
            return None;
        }
        // SAFETY: 非空指针指向 libmpv 拥有的 mpv_event，在本函数内保持有效。
        let ev = unsafe { &*ev };
        let id = ev.event_id;
        if id == raw::mpv_event_id_MPV_EVENT_NONE {
            return None;
        }
        Some(match id {
            raw::mpv_event_id_MPV_EVENT_START_FILE => MpvEvent::StartFile,
            raw::mpv_event_id_MPV_EVENT_FILE_LOADED => MpvEvent::FileLoaded,
            raw::mpv_event_id_MPV_EVENT_VIDEO_RECONFIG => MpvEvent::VideoReconfig,
            raw::mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => MpvEvent::PlaybackRestart,
            raw::mpv_event_id_MPV_EVENT_IDLE => MpvEvent::Idle,
            raw::mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                if ev.data.is_null() {
                    return Some(MpvEvent::Other(id));
                }
                // SAFETY: LOG_MESSAGE 的 data 指向 mpv_event_log_message；三个字符串字段都由
                // libmpv 拥有，只在本次事件有效期内可读，所以立刻拷成 String。
                let m = unsafe { &*(ev.data as *const raw::mpv_event_log_message) };
                MpvEvent::Log(LogLine {
                    prefix: cstr_lossy(m.prefix),
                    level: cstr_lossy(m.level),
                    text: cstr_lossy(m.text),
                })
            }
            raw::mpv_event_id_MPV_EVENT_END_FILE => {
                if ev.data.is_null() {
                    // 协议上 data 必然指向 mpv_event_end_file，但解引用空指针是 UB；
                    // 宁可当成"未知结束"，也不在这里崩溃。
                    return Some(MpvEvent::EndFile {
                        reason: EndFileReason::Other,
                        error: 0,
                    });
                }
                // SAFETY: END_FILE 的 data 按 client.h 约定指向 mpv_event_end_file；
                // 只读两个标量字段，指针不越出本次事件的有效期。
                let ef = unsafe { &*(ev.data as *const raw::mpv_event_end_file) };
                let reason = if ef.reason == raw::mpv_end_file_reason_MPV_END_FILE_REASON_EOF {
                    EndFileReason::Eof
                } else if ef.reason == raw::mpv_end_file_reason_MPV_END_FILE_REASON_STOP {
                    EndFileReason::Stop
                } else if ef.reason == raw::mpv_end_file_reason_MPV_END_FILE_REASON_ERROR {
                    EndFileReason::Error
                } else {
                    EndFileReason::Other
                };
                MpvEvent::EndFile {
                    reason,
                    error: ef.error,
                }
            }
            raw::mpv_event_id_MPV_EVENT_PROPERTY_CHANGE => {
                if ev.data.is_null() {
                    return Some(MpvEvent::Other(id));
                }
                // SAFETY: PROPERTY_CHANGE 的 data 指向 mpv_event_property，仅本函数内有效。
                let prop = unsafe { &*(ev.data as *const raw::mpv_event_property) };
                // SAFETY: prop.data 的布局由 prop.format 决定（见 client.h），
                // convert_property 逐种情况判空后读取。
                let value = unsafe { convert_property(prop) };
                MpvEvent::PropertyChange {
                    id: ev.reply_userdata,
                    value,
                }
            }
            other => MpvEvent::Other(other),
        })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        // SAFETY: ctx 由 mpv_create() 创建且尚未销毁。调用方（engine::shutdown）保证
        // render context 已经在**调用本函数之前**释放（render.h 的硬要求）。
        // mpv_terminate_destroy 会阻塞到 core 线程退出并销毁对象。
        unsafe { raw::mpv_terminate_destroy(self.ctx) };
        self.ctx = ptr::null_mut();
    }
}

/// 把可能为 NULL 的 C 字符串转成 `String`（诊断用，丢字符不致命）。
fn cstr_lossy(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: 调用方保证非空指针指向 NUL 结尾的字符串。
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// 观察到的属性值 → [`PropValue`]。
///
/// # Safety
/// 调用方保证 `prop.data` 与 `prop.format` 匹配（client.h「Property expansion」）。
unsafe fn convert_property(prop: &raw::mpv_event_property) -> PropValue {
    if prop.data.is_null() || prop.format == raw::mpv_format_MPV_FORMAT_NONE {
        return PropValue::Unavailable;
    }
    if prop.format == raw::mpv_format_MPV_FORMAT_FLAG {
        // SAFETY: MPV_FORMAT_FLAG 的 data 指向 int（非 0 为真）。
        let flag = unsafe { *(prop.data as *const c_int) };
        return PropValue::Flag(flag != 0);
    }
    if prop.format == raw::mpv_format_MPV_FORMAT_DOUBLE {
        // SAFETY: MPV_FORMAT_DOUBLE 的 data 指向 f64。
        return PropValue::Double(unsafe { *(prop.data as *const f64) });
    }
    if prop.format == raw::mpv_format_MPV_FORMAT_INT64 {
        // SAFETY: MPV_FORMAT_INT64 的 data 指向 i64。
        return PropValue::Int(unsafe { *(prop.data as *const i64) });
    }
    if prop.format == raw::mpv_format_MPV_FORMAT_STRING {
        // SAFETY: MPV_FORMAT_STRING 的 data 指向 char*（client.h 明确：
        // `char *value = *(char **)(event_property->data)`）。
        let s = unsafe { *(prop.data as *const *const c_char) };
        return PropValue::String(cstr_lossy(s));
    }
    if prop.format == raw::mpv_format_MPV_FORMAT_NODE {
        // SAFETY: MPV_FORMAT_NODE 的 data 指向 mpv_node，只在本次事件内有效。
        return PropValue::Tracks(unsafe { parse_track_list(prop.data as *const raw::mpv_node) });
    }
    PropValue::Unavailable
}

/// 把 `MPV_FORMAT_NODE` 的 `track-list` 拷贝成 `Vec<RawTrack>`。
///
/// # Safety
/// `node` 必须指向一个有效的 `mpv_node`（由 mpv 拥有，调用期间有效）。
unsafe fn parse_track_list(node: *const raw::mpv_node) -> Vec<RawTrack> {
    if node.is_null() {
        return Vec::new();
    }
    // SAFETY: 调用方保证 node 非空且有效；只读 format 字段。
    let root = unsafe { &*node };
    if root.format != raw::mpv_format_MPV_FORMAT_NODE_ARRAY {
        return Vec::new();
    }
    // SAFETY: root 是有效的 mpv_node（调用方保证），只读 format 与 u.list。
    let Some((values, num)) = (unsafe { node_list(root) }) else {
        return Vec::new();
    };
    let mut tracks = Vec::with_capacity(num);
    for index in 0..num {
        // SAFETY: node_list 返回的 values 区间长度为 num，index < num，所以指针有效。
        let item = unsafe { values.add(index) };
        // SAFETY: item 是数组内的合法元素。
        // SAFETY: item 指向数组内合法元素；node_map 只读 map 的键值指针。
        let Some((keys, values, count)) = (unsafe { node_map(&*item) }) else {
            continue;
        };
        let mut track = RawTrack {
            id: 0,
            kind: TrackKind::Other,
            codec: String::new(),
            lang: None,
            title: None,
            is_default: false,
            is_forced: false,
            is_external: false,
            width: None,
            height: None,
            channels: None,
            sample_rate: None,
            frame_rate: None,
        };
        for field in 0..count {
            let Some((key, value)) = (unsafe { map_entry(keys, values, count, field) }) else {
                continue;
            };
            match key.as_str() {
                "id" => {
                    if let Some(v) = unsafe { node_i64(value) } {
                        track.id = u32::try_from(v).unwrap_or(0);
                    }
                }
                "type" => {
                    track.kind = match unsafe { node_string(value) }.as_deref() {
                        Some("video") => TrackKind::Video,
                        Some("audio") => TrackKind::Audio,
                        Some("sub") => TrackKind::Subtitle,
                        _ => TrackKind::Other,
                    };
                }
                "codec" => track.codec = unsafe { node_string(value) }.unwrap_or_default(),
                "lang" => track.lang = unsafe { node_string(value) },
                "title" => track.title = unsafe { node_string(value) },
                "default" => track.is_default = unsafe { node_flag(value) }.unwrap_or(false),
                "forced" => track.is_forced = unsafe { node_flag(value) }.unwrap_or(false),
                "external" => track.is_external = unsafe { node_flag(value) }.unwrap_or(false),
                "demux-w" => {
                    track.width = unsafe { node_i64(value) }.and_then(|v| u32::try_from(v).ok());
                }
                "demux-h" => {
                    track.height = unsafe { node_i64(value) }.and_then(|v| u32::try_from(v).ok());
                }
                "demux-channel-count" => {
                    track.channels = unsafe { node_i64(value) }.and_then(|v| u16::try_from(v).ok());
                }
                "demux-samplerate" => {
                    track.sample_rate =
                        unsafe { node_i64(value) }.and_then(|v| u32::try_from(v).ok());
                }
                "demux-fps" => {
                    track.frame_rate = unsafe { node_double(value) }.map(|v| v as f32);
                }
                _ => {}
            }
        }
        if track.kind != TrackKind::Other {
            tracks.push(track);
        }
    }
    tracks
}

/// 读 NODE_ARRAY / NODE_MAP 的 `mpv_node_list`，返回（values 指针, 元素数）。
///
/// # Safety
/// `node` 必须指向有效的 `mpv_node`，且它的 `format` 是 NODE_ARRAY / NODE_MAP。
unsafe fn node_list(node: &raw::mpv_node) -> Option<(*const raw::mpv_node, usize)> {
    // SAFETY: 由调用方保证 format 是 NODE_ARRAY / NODE_MAP，此时 u.list 有效。
    let list = unsafe { node.u.list };
    if list.is_null() {
        return None;
    }
    // SAFETY: list 非空且由 mpv 拥有，只在当前事件/属性读取期间有效。
    let list = unsafe { &*list };
    if list.num < 0 || list.values.is_null() {
        return None;
    }
    Some((list.values, list.num as usize))
}

/// 把一个 `mpv_node` 的 NODE_MAP 列表解出来：返回（keys, values, 元素数）。
///
/// # Safety
/// `node` 必须指向有效的 `mpv_node`。
unsafe fn node_map(node: &raw::mpv_node) -> Option<(*mut *mut c_char, *mut raw::mpv_node, usize)> {
    if node.format != raw::mpv_format_MPV_FORMAT_NODE_MAP {
        return None;
    }
    // SAFETY: format 是 NODE_MAP，u.list 有效（可能为 NULL 表示空 map）。
    let list = unsafe { node.u.list };
    if list.is_null() {
        return None;
    }
    // SAFETY: 同上。
    let list = unsafe { &*list };
    if list.num < 0 || list.keys.is_null() || list.values.is_null() {
        return None;
    }
    Some((list.keys, list.values, list.num as usize))
}

/// 取 NODE_MAP 的第 `index` 个键值对（键已转成 `String`，值仍是 mpv 拥有的借用）。
///
/// # Safety
/// `keys`/`values`/`count` 必须来自同一个 `node_map` 调用，且 `index < count`；
/// 三者的有效期与本次事件相同。
unsafe fn map_entry<'a>(
    keys: *mut *mut c_char,
    values: *mut raw::mpv_node,
    count: usize,
    index: usize,
) -> Option<(String, &'a raw::mpv_node)> {
    if index >= count {
        return None;
    }
    // SAFETY: keys[0..count] 有效（mpv_node_list 的约定）。
    let key = unsafe { *keys.add(index) };
    if key.is_null() {
        return None;
    }
    // SAFETY: values[0..count] 与 keys 同源同长、index < count，所以指针有效；
    // 生命周期与 keys 相同（同一次节点读取）。
    let value = unsafe { &*values.add(index) };
    Some((cstr_lossy(key), value))
}

/// # Safety
/// `node` 必须指向有效的 `mpv_node`。
unsafe fn node_string(node: &raw::mpv_node) -> Option<String> {
    if node.format != raw::mpv_format_MPV_FORMAT_STRING {
        return None;
    }
    // SAFETY: format 是 STRING，u.string 有效（可能为 NULL）。
    let p = unsafe { node.u.string };
    if p.is_null() {
        return None;
    }
    // SAFETY: 非空且 NUL 结尾，由 mpv 拥有，当前作用域内有效。
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// # Safety
/// `node` 必须指向有效的 `mpv_node`。
unsafe fn node_i64(node: &raw::mpv_node) -> Option<i64> {
    if node.format == raw::mpv_format_MPV_FORMAT_INT64 {
        // SAFETY: format 是 INT64，u.int64 有效。
        return Some(unsafe { node.u.int64 });
    }
    if node.format == raw::mpv_format_MPV_FORMAT_DOUBLE {
        // SAFETY: format 是 DOUBLE，u.double_ 有效。
        return Some(unsafe { node.u.double_ } as i64);
    }
    None
}

/// # Safety
/// `node` 必须指向有效的 `mpv_node`。
unsafe fn node_double(node: &raw::mpv_node) -> Option<f64> {
    if node.format == raw::mpv_format_MPV_FORMAT_DOUBLE {
        // SAFETY: format 是 DOUBLE，u.double_ 有效。
        return Some(unsafe { node.u.double_ });
    }
    None
}

/// # Safety
/// `node` 必须指向有效的 `mpv_node`。
unsafe fn node_flag(node: &raw::mpv_node) -> Option<bool> {
    if node.format == raw::mpv_format_MPV_FORMAT_FLAG {
        // SAFETY: format 是 FLAG，u.flag 有效。
        return Some(unsafe { node.u.flag } != 0);
    }
    None
}

/// render context 的唤醒信号。不触碰任何 mpv API：update callback 运行在 mpv 的内部线程上，
/// 能做的只有置位一个原子量。
#[derive(Default)]
pub(crate) struct FrameSignal {
    ready: AtomicBool,
}

impl FrameSignal {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 有没有未消费的"有新帧"通知（不消费）。
    pub(crate) fn pending(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn raise(&self) {
        self.ready.store(true, Ordering::Release);
    }
}

/// libmpv 的渲染更新回调：只在 mpv 的内部线程上跑，不调用 mpv API、不 panic。
unsafe extern "C" fn update_callback(data: *mut c_void) {
    if data.is_null() {
        // 协议上不该出现；空指针时静默丢弃一次唤醒，最多让下一帧晚一次刷新。
        return;
    }
    // SAFETY: data 是 RenderContext 持有的 Arc<FrameSignal> 的裸指针，从
    // set_update_callback() 到 clear（Drop 里）期间一直存活。
    let signal = unsafe { &*(data as *const FrameSignal) };
    // 跨 FFI 边界不允许 panic：这里只有原子操作，没有可能 panic 的路径。
    signal.raise();
}

/// OpenGL 渲染（`MPV_RENDER_API_TYPE_OPENGL`）上下文。
pub(crate) struct RenderContext {
    ctx: *mut raw::mpv_render_context,
    /// 用 `Option` 持有：Drop 里显式 `take()`，让"注销回调 → 释放 context → Arc 落地"
    /// 的顺序在代码里看得见，而不是依赖字段声明顺序。
    signal: Option<Arc<FrameSignal>>,
    /// 传给 mpv 的 DRM render node fd（VAAPI interop 用）。context 释放后再关。
    render_fd: Option<std::os::fd::OwnedFd>,
}

// SAFETY: render context 必须只在持有 GL 上下文的那个线程上使用；engine 侧只在 `render()`
// （UI 的 GL 线程）与 `shutdown()`（同一线程）里调用它。`PlaybackEngine: Send` 要求
// MpvEngine 可移动，这里声明 Send 表示"可以搬到别的线程去，但搬过去之后仍然只用那个线程"。
unsafe impl Send for RenderContext {}

impl RenderContext {
    /// 创建 GL 渲染上下文。必须在第一个 VO 被创建之前调用（render.h），
    /// 且调用时 GL 上下文必须 current。
    pub(crate) fn create(
        handle: &Handle,
        render_fd: Option<std::os::fd::OwnedFd>,
    ) -> Result<Self, String> {
        let signal = Arc::new(FrameSignal::new());
        let mut init = raw::mpv_opengl_init_params {
            // mpv 会把这个回调保存下来，之后在 mpv_render_* 调用期间随时用它取函数指针。
            // 实现见本模块的 mpv_get_proc_address：缓存 + 宿主 loader 里取到的 eglGetProcAddress。
            get_proc_address: Some(mpv_get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut ctx: *mut raw::mpv_render_context = ptr::null_mut();
        // MPV_RENDER_API_TYPE_OPENGL 是静态的 NUL 结尾字节串 b"opengl\0"。
        let api_type = raw::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void;
        // VAAPI 零拷贝（hwdec=vaapi，不是 -copy）在 libmpv 模式下**必须**把 GPU 侧的原生
        // 句柄交给 mpv：mpv 的 hwdec_vaapi 通过 ra_get_native_resource("drm_params_v2") 找
        // VA display，而 libmpv_gpu.c 只在收到 MPV_RENDER_PARAM_DRM_DISPLAY_V2 时才注册它。
        // 不给 → "Could not create a VA display." + 静默退回软解（ADR-0001 坑 1）。
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
        // init / drm 指向本栈帧上的结构，mpv 在 create 期间只读它们；ctx 是输出参数，
        // 返回码检查通过后才使用其中的指针。
        let rc = unsafe {
            raw::mpv_render_context_create(&mut ctx, handle_ctx(handle), params.as_mut_ptr())
        };
        check(rc, "mpv_render_context_create(MPV_RENDER_API_TYPE_OPENGL)")?;
        if ctx.is_null() {
            return Err("mpv_render_context_create 成功但 context 为 NULL".to_string());
        }
        // SAFETY: ctx 已创建成功；回调指针是 'static fn；回调上下文是 Arc 的裸指针，
        // 只要 RenderContext 活着（直到 Drop 里清回调）就有效。
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

    pub(crate) fn signal(&self) -> Option<&Arc<FrameSignal>> {
        self.signal.as_ref()
    }

    /// 是否有新帧/需要重绘；返回值是 `mpv_render_update_flag` 位集。
    pub(crate) fn update(&self) -> u64 {
        // SAFETY: self.ctx 由 create() 成功返回，直到 Drop 前一直有效。
        unsafe { raw::mpv_render_context_update(self.ctx) }
    }

    pub(crate) fn has_new_frame(flags: u64) -> bool {
        flags & u64::from(raw::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME) != 0
    }

    /// 把当前帧渲染进 `fbo`（我们自己的 `GL_RGBA8` 纹理）。
    pub(crate) fn render_into(
        &self,
        fbo: u32,
        width: u32,
        height: u32,
        block_for_target_time: bool,
    ) -> Result<(), String> {
        // 尺寸来自视频属性（外部输入）：先守住 c_int 上界，再交给 mpv。
        if width == 0 || height == 0 {
            return Err(format!("渲染目标尺寸非法: {width}x{height}"));
        }
        if width > c_int::MAX as u32 || height > c_int::MAX as u32 || fbo > c_int::MAX as u32 {
            return Err(format!("渲染目标超出 c_int: fbo={fbo} {width}x{height}"));
        }
        let mut target = raw::mpv_opengl_fbo {
            fbo: fbo as c_int,
            w: width as c_int,
            h: height as c_int,
            // GL_RGBA8：告诉 mpv 目标纹理的内部格式（影响 dithering 决定）。
            internal_format: crate::gl::GL_RGBA8,
        };
        // 0 = 不翻转。mpv 的 FLIP_Y 只对"渲染到默认帧缓冲"有意义；我们渲染进自己的 FBO，
        // 纹理按 Slint 的 `BorrowedOpenGLTextureOrigin::TopLeft` 采样（见 `nova-app`
        // 的 `borrowed_texture`：`FLIP_Y = 0` 时纹理第 0 行就是画面顶部）。
        let mut flip: c_int = 0;
        let mut block: c_int = if block_for_target_time { 1 } else { 0 };
        let mut params = [
            raw::mpv_render_param {
                type_: raw::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&mut target as *mut raw::mpv_opengl_fbo).cast(),
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
        // SAFETY: 参数类型与 render.h / render_gl.h 一致：
        // OPENGL_FBO -> mpv_opengl_fbo*，FLIP_Y -> int*，BLOCK_FOR_TARGET_TIME -> int*。
        // 它们都指向本函数栈上、调用期间有效且可写的内存；fbo 指向的 GL 对象由调用方
        // （engine::render 里的 VideoTarget）保证存在。
        check(
            unsafe { raw::mpv_render_context_render(self.ctx, params.as_mut_ptr()) },
            "mpv_render_context_render",
        )
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        // 顺序：注销回调 → 释放 context → Arc 落地 → 关 fd。显式写出来，不靠字段顺序。
        let signal = self.signal.take();
        // SAFETY: 先清 update callback，libmpv 保证 mpv_render_context_free 返回后不会再调用
        // update_callback，因此 signal 里的 Arc 不会在回调中被访问；然后释放 render context。
        // 必须发生在 mpv_terminate_destroy 之前（render.h 硬要求，见 Handle::drop）。
        unsafe {
            raw::mpv_render_context_set_update_callback(self.ctx, None, ptr::null_mut());
            raw::mpv_render_context_free(self.ctx);
        }
        self.ctx = ptr::null_mut();
        drop(signal);
        // mpv 只把 fd 的整数值 memdup 到自己的结构体里，不持有所有权；context 释放后再关。
        drop(self.render_fd.take());
    }
}

/// 取 `mpv_handle` 的裸指针（`RenderContext::create` 用）。
fn handle_ctx(handle: &Handle) -> *mut raw::mpv_handle {
    handle.ctx
}

// ---------------------------------------------------------------------------
// GL 函数指针：装载 + 给 mpv 的 get_proc_address
// ---------------------------------------------------------------------------

/// 只读地看一眼 GL 返回的静态字符串（`glGetString` 之类）。
///
/// GL 规范保证这些字符串是 NUL 结尾、与上下文同寿命的静态数据；这里只做一次拷贝。
pub(crate) fn gl_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: GL 规范保证非空返回值是 NUL 结尾的静态字符串，与上下文同寿命；
    // 本函数只读取并拷贝，不保留引用。
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// 解析结果的缓存：符号名 → 函数指针（以 usize 存，避免把裸指针放进静态变量）。
fn proc_cache() -> &'static Mutex<HashMap<Vec<u8>, usize>> {
    static CACHE: OnceLock<Mutex<HashMap<Vec<u8>, usize>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 宿主 loader 里取到的 `eglGetProcAddress`（不链接 libEGL）。
static EGL_RESOLVER: AtomicUsize = AtomicUsize::new(0);
/// 靠 `eglGetProcAddress` 兜底解析的次数（正常路径全部命中缓存）。
static RESOLVER_FALLBACKS: AtomicUsize = AtomicUsize::new(0);

/// 记录宿主 loader 能给出的 `eglGetProcAddress`，供 mpv 之后（离开 loader 作用域）取符号。
pub(crate) fn install_egl_resolver(ptr: *const c_void) {
    EGL_RESOLVER.store(ptr as usize, Ordering::Release);
}

/// 解析一个 GL 符号：缓存 → `eglGetProcAddress`。返回 NULL 表示解析失败。
pub(crate) fn resolve(name: &CStr) -> *const c_void {
    let key = name.to_bytes();
    if let Some(&ptr) = proc_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
    {
        return ptr as *const c_void;
    }
    let resolver = EGL_RESOLVER.load(Ordering::Acquire);
    if resolver == 0 {
        return ptr::null();
    }
    RESOLVER_FALLBACKS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: resolver 是宿主 loader 交给我们的 eglGetProcAddress（EGL 1.5 签名：
    // `void* (*)(const char*)`，EGLAPIENTRY = default calling convention on Unix）。
    // name 是有效的 NUL 结尾字符串；返回指针属于已加载的驱动库，与上下文同寿命。
    let resolved = unsafe { call_egl_resolver(resolver, name.as_ptr()) };
    if resolved.is_null() {
        return ptr::null();
    }
    proc_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.to_vec(), resolved as usize);
    resolved
}

type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;

/// # Safety
/// `resolver` 必须来自 `eglGetProcAddress` 的地址，`name` 必须非空且 NUL 结尾。
unsafe fn call_egl_resolver(resolver: usize, name: *const c_char) -> *const c_void {
    // SAFETY: 见函数文档；签名的正确性由调用方（install_egl_resolver 的宿主）保证。
    let f: EglGetProcAddress = unsafe { std::mem::transmute::<usize, EglGetProcAddress>(resolver) };
    // SAFETY: 同上。
    unsafe { f(name) as *const c_void }
}

/// 交给 mpv 的 `get_proc_address`（`mpv_opengl_init_params` 的字段类型）。
///
/// # Safety
/// 由 libmpv 调用：`name` 必须是有效的 NUL 结尾字符串（或 NULL）。
unsafe extern "C" fn mpv_get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if name.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: 见函数文档，name 非空且以 NUL 结尾。
    let name = unsafe { CStr::from_ptr(name) };
    resolve(name) as *mut c_void
}

/// 从宿主 loader 里解析一个符号（`GlFunctions` 装载用）。
///
/// # Safety
/// `f` 必须与符号名对应的 GL/EGL 签名一致；本模块是唯一的 `unsafe` 集中区，具体签名由
/// `crate::gl::GlFunctions` 的字段类型声明。
unsafe fn load_required(
    loader: &dyn Fn(&CStr) -> *const c_void,
    name: &CStr,
) -> Result<usize, String> {
    let ptr = loader(name);
    if ptr.is_null() {
        // loader 拿不到时退回 eglGetProcAddress（宿主可能只实现了一半）。
        let fallback = resolve(name);
        if fallback.is_null() {
            return Err(format!(
                "GL 入口点 {} 解析失败（宿主 loader 与 eglGetProcAddress 都拿不到）",
                name.to_string_lossy()
            ));
        }
        return Ok(fallback as usize);
    }
    let key = name.to_bytes();
    proc_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.to_vec(), ptr as usize);
    Ok(ptr as usize)
}

/// 装载 [`crate::gl::GlFunctions`] 需要的全部入口点，并装好给 mpv 的兜底解析器。
pub(crate) fn load_gl_functions(
    loader: &dyn Fn(&CStr) -> *const c_void,
) -> Result<crate::gl::GlFunctions, String> {
    // 先记下 eglGetProcAddress：mpv 在 `mpv_render_context_create` 之后还会来要符号，
    // 而那时宿主的 loader 已经出作用域了。
    let egl = loader(c"eglGetProcAddress");
    if !egl.is_null() {
        install_egl_resolver(egl);
    }
    let mut raw = RawGlTable::default();
    macro_rules! load {
        ($field:ident, $name:literal) => {
            raw.$field = unsafe { load_required(loader, $name)? };
        };
    }
    load!(gen_textures, c"glGenTextures");
    load!(delete_textures, c"glDeleteTextures");
    load!(bind_texture, c"glBindTexture");
    load!(tex_image_2d, c"glTexImage2D");
    load!(tex_parameter_i32, c"glTexParameteri");
    load!(gen_framebuffers, c"glGenFramebuffers");
    load!(delete_framebuffers, c"glDeleteFramebuffers");
    load!(bind_framebuffer, c"glBindFramebuffer");
    load!(framebuffer_texture_2d, c"glFramebufferTexture2D");
    load!(check_framebuffer_status, c"glCheckFramebufferStatus");
    load!(get_integer_v, c"glGetIntegerv");
    load!(get_float_v, c"glGetFloatv");
    load!(is_enabled, c"glIsEnabled");
    load!(enable, c"glEnable");
    load!(disable, c"glDisable");
    load!(viewport, c"glViewport");
    load!(scissor, c"glScissor");
    load!(blend_func_separate, c"glBlendFuncSeparate");
    load!(blend_equation_separate, c"glBlendEquationSeparate");
    load!(clear_color, c"glClearColor");
    load!(active_texture, c"glActiveTexture");
    load!(get_string, c"glGetString");
    // SAFETY: 每个字段的签名都由 crate::gl::GlFunctions 的字段类型与 GL 规范一一对应，
    // load_required 只返回宿主 loader / eglGetProcAddress 给出的该符号的真实地址。
    Ok(unsafe { build_gl_functions(raw) })
}

/// 用调用方给的 `eglGetProcAddress` 装载 GL 入口点（集成测试 / 非 Slint 宿主用）。
///
/// # Safety
/// `get_proc_address` 必须是当前进程里有效的 `eglGetProcAddress`，且调用时 GL 上下文
/// 必须 current。签名一致性由调用方保证（`GlFunctions` 的字段类型即签名声明）。
pub(crate) fn load_gl_functions_from_egl(
    get_proc_address: unsafe extern "C" fn(*const c_char) -> *mut c_void,
) -> Result<crate::gl::GlFunctions, String> {
    let loader = move |name: &CStr| -> *const c_void {
        // SAFETY: 见本函数的 `# Safety`；name 是本 crate 传入的 NUL 结尾字符串。
        unsafe { get_proc_address(name.as_ptr()) as *const c_void }
    };
    load_gl_functions(&loader)
}

/// 装载阶段的裸地址表（全部按 `usize` 存，避免在函数体内出现未初始化的函数指针）。
#[derive(Default)]
pub(crate) struct RawGlTable {
    pub gen_textures: usize,
    pub delete_textures: usize,
    pub bind_texture: usize,
    pub tex_image_2d: usize,
    pub tex_parameter_i32: usize,
    pub gen_framebuffers: usize,
    pub delete_framebuffers: usize,
    pub bind_framebuffer: usize,
    pub framebuffer_texture_2d: usize,
    pub check_framebuffer_status: usize,
    pub get_integer_v: usize,
    pub get_float_v: usize,
    pub is_enabled: usize,
    pub enable: usize,
    pub disable: usize,
    pub viewport: usize,
    pub scissor: usize,
    pub blend_func_separate: usize,
    pub blend_equation_separate: usize,
    pub clear_color: usize,
    pub active_texture: usize,
    pub get_string: usize,
}

/// 把装载阶段的结果转换成 [`crate::gl::GlFunctions`]。
///
/// # Safety
/// `raw` 里的每个字段都必须是对应 GL 符号的真实地址（`load_gl_functions` 保证这一点）。
pub(crate) unsafe fn build_gl_functions(raw: RawGlTable) -> crate::gl::GlFunctions {
    /// 把一个绝对地址转成声明的函数指针类型。
    ///
    /// # Safety
    /// `addr` 必须指向签名与 `$ty` 完全一致的函数。
    macro_rules! cast {
        ($addr:expr, $ty:ty) => {{
            // SAFETY: 见外层函数的要求；调用方保证地址与签名的对应关系。
            unsafe { std::mem::transmute::<usize, $ty>($addr) }
        }};
    }
    crate::gl::GlFunctions {
        gen_textures: cast!(raw.gen_textures, extern "C" fn(c_int, *mut u32)),
        delete_textures: cast!(raw.delete_textures, extern "C" fn(c_int, *const u32)),
        bind_texture: cast!(raw.bind_texture, extern "C" fn(u32, u32)),
        tex_image_2d: cast!(
            raw.tex_image_2d,
            extern "C" fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void)
        ),
        tex_parameter_i32: cast!(raw.tex_parameter_i32, extern "C" fn(u32, u32, c_int)),
        gen_framebuffers: cast!(raw.gen_framebuffers, extern "C" fn(c_int, *mut u32)),
        delete_framebuffers: cast!(raw.delete_framebuffers, extern "C" fn(c_int, *const u32)),
        bind_framebuffer: cast!(raw.bind_framebuffer, extern "C" fn(u32, u32)),
        framebuffer_texture_2d: cast!(
            raw.framebuffer_texture_2d,
            extern "C" fn(u32, u32, u32, u32, c_int)
        ),
        check_framebuffer_status: cast!(raw.check_framebuffer_status, extern "C" fn(u32) -> u32),
        get_integer_v: cast!(raw.get_integer_v, extern "C" fn(u32, *mut c_int)),
        get_float_v: cast!(raw.get_float_v, extern "C" fn(u32, *mut f32)),
        is_enabled: cast!(raw.is_enabled, extern "C" fn(u32) -> u8),
        enable: cast!(raw.enable, extern "C" fn(u32)),
        disable: cast!(raw.disable, extern "C" fn(u32)),
        viewport: cast!(raw.viewport, extern "C" fn(c_int, c_int, c_int, c_int)),
        scissor: cast!(raw.scissor, extern "C" fn(c_int, c_int, c_int, c_int)),
        blend_func_separate: cast!(raw.blend_func_separate, extern "C" fn(u32, u32, u32, u32)),
        blend_equation_separate: cast!(raw.blend_equation_separate, extern "C" fn(u32, u32)),
        clear_color: cast!(raw.clear_color, extern "C" fn(f32, f32, f32, f32)),
        active_texture: cast!(raw.active_texture, extern "C" fn(u32)),
        get_string: cast!(raw.get_string, extern "C" fn(u32) -> *const u8),
    }
}
