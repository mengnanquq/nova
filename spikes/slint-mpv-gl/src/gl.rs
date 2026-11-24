// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 我们自己持有的 GL 资源：视频纹理 + FBO、GL 状态保存/恢复、窗口回读与像素统计。
//!
//! 所有 GL 入口都通过 `glow` 调用，函数指针由 Slint 的 `get_proc_address` 提供
//! （见 `glproc.rs`），**不链接 `libGL`**。

use std::path::Path;

use glow::HasContext;

use crate::glproc;

/// 一个"当前 GL 上下文"的包装。
pub struct GlContext {
    gl: glow::Context,
}

/// `glGetString` 拿到的身份信息（用来证明确实跑在 B580 上而不是 llvmpipe）。
#[derive(Debug, Clone)]
pub struct GlInfo {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    pub shading_language_version: String,
    pub is_embedded: bool,
}

impl GlContext {
    /// 装载 GL 入口点。**必须在 Slint 的 notifier 回调里调用**（此时上下文 current、
    /// `GraphicsAPI::NativeOpenGL` 给的 loader 有效）。
    pub fn load(loader: glproc::SlintLoader<'_>) -> Result<Self, String> {
        // glow 在 glGetString(GL_VERSION) 返回 NULL 时会 panic；这里处在跨 FFI 的渲染
        // 回调里，panic 不能穿出去，所以先自己探一下上下文是否可用。
        let get_string = loader(c"glGetString");
        if get_string.is_null() {
            return Err("Slint 的 get_proc_address 拿不到 glGetString".to_string());
        }
        // SAFETY: 指针来自当前 GL 实现，签名与 glGetString 一致。
        let get_string: unsafe extern "C" fn(u32) -> *const u8 =
            unsafe { std::mem::transmute(get_string) };
        let version_ptr = unsafe { get_string(glow::VERSION) };
        if version_ptr.is_null() {
            return Err("glGetString(GL_VERSION) 返回 NULL（上下文不可用？）".to_string());
        }
        let _scope = glproc::LoaderScope::new(loader);
        // SAFETY: 上面已验证上下文有效；loader 返回的函数指针都属于这个上下文。
        let gl = unsafe { glow::Context::from_loader_function_cstr(glproc::resolve) };
        Ok(Self { gl })
    }

