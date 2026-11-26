// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 窗口 back buffer 的 `glReadPixels` 回读（`--play --capture` 的验证手段）。
//!
//! Slint 不暴露 `glReadPixels`，而 `nova-playback-mpv` 的 `GlFunctions` 只装渲染必需的
//! 那几个入口点（回读不属于渲染路径，放那边会把它拖进产品依赖）。所以这里自己从
//! `GraphicsAPI::NativeOpenGL::get_proc_address` 解析 4 个符号，并在**一个** `unsafe`
//! 块里把 `*const c_void` 转成逐字对应 GL 规范签名的函数指针。
//!
//! 回读时机与 `spikes/slint-mpv-gl` 一致：`RenderingState::AfterRendering`（Slint 已经把整帧
//! 画进 back buffer、还没 present），此时读到的是**窗口上真实显示的内容**。
//!
//! 这条路径依赖合成器给帧回调。没有显示器 / 锁屏时 Wayland 合成器根本不下发
//! `wl_surface.frame`，`request_redraw()` 变成空操作（实测整个进程只有首帧一次
//! `AfterRendering`），回读永远等不到。所以截图先给 [`SHOT_FRAME_TIMEOUT`] 的宽限期，
//! 超时就改走 `slint::Window::take_snapshot()`（离屏渲染同一条渲染管线，见
//! [`Frame::from_pixel_buffer`]），不需要任何帧回调或截屏协议。

use std::ffi::{CStr, c_int, c_void};
use std::time::{Duration, Instant};

use i_slint_core::graphics::{Rgba8Pixel, SharedPixelBuffer};

/// 等 `AfterRendering` 回读的宽限期：两个 60Hz 帧周期。
///
/// 有显示器时合成器每帧都回调，几十毫秒内就拿得到；超出这个时长还没帧，
/// 说明合成器不再给帧回调（无显示器 / 锁屏 / 窗口被遮挡），改成自己离屏渲染。
pub(crate) const SHOT_FRAME_TIMEOUT: Duration = Duration::from_millis(200);

/// 发起截图后过了 `elapsed` 仍未等到 `AfterRendering`（纯函数，便于单测）。
pub(crate) fn shot_deadline_passed(elapsed: Duration) -> bool {
    elapsed >= SHOT_FRAME_TIMEOUT
}

/// 一次截图请求的等待状态。
///
/// 先等帧回调（`glReadPixels` 读的是窗口真实 back buffer），等不到再改道生效一次
/// `take_snapshot()`（离屏渲染，不需要合成器配合）。
pub(crate) struct ShotWait {
    started: Instant,
    snapshot_tried: bool,
}

impl ShotWait {
    /// 从此刻开始计时。
    pub(crate) fn start() -> Self {
        Self {
            started: Instant::now(),
            snapshot_tried: false,
        }
    }

    /// 是否该改走 `take_snapshot()`：宽限期已过且还没改过道（同一个请求只返回一次 `true`）。
    pub(crate) fn snapshot_due(&mut self, now: Instant) -> bool {
        if self.snapshot_tried {
            return false;
        }
        if !shot_deadline_passed(now.saturating_duration_since(self.started)) {
            return false;
        }
        self.snapshot_tried = true;
        true
    }
}

/// `glPixelStorei(GL_PACK_ALIGNMENT, 1)`。
const GL_PACK_ALIGNMENT: u32 = 0x0D05;
/// `GL_RGBA`。
const GL_RGBA: u32 = 0x1908;
/// `GL_UNSIGNED_BYTE`。
const GL_UNSIGNED_BYTE: u32 = 0x1401;
/// `GL_FRAMEBUFFER_BINDING`。
const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
/// `GL_BACK`（窗口 back buffer）。
const GL_BACK: u32 = 0x0405;
/// `GL_COLOR_ATTACHMENT0`（离屏 FBO）。
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;

type ReadBufferFn = extern "C" fn(u32);
type ReadPixelsFn = extern "C" fn(c_int, c_int, c_int, c_int, u32, u32, *mut c_void);
type PixelStoreFn = extern "C" fn(u32, c_int);
type GetIntegerFn = extern "C" fn(u32, *mut c_int);

/// 回读需要的一组 GL 入口点。
#[derive(Clone, Copy)]
pub(crate) struct Readback {
    read_buffer: ReadBufferFn,
    read_pixels: ReadPixelsFn,
    pixel_store: PixelStoreFn,
    get_integer: GetIntegerFn,
}

/// 一帧窗口像素。
pub(crate) struct Frame {
    /// 窗口像素宽。
    pub(crate) width: u32,
    /// 窗口像素高。
    pub(crate) height: u32,
    /// 行序已翻成自顶向下（PNG 顺序）的 RGBA8。
    pub(crate) rgba: Vec<u8>,
    /// 像素统计。
    pub(crate) stats: PixelStats,
}

/// 亮度 / 方差 / 非黑像素比例（判断"真的不是全黑"）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PixelStats {
    /// Rec.709 加权平均亮度（0..255）。
    pub(crate) mean_luma: f64,
    /// 亮度方差。
    pub(crate) variance: f64,
    /// 亮度 > 16/255 的像素占比（0..1）。
    pub(crate) non_black_ratio: f64,
}