    pub fn info(&self) -> GlInfo {
        let gl = &self.gl;
        // SAFETY: 调用方保证上下文 current（只在 notifier 回调里调用本方法）。
        unsafe {
            GlInfo {
                vendor: gl.get_parameter_string(glow::VENDOR),
                renderer: gl.get_parameter_string(glow::RENDERER),
                version: gl.get_parameter_string(glow::VERSION),
                shading_language_version: gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION),
                is_embedded: gl.version().is_embedded,
            }
        }
    }

    /// 建一张 `GL_RGBA8` 纹理 + 把颜色附件挂在 FBO 上。
    pub fn create_video_target(&self, width: u32, height: u32) -> Result<VideoTarget, String> {
        let gl = &self.gl;
        // SAFETY: 上下文 current + 参数都是常量/已校验的尺寸。
        unsafe {
            let texture = gl
                .create_texture()
                .map_err(|e| format!("glGenTextures 失败: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                VideoTarget::GL_RGBA8,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            let fbo = gl
                .create_framebuffer()
                .map_err(|e| format!("glGenFramebuffers 失败: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
                return Err(format!("FBO 不完整: 0x{status:04x}"));
            }
            Ok(VideoTarget {
                texture,
                fbo,
                width,
                height,
            })
        }
    }

    /// 删除 [`VideoTarget`] 持有的 GL 对象。必须在上下文 current 时调用。
    pub fn destroy_video_target(&self, target: VideoTarget) {
        let gl = &self.gl;
        // SAFETY: 对象由本上下文创建，且此处上下文 current；删除后不再使用。
        unsafe {
            gl.delete_framebuffer(target.fbo);
            gl.delete_texture(target.texture);
        }
    }

    /// 保存会被 mpv 改掉的那部分 GL 状态。
    pub fn save_state(&self) -> SavedState {
        let gl = &self.gl;
        // SAFETY: 只读查询，上下文 current。
        unsafe {
            let mut viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut scissor = [0i32; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor);
            let mut clear = [0f32; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
            SavedState {
                framebuffer: gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING),
                viewport,
                scissor,
                scissor_enabled: gl.is_enabled(glow::SCISSOR_TEST),
                blend_enabled: gl.is_enabled(glow::BLEND),
                clear_color: clear,
                active_texture: gl.get_parameter_i32(glow::ACTIVE_TEXTURE),
                texture_2d: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
            }
        }
    }

    /// 恢复 [`SavedState`]。render_gl.h 明确列出 mpv 会留下 glViewport / glScissor /
    /// glBlendFuncSeparate / glClearColor 等状态，所以这几项必须由调用方还原。
    pub fn restore_state(&self, state: &SavedState) {
        let gl = &self.gl;
        // SAFETY: 全是设置类调用，参数来自同上下文的查询结果。
        unsafe {
            gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                std::num::NonZeroU32::new(state.framebuffer as u32).map(glow::NativeFramebuffer),
            );
            gl.viewport(
                state.viewport[0],
                state.viewport[1],
                state.viewport[2],
                state.viewport[3],
            );
            gl.scissor(
                state.scissor[0],
                state.scissor[1],
                state.scissor[2],
                state.scissor[3],
            );
            if state.scissor_enabled {
                gl.enable(glow::SCISSOR_TEST);
            } else {
                gl.disable(glow::SCISSOR_TEST);
            }
            if state.blend_enabled {
                gl.enable(glow::BLEND);
            } else {
                gl.disable(glow::BLEND);
            }
            gl.clear_color(
                state.clear_color[0],
                state.clear_color[1],
                state.clear_color[2],
                state.clear_color[3],
            );
            gl.active_texture(state.active_texture as u32);
            gl.bind_texture(
                glow::TEXTURE_2D,
                std::num::NonZeroU32::new(state.texture_2d as u32).map(glow::NativeTexture),
            );
        }
    }

    /// 把**当前绑定的**帧缓冲读回 CPU。
    ///
    /// - 正常窗口渲染时绑定的是默认帧缓冲（窗口 back buffer），读 `GL_BACK`；
    /// - `Window::take_snapshot()` 驱动渲染时绑定的是 Slint 的离屏 FBO，读 `GL_COLOR_ATTACHMENT0`。
    ///
    /// 两种情况都发生在 `AfterRendering`（上下文 current），所以这里既能验证“窗口上真有画面”，
    /// 也能在窗口拿不到合成器帧回调（例如会话锁屏）时继续做像素验证。
    ///
    /// `flip_rows` 为真时把行序倒过来（GL 的原点在左下，PNG 的原点在左上）。
    pub fn read_bound_framebuffer(
        &self,
        width: u32,
        height: u32,
        flip_rows: bool,
    ) -> Result<FrameSample, String> {
        let gl = &self.gl;
        // 审查修复 #5：所有尺寸→字节数的乘法都走 checked_mul。
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| format!("回读缓冲 {width}x{height} 字节数溢出 usize"))?;
        if width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(format!("回读尺寸超出 i32: {width}x{height}"));
        }
        let mut buf = vec![0u8; bytes];
        let stride = width as usize * 4;
        // SAFETY: buf 的长度正好是 width*height*4；上下文 current；
        // PACK_ALIGNMENT 设为 1 以免 Mesa 对行宽做对齐假设（这里行宽本来就是 4 的倍数）。
        unsafe {
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            // 不改变 FBO 绑定：读的就是调用方（femtovg / Slint）刚渲染完的那个目标。
            let bound = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            if bound == 0 {
                gl.read_buffer(glow::BACK);
            } else {
                gl.read_buffer(glow::COLOR_ATTACHMENT0);
            }
            gl.read_pixels(
                0,
                0,
                width as i32,
                height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut buf)),
            );
        }
        if flip_rows {
            let mut flipped = vec![0u8; bytes];
            for y in 0..height as usize {
                let src = (height as usize - 1 - y) * stride;
                let dst = y * stride;
                flipped[dst..dst + stride].copy_from_slice(&buf[src..src + stride]);
            }
            buf = flipped;
        }
        Ok(FrameSample {
            width,
            height,
            stats: PixelStats::from_rgba(&buf),
            rgba: buf,
        })
    }
}

/// `GlContext::save_state` 的快照。
pub struct SavedState {
    framebuffer: i32,
    viewport: [i32; 4],
    scissor: [i32; 4],
    scissor_enabled: bool,
    blend_enabled: bool,
    clear_color: [f32; 4],
    active_texture: i32,
    texture_2d: i32,
}

/// 视频纹理 + 它的 FBO：mpv 渲染进这里，Slint 把同一张纹理合成到窗口上。
pub struct VideoTarget {
    texture: glow::Texture,
    fbo: glow::Framebuffer,
    width: u32,
    height: u32,
}

impl VideoTarget {
    /// GL 的 `GL_RGBA8`。
    pub const GL_RGBA8: i32 = 0x8058;

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// 纹理名（`glGenTextures` 的返回值），交给 Slint 的 BorrowedOpenGLTextureBuilder。
    pub fn texture_id(&self) -> u32 {
        self.texture.0.get()
    }

    /// 给 mpv 用的 FBO 名（`mpv_opengl_fbo::fbo`）。
    pub fn sys_target(&self) -> Result<crate::sys::GlTarget, String> {
        crate::sys::GlTarget::new(self.fbo.0.get(), self.width, self.height)
    }
}

/// 一帧的像素统计（亮度 = Rec.709 加权，输入按 sRGB 字节值直接算，不做线性化）。
#[derive(Debug, Clone, Copy, Default)]
pub struct PixelStats {
    pub mean_luma: f64,
    pub variance: f64,
    /// 亮度 > 16/255 的像素占比（"非黑像素比例"）。
    pub non_black_ratio: f64,
    pub mean_rgb: [f64; 3],
}

impl PixelStats {
    pub fn from_rgba(rgba: &[u8]) -> Self {
        let n = rgba.len() / 4;
        if n == 0 {
            return Self::default();
        }
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut non_black = 0usize;
        let mut rgb = [0.0f64; 3];
        for px in rgba.chunks_exact(4) {
            let r = px[0] as f64;
            let g = px[1] as f64;
            let b = px[2] as f64;
            rgb[0] += r;
            rgb[1] += g;
            rgb[2] += b;
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            sum += luma;
            sum_sq += luma * luma;
            if luma > 16.0 {
                non_black += 1;
            }
        }
        let n_f = n as f64;
        let mean = sum / n_f;
        Self {
            mean_luma: mean,
            variance: (sum_sq / n_f) - mean * mean,
            non_black_ratio: non_black as f64 / n_f,
            mean_rgb: [rgb[0] / n_f, rgb[1] / n_f, rgb[2] / n_f],
        }
    }

    pub fn looks_like_video(&self) -> bool {
        // 全黑画面：均值≈0、方差≈0。这里两个条件都要满足才算“有画面”。
        self.mean_luma > 8.0 && self.variance > 100.0
    }
}

/// 一次回读的结果。
pub struct FrameSample {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub stats: PixelStats,
}

/// 把 RGBA8（行序已在调用方统一成自顶向下）写成 PNG。
pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("PNG 尺寸 {width}x{height} 溢出"))?;
    if rgba.len() != expected {
        return Err(format!(
            "PNG 数据长度 {} 与 {width}x{height}x4={expected} 不符",
            rgba.len()
        ));
    }
    let file =
        std::fs::File::create(path).map_err(|e| format!("创建 {} 失败: {e}", path.display()))?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("PNG 头写入失败: {e}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| format!("PNG 数据写入失败: {e}"))?;
    writer.finish().map_err(|e| format!("PNG 收尾失败: {e}"))?;
    Ok(())
}