impl PixelStats {
    /// 从 RGBA8 数据统计。
    pub(crate) fn from_rgba(rgba: &[u8]) -> Self {
        let pixels = rgba.len() / 4;
        if pixels == 0 {
            return Self::default();
        }
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut non_black = 0usize;
        for px in rgba.chunks_exact(4) {
            let luma =
                0.2126 * f64::from(px[0]) + 0.7152 * f64::from(px[1]) + 0.0722 * f64::from(px[2]);
            sum += luma;
            sum_sq += luma * luma;
            if luma > 16.0 {
                non_black += 1;
            }
        }
        let count = pixels as f64;
        let mean = sum / count;
        Self {
            mean_luma: mean,
            variance: (sum_sq / count) - mean * mean,
            non_black_ratio: non_black as f64 / count,
        }
    }

    /// 是否像一段真实画面（全黑 / 纯色画面不算）。
    pub(crate) fn looks_like_video(&self) -> bool {
        self.mean_luma > 8.0 && self.variance > 100.0
    }
}

impl Frame {
    /// 从 `Window::take_snapshot()` 的像素缓冲构造一帧。
    ///
    /// 缓冲的行序已经是自顶向下（PNG 顺序），不需要像 `glReadPixels` 那样翻行；
    /// 像素是 GL 上下文里同一份场景的离屏渲染结果（视频纹理也照常合成）。
    pub(crate) fn from_pixel_buffer(
        buffer: &SharedPixelBuffer<Rgba8Pixel>,
    ) -> Result<Self, String> {
        let width = buffer.width();
        let height = buffer.height();
        if width == 0 || height == 0 {
            return Err(format!("离屏渲染尺寸非法：{width}x{height}"));
        }
        let pixels = buffer.as_slice();
        let expected = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| format!("离屏渲染尺寸 {width}x{height} 溢出 usize"))?;
        if pixels.len() != expected {
            return Err(format!(
                "离屏渲染像素数 {} 与 {width}x{height}={expected} 不符",
                pixels.len()
            ));
        }
        let mut rgba = Vec::with_capacity(
            expected
                .checked_mul(4)
                .ok_or_else(|| format!("离屏渲染字节数 {expected}x4 溢出 usize"))?,
        );
        for pixel in pixels {
            rgba.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
        }
        let stats = PixelStats::from_rgba(&rgba);
        Ok(Self {
            width,
            height,
            rgba,
            stats,
        })
    }
}

impl std::fmt::Debug for Readback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Readback { … }")
    }
}

#[allow(unsafe_code)]
fn resolve(loader: &dyn Fn(&CStr) -> *const c_void, name: &CStr) -> Option<*const c_void> {
    let pointer = loader(name);
    (!pointer.is_null()).then_some(pointer)
}

impl Readback {
    /// 解析入口点。**必须在 GL 上下文 current 时调用**（Slint 的 notifier 回调里）。
    ///
    /// 任何一个符号拿不到就返回 `None`（窗口后端不是 OpenGL 时就是这种情况），
    /// 调用方据此跳过捕获并给出可读原因。
    #[allow(unsafe_code)]
    pub(crate) fn load(loader: &dyn Fn(&CStr) -> *const c_void) -> Option<Self> {
        let read_buffer = resolve(loader, c"glReadBuffer")?;
        let read_pixels = resolve(loader, c"glReadPixels")?;
        let pixel_store = resolve(loader, c"glPixelStorei")?;
        let get_integer = resolve(loader, c"glGetIntegerv")?;
        // SAFETY: 四个符号都来自当前 GL 上下文的 loader；下面的函数指针类型逐字对应
        // `glReadBuffer(GLenum)`、`glReadPixels(GLint,GLint,GLsizei,GLsizei,GLenum,GLenum,void*)`、
        // `glPixelStorei(GLenum,GLint)`、`glGetIntegerv(GLenum,GLint*)` 的 C ABI 签名
        // （见 gl.xml）。转换后只在持有同一上下文的线程上调用。
        Some(unsafe {
            Self {
                read_buffer: std::mem::transmute::<*const c_void, ReadBufferFn>(read_buffer),
                read_pixels: std::mem::transmute::<*const c_void, ReadPixelsFn>(read_pixels),
                pixel_store: std::mem::transmute::<*const c_void, PixelStoreFn>(pixel_store),
                get_integer: std::mem::transmute::<*const c_void, GetIntegerFn>(get_integer),
            }
        })
    }

    /// 回读**当前绑定的**帧缓冲。
    ///
    /// 窗口渲染时绑定的是默认帧缓冲（读 `GL_BACK`）；Slint 用离屏 FBO 时读
    /// `GL_COLOR_ATTACHMENT0`。两种情况都在 `AfterRendering` 里发生。
    pub(crate) fn read(&self, width: u32, height: u32) -> Result<Frame, String> {
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| format!("回读 {width}x{height} 的字节数溢出 usize"))?;
        if width == 0 || height == 0 {
            return Err(format!("回读尺寸非法：{width}x{height}"));
        }
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(format!("回读尺寸超出 i32：{width}x{height}"));
        }
        let mut buffer = vec![0u8; bytes];
        let mut bound = 0i32;
        (self.get_integer)(GL_FRAMEBUFFER_BINDING, &mut bound);
        (self.pixel_store)(GL_PACK_ALIGNMENT, 1);
        if bound == 0 {
            (self.read_buffer)(GL_BACK);
        } else {
            (self.read_buffer)(GL_COLOR_ATTACHMENT0);
        }
        (self.read_pixels)(
            0,
            0,
            width as i32,
            height as i32,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            buffer.as_mut_ptr().cast(),
        );

        let stride = width as usize * 4;
        let mut flipped = vec![0u8; bytes];
        for row in 0..height as usize {
            let source = (height as usize - 1 - row) * stride;
            let target = row * stride;
            flipped[target..target + stride].copy_from_slice(&buffer[source..source + stride]);
        }
        let stats = PixelStats::from_rgba(&flipped);
        Ok(Frame {
            width,
            height,
            rgba: flipped,
            stats,
        })
    }
}

/// 把 RGBA8（自顶向下）写成 PNG。
pub(crate) fn write_png(path: &std::path::Path, frame: &Frame) -> Result<(), String> {
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("PNG 尺寸 {}x{} 溢出", frame.width, frame.height))?;
    if frame.rgba.len() != expected {
        return Err(format!(
            "PNG 数据长度 {} 与 {}x{}x4={expected} 不符",
            frame.rgba.len(),
            frame.width,
            frame.height
        ));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建目录 {} 失败：{error}", parent.display()))?;
    }
    let file = std::fs::File::create(path)
        .map_err(|error| format!("创建 {} 失败：{error}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("PNG 头写入失败：{error}"))?;
    writer
        .write_image_data(&frame.rgba)
        .map_err(|error| format!("PNG 数据写入失败：{error}"))?;
    writer
        .finish()
        .map_err(|error| format!("PNG 收尾失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_frame_is_not_video() {
        let stats = PixelStats::from_rgba(&[0u8; 16]);
        assert_eq!(stats.mean_luma, 0.0);
        assert!(!stats.looks_like_video());
    }

    #[test]
    fn shot_deadline_boundaries() {
        assert!(!shot_deadline_passed(Duration::ZERO));
        assert!(!shot_deadline_passed(
            SHOT_FRAME_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(shot_deadline_passed(SHOT_FRAME_TIMEOUT));
        assert!(shot_deadline_passed(
            SHOT_FRAME_TIMEOUT + Duration::from_secs(1)
        ));
    }

    #[test]
    fn shot_wait_falls_back_once() {
        let mut wait = ShotWait::start();
        let start = wait.started;
        // 宽限期内不改道（有显示器时这条路能直接回读）。
        assert!(!wait.snapshot_due(start + Duration::from_millis(50)));
        // 宽限期到了就改道，且只改一次。
        assert!(wait.snapshot_due(start + SHOT_FRAME_TIMEOUT));
        assert!(!wait.snapshot_due(start + SHOT_FRAME_TIMEOUT));
        assert!(!wait.snapshot_due(start + Duration::from_secs(5)));
    }

    #[test]
    fn snapshot_buffer_converts_to_frame() {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(2, 2);
        let colors = [
            Rgba8Pixel {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            Rgba8Pixel {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            },
            Rgba8Pixel {
                r: 200,
                g: 100,
                b: 50,
                a: 255,
            },
            Rgba8Pixel {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
        ];
        buffer.make_mut_slice().copy_from_slice(&colors);

        let frame = Frame::from_pixel_buffer(&buffer).expect("2x2 缓冲应当转换成功");
        assert_eq!((frame.width, frame.height), (2, 2));
        // 行序保持自顶向下（不像 glReadPixels 那样需要翻行）。
        assert_eq!(
            frame.rgba,
            vec![
                0, 0, 0, 255, 10, 20, 30, 255, 200, 100, 50, 255, 255, 255, 255, 255
            ]
        );
        assert!(frame.stats.mean_luma > 8.0);
        assert!((frame.stats.non_black_ratio - 0.75).abs() < 1e-9);
    }

    #[test]
    fn empty_snapshot_buffer_is_rejected() {
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::new(0, 0);
        assert!(Frame::from_pixel_buffer(&buffer).is_err());
    }

    #[test]
    fn gradient_frame_is_video() {
        let mut rgba = Vec::new();
        for value in 0..64u8 {
            rgba.extend_from_slice(&[value, value, value, 255]);
        }
        let stats = PixelStats::from_rgba(&rgba);
        assert!(stats.mean_luma > 8.0);
        assert!(stats.variance > 100.0);
        assert!(stats.looks_like_video());
        // 灰度像素的 Rec.709 亮度等于它的字节值：17..=63 共 47 个非黑像素。
        assert!((stats.non_black_ratio - 47.0 / 64.0).abs() < 1e-9);
    }
}
