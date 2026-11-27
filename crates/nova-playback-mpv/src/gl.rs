// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 我们自己的 GL 资源：视频纹理 + FBO，以及 GL 状态的保存/恢复。
//!
//! 这里**没有 `unsafe`**：所有入口点都是 [`GlFunctions`] 里的函数指针（由 `ffi` 模块
//! 从宿主 loader 解析并做一次类型转换），调用它们是普通函数调用。
//! **不链接** `libGL`/`libEGL`：入口点全部来自 Slint 的 `get_proc_address`
//! （`GraphicsAPI::NativeOpenGL`）。
//!
//! 为什么必须自己持有一张纹理：mpv 的 GL render API 只会把画面画进「调用方给的 FBO」，
//! 而 Slint 的 `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture` 只能包装一张
//! 8bit RGBA 纹理。所以路径固定是：
//!
//! ```text
//! mpv（vaapi 解码结果，GPU 上）→ EGL dmabuf interop → 采样进我们的 GL_RGBA8 纹理
//!   → Slint 把同一张纹理合成到窗口
//! ```
//!
//! 中间没有 GPU→CPU 回读（4K 下一次回读 16 ms/帧，比 60 Hz 的整个帧预算还多，见 ADR-0001）。

use std::ffi::{CStr, c_int, c_void};

use nova_playback::PlaybackError;

/// `GL_RGBA8`（也作为 `mpv_opengl_fbo::internal_format` 传给 mpv）。
pub(crate) const GL_RGBA8: i32 = 0x8058;

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_SCISSOR_BOX: u32 = 0x0C10;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_BLEND: u32 = 0x0BE2;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_STENCIL_TEST: u32 = 0x0B90;
const GL_CULL_FACE: u32 = 0x0B44;
const GL_BLEND_SRC_RGB: u32 = 0x80C9;
const GL_BLEND_DST_RGB: u32 = 0x80C8;
const GL_BLEND_SRC_ALPHA: u32 = 0x80CB;
const GL_BLEND_DST_ALPHA: u32 = 0x80CA;
const GL_BLEND_EQUATION_RGB: u32 = 0x8009;
const GL_BLEND_EQUATION_ALPHA: u32 = 0x883D;
const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
const GL_ACTIVE_TEXTURE: u32 = 0x84E0;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_RENDERER: u32 = 0x1F01;

/// mpv 要求调用方在进入 `mpv_render_context_render` 前保持默认值（关闭）的能力位。
///
/// `mpv/render_gl.h`（46–59 行）的「OpenGL state」一节原文要求：调用 mpv 前 GL 状态
/// 应当是 **OpenGL 标准默认值**，mpv 也会尽量把上下文留成标准默认值；**例外**只有
/// `glViewport`、`glScissor` 矩形、`glBlendFuncSeparate()` 与 `glClearColor()`
/// （这几项 mpv 自己会改、不负责还原，由 [`GlFunctions::save_state`] /
/// [`GlFunctions::restore_state`] 自己保存恢复）。除这四项之外，能力位必须是默认的
/// **关闭**状态；mpv 只在需要混合的 pass 上自己 `glEnable(GL_BLEND)`，从不主动关掉
/// 宿主遗留的混合 / 裁剪 / 深度 / 模板 / 面剔除状态。
///
/// 实测（体检 R6-1，Mesa/Intel + GLES，窗口 1920×1080）：Slint（femtovg）总是开着
/// premultiplied 混合（`ONE, ONE_MINUS_SRC_ALPHA`），mpv 的「output to screen」pass 就在
/// 混合状态下落盘——回读我们自己的 FBO 只有底部一条 96 行的画面（窗口里看到的正是那块
/// "底下一条视频、其余全黑"）；把这些能力位复位成默认值后同一帧完整落盘（3/3 对 3/3）。
const MPV_REQUIRED_GL_DEFAULTS: [(&str, u32); 5] = [
    ("GL_SCISSOR_TEST", GL_SCISSOR_TEST),
    ("GL_BLEND", GL_BLEND),
    ("GL_DEPTH_TEST", GL_DEPTH_TEST),
    ("GL_STENCIL_TEST", GL_STENCIL_TEST),
    ("GL_CULL_FACE", GL_CULL_FACE),
];

/// 一个「当前 GL 上下文」里我们需要的入口点。
///
/// 字段类型就是 GL 规范里的签名（第一次转换在 `ffi::load_gl_functions` 里做一次），
/// 这样本模块调用它们时不需要 `unsafe`。
#[derive(Clone, Copy)]
pub struct GlFunctions {
    pub(crate) gen_textures: extern "C" fn(c_int, *mut u32),
    pub(crate) delete_textures: extern "C" fn(c_int, *const u32),
    pub(crate) bind_texture: extern "C" fn(u32, u32),
    pub(crate) tex_image_2d:
        extern "C" fn(u32, c_int, c_int, c_int, c_int, c_int, u32, u32, *const c_void),
    pub(crate) tex_parameter_i32: extern "C" fn(u32, u32, c_int),
    pub(crate) gen_framebuffers: extern "C" fn(c_int, *mut u32),
    pub(crate) delete_framebuffers: extern "C" fn(c_int, *const u32),
    pub(crate) bind_framebuffer: extern "C" fn(u32, u32),
    pub(crate) framebuffer_texture_2d: extern "C" fn(u32, u32, u32, u32, c_int),
    pub(crate) check_framebuffer_status: extern "C" fn(u32) -> u32,
    pub(crate) get_integer_v: extern "C" fn(u32, *mut c_int),
    pub(crate) get_float_v: extern "C" fn(u32, *mut f32),
    pub(crate) is_enabled: extern "C" fn(u32) -> u8,
    pub(crate) enable: extern "C" fn(u32),
    pub(crate) disable: extern "C" fn(u32),
    pub(crate) viewport: extern "C" fn(c_int, c_int, c_int, c_int),
    pub(crate) scissor: extern "C" fn(c_int, c_int, c_int, c_int),
    pub(crate) blend_func_separate: extern "C" fn(u32, u32, u32, u32),
    pub(crate) blend_equation_separate: extern "C" fn(u32, u32),
    pub(crate) clear_color: extern "C" fn(f32, f32, f32, f32),
    pub(crate) active_texture: extern "C" fn(u32),
    pub(crate) get_string: extern "C" fn(u32) -> *const u8,
}

impl std::fmt::Debug for GlFunctions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GlFunctions { … }")
    }
}

impl GlFunctions {
    /// 从宿主 loader 装载入口点（必须是 Slint 的
    /// `GraphicsAPI::NativeOpenGL::get_proc_address`，或任何语义等价的
    /// `eglGetProcAddress` 包装）。
    ///
    /// **必须在 GL 上下文 current 时调用**：loader 的有效期只到 `RenderingSetup` 回调结束，
    /// 所以这里一次性把需要的入口点全部解析出来，并把 `eglGetProcAddress` 记下来供 mpv
    /// 之后（`mpv_render_context_create` 及其后的渲染）继续取符号。
    ///
    /// # Errors
    /// 有任何必需入口点解析失败时返回 [`PlaybackError::Unsupported`]。
    pub fn from_loader(loader: &dyn Fn(&CStr) -> *const c_void) -> Result<Self, PlaybackError> {
        crate::ffi::load_gl_functions(loader).map_err(PlaybackError::engine)
    }

    /// 直接从 `eglGetProcAddress` 装载（给集成测试 / 未来的非 Slint 宿主用）。
    ///
    /// # Errors
    /// 同上。
    pub fn from_egl_get_proc_address(
        get_proc_address: unsafe extern "C" fn(*const std::ffi::c_char) -> *mut c_void,
    ) -> Result<Self, PlaybackError> {
        // 真正的 `unsafe` 调用在 `ffi` 里（本 crate 只允许那一个模块写 unsafe）。
        crate::ffi::load_gl_functions_from_egl(get_proc_address).map_err(PlaybackError::engine)
    }

    /// 从 Slint 的后端 API 装载（`nova-app` 在 `RenderingState::RenderingSetup` 里用这个）。
    ///
    /// # Errors
    /// 后端不是 OpenGL（femtovg 之外的渲染器）、或入口点解析失败。
    pub fn from_slint_api(api: &slint::GraphicsAPI<'_>) -> Result<Self, PlaybackError> {
        match api {
            slint::GraphicsAPI::NativeOpenGL { get_proc_address } => {
                Self::from_loader(*get_proc_address)
            }
            other => Err(PlaybackError::engine(format!(
                "Slint 后端不是 OpenGL（{other:?}）；本引擎只支持 femtovg + GL"
            ))),
        }
    }

    /// GL_RENDERER 字符串（诊断用，不含任何 URL / 令牌）。
    pub(crate) fn renderer(&self) -> String {
        let ptr = (self.get_string)(GL_RENDERER);
        let text = gl_string(ptr);
        if text.is_empty() {
            "未知 GL 实现".to_string()
        } else {
            text
        }
    }

    /// 建一张 `GL_RGBA8` 纹理，并把视图挂到新建的 FBO 上。
    pub(crate) fn create_target(&self, width: u32, height: u32) -> Result<VideoTarget, String> {
        checked_target_size(width, height)?;
        let mut texture = 0u32;
        (self.gen_textures)(1, &mut texture);
        if texture == 0 {
            return Err("glGenTextures 返回 0".to_string());
        }
        (self.bind_texture)(GL_TEXTURE_2D, texture);
        (self.tex_image_2d)(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8,
            width as i32,
            height as i32,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            std::ptr::null(),
        );
        (self.tex_parameter_i32)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (self.tex_parameter_i32)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (self.tex_parameter_i32)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (self.tex_parameter_i32)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

        let mut fbo = 0u32;
        (self.gen_framebuffers)(1, &mut fbo);
        if fbo == 0 {
            (self.delete_textures)(1, &texture);
            (self.bind_texture)(GL_TEXTURE_2D, 0);
            return Err("glGenFramebuffers 返回 0".to_string());
        }
        (self.bind_framebuffer)(GL_FRAMEBUFFER, fbo);
        (self.framebuffer_texture_2d)(
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            texture,
            0,
        );
        let status = (self.check_framebuffer_status)(GL_FRAMEBUFFER);
        (self.bind_framebuffer)(GL_FRAMEBUFFER, 0);
        (self.bind_texture)(GL_TEXTURE_2D, 0);
        if status != GL_FRAMEBUFFER_COMPLETE {
            (self.delete_framebuffers)(1, &fbo);
            (self.delete_textures)(1, &texture);
            return Err(format!("FBO 不完整: 0x{status:04x}"));
        }
        Ok(VideoTarget {
            texture,
            fbo,
            width,
            height,
        })
    }

    /// 把宿主遗留的 GL 状态恢复到 mpv 契要求的默认值。
    ///
    /// 对每个必须默认关闭的能力位（[`MPV_REQUIRED_GL_DEFAULTS`]）先读一下宿主是否把它
    /// 打开了（返回值供调用方记日志：正常产品运行里应当为空），然后无条件关掉它——mpv
    /// 自己会在需要混合的 pass 上重新 `glEnable(GL_BLEND)`。
    ///
    /// 必须在 `mpv_render_context_render` **之前**、且在 `save_state` 之后调用：
    /// `restore_state` 会把宿主的混合状态还回去。
    pub(crate) fn restore_mpv_state_defaults(&self) -> Vec<&'static str> {
        enforce_defaults(
            &MPV_REQUIRED_GL_DEFAULTS,
            |cap| (self.is_enabled)(cap) != 0,
            |cap| (self.disable)(cap),
        )
    }

    /// 删除 [`VideoTarget`] 的 GL 对象。必须在上下文 current 时调用。
    pub(crate) fn destroy_target(&self, target: VideoTarget) {
        (self.delete_framebuffers)(1, &target.fbo);
        (self.delete_textures)(1, &target.texture);
    }

    /// 保存会被 mpv 改掉的 GL 状态。
    ///
    /// render_gl.h 明确说 mpv 不保证恢复 `glViewport`/`glScissor`/`glBlendFuncSeparate`/
    /// `glClearColor` 等状态（见 [`MPV_REQUIRED_GL_DEFAULTS`] 的说明），所以调用方必须
    /// 自己还原：包括分离式混合因子与 blend equation（mpv 的 pass 会改它们）。
    pub(crate) fn save_state(&self) -> SavedState {
        let mut viewport = [0i32; 4];
        (self.get_integer_v)(GL_VIEWPORT, viewport.as_mut_ptr());
        let mut scissor = [0i32; 4];
        (self.get_integer_v)(GL_SCISSOR_BOX, scissor.as_mut_ptr());
        let mut clear = [0f32; 4];
        (self.get_float_v)(GL_COLOR_CLEAR_VALUE, clear.as_mut_ptr());
        SavedState {
            viewport,
            scissor,
            scissor_enabled: (self.is_enabled)(GL_SCISSOR_TEST) != 0,
            blend_enabled: (self.is_enabled)(GL_BLEND) != 0,
            clear_color: clear,
            active_texture: self.get_integer_v_i32(GL_ACTIVE_TEXTURE),
            texture_2d: self.get_integer_v_i32(GL_TEXTURE_BINDING_2D),
            blend_src_rgb: self.get_integer_v_i32(GL_BLEND_SRC_RGB),
            blend_dst_rgb: self.get_integer_v_i32(GL_BLEND_DST_RGB),
            blend_src_alpha: self.get_integer_v_i32(GL_BLEND_SRC_ALPHA),
            blend_dst_alpha: self.get_integer_v_i32(GL_BLEND_DST_ALPHA),
            blend_equation_rgb: self.get_integer_v_i32(GL_BLEND_EQUATION_RGB),
            blend_equation_alpha: self.get_integer_v_i32(GL_BLEND_EQUATION_ALPHA),
        }
    }

    fn get_integer_v_i32(&self, name: u32) -> i32 {
        let mut value = 0i32;
        (self.get_integer_v)(name, &mut value);
        value
    }

    /// 恢复 [`SavedState`]；`framebuffer` 为调用方当前绑定的 FBO（0 = 默认帧缓冲），
    /// `viewport` 是调用方的像素尺寸（Slint 的 back buffer 就是整窗）。
    pub(crate) fn restore_state(
        &self,
        state: &SavedState,
        framebuffer: u32,
        width: u32,
        height: u32,
    ) {
        (self.bind_framebuffer)(GL_FRAMEBUFFER, framebuffer);
        let viewport_w = if width == 0 {
            state.viewport[2]
        } else {
            i32::try_from(width).unwrap_or(state.viewport[2])
        };
        let viewport_h = if height == 0 {
            state.viewport[3]
        } else {
            i32::try_from(height).unwrap_or(state.viewport[3])
        };
        (self.viewport)(0, 0, viewport_w, viewport_h);
        (self.scissor)(
            state.scissor[0],
            state.scissor[1],
            state.scissor[2],
            state.scissor[3],
        );
        if state.scissor_enabled {
            (self.enable)(GL_SCISSOR_TEST);
        } else {
            (self.disable)(GL_SCISSOR_TEST);
        }
        if state.blend_enabled {
            (self.enable)(GL_BLEND);
        } else {
            (self.disable)(GL_BLEND);
        }
        // mpv 不负责还原分离式混合因子与 blend equation：这里还回宿主的原值。
        (self.blend_func_separate)(
            u32::try_from(state.blend_src_rgb).unwrap_or_default(),
            u32::try_from(state.blend_dst_rgb).unwrap_or_default(),
            u32::try_from(state.blend_src_alpha).unwrap_or_default(),
            u32::try_from(state.blend_dst_alpha).unwrap_or_default(),
        );
        (self.blend_equation_separate)(
            u32::try_from(state.blend_equation_rgb).unwrap_or_default(),
            u32::try_from(state.blend_equation_alpha).unwrap_or_default(),
        );
        (self.clear_color)(
            state.clear_color[0],
            state.clear_color[1],
            state.clear_color[2],
            state.clear_color[3],
        );
        if state.active_texture > 0 {
            (self.active_texture)(state.active_texture as u32);
        }
        (self.bind_texture)(
            GL_TEXTURE_2D,
            u32::try_from(state.texture_2d).unwrap_or_default(),
        );
    }
}

/// 只读地看一眼 GL 的字符串（非空、NUL 结尾、与上下文同寿命）。
///
/// 实现放在 `ffi` 模块里：`CStr::from_ptr` 是 unsafe API，而本 crate 的硬约束是
/// **unsafe 只出现在 `ffi`**。
fn gl_string(ptr: *const u8) -> String {
    crate::ffi::gl_string(ptr as *const std::ffi::c_char)
}

/// `GlFunctions::save_state` 的快照。
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedState {
    pub(crate) viewport: [i32; 4],
    pub(crate) scissor: [i32; 4],
    pub(crate) scissor_enabled: bool,
    pub(crate) blend_enabled: bool,
    pub(crate) clear_color: [f32; 4],
    pub(crate) active_texture: i32,
    pub(crate) texture_2d: i32,
    /// `GL_BLEND_SRC_RGB` / `GL_BLEND_DST_RGB` / `GL_BLEND_SRC_ALPHA` / `GL_BLEND_DST_ALPHA`。
    pub(crate) blend_src_rgb: i32,
    pub(crate) blend_dst_rgb: i32,
    pub(crate) blend_src_alpha: i32,
    pub(crate) blend_dst_alpha: i32,
    /// `GL_BLEND_EQUATION_RGB` / `GL_BLEND_EQUATION_ALPHA`。
    pub(crate) blend_equation_rgb: i32,
    pub(crate) blend_equation_alpha: i32,
}

/// 视频纹理 + 它的 FBO：mpv 渲染进这里，Slint 把同一张纹理合成到窗口。
///
/// 字段私有：外部只能通过 [`VideoTarget::texture_id`] / [`VideoTarget::size`] 读取，
/// 避免「纹理名被改成别的对象」这类错误。
pub(crate) struct VideoTarget {
    texture: u32,
    fbo: u32,
    width: u32,
    height: u32,
}

impl VideoTarget {
    /// 纹理名（`glGenTextures` 的返回值），交给 Slint 的
    /// `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture`。
    pub(crate) fn texture_id(&self) -> u32 {
        self.texture
    }

    pub(crate) fn fbo(&self) -> u32 {
        self.fbo
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl std::fmt::Debug for VideoTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoTarget")
            .field("texture", &self.texture)
            .field("fbo", &self.fbo)
            .field("size", &(self.width, self.height))
            .finish()
    }
}

/// 切入 mpv 前把「必须默认关闭」的能力位关掉，返回其中宿主确实打开过的名字。
///
/// 抽成纯函数是为了能在没有 GL 上下文的情况下做单测（[`GlFunctions::restore_mpv_state_defaults`]
/// 只是把两个函数指针接进来）。
fn enforce_defaults(
    caps: &[(&'static str, u32)],
    is_enabled: impl Fn(u32) -> bool,
    disable: impl Fn(u32),
) -> Vec<&'static str> {
    let mut violated = Vec::new();
    for (name, cap) in caps {
        if is_enabled(*cap) {
            violated.push(*name);
        }
        disable(*cap);
    }
    violated
}

/// 视频纹理尺寸的校验（尺寸来自视频属性，是外部输入）。
///
/// 所有乘法走 `checked_mul`：`width * height * 4` 在 32 位目标上真的会溢出。
fn checked_target_size(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 {
        return Err(format!("视频尺寸非法: {width}x{height}"));
    }
    if width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(format!("视频尺寸超出 i32: {width}x{height}"));
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("视频尺寸 {width}x{height} 的 RGBA 字节数溢出 usize"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_size_validation_rejects_zero_and_overflow() {
        assert!(checked_target_size(0, 720).is_err());
        assert!(checked_target_size(1280, 0).is_err());
        assert_eq!(
            checked_target_size(1920, 1080).unwrap_or(0),
            1920 * 1080 * 4
        );
        // 超出 i32 的尺寸直接在 GL 之前挡掉。
        assert!(checked_target_size(u32::MAX, 2).is_err());
        assert!(checked_target_size(i32::MAX as u32 + 1, 1).is_err());
        // u32::MAX * u32::MAX * 4 必然溢出，不能靠 wrap 算出一个小数字。
        assert!(checked_target_size(u32::MAX - 1, u32::MAX - 1).is_err());
    }

    /// 体检 R6-1：mpv 的 GL 状态契约必须包含这五个默认关闭的能力位
    /// （`mpv/render_gl.h` 46–59 行：除 viewport / scissor 矩形 / blend func /
    /// clear color 四项例外外，其余状态必须是 OpenGL 标准默认值）。
    #[test]
    fn mpv_state_contract_covers_the_default_off_capabilities() {
        let names: Vec<&str> = MPV_REQUIRED_GL_DEFAULTS.iter().map(|(n, _)| *n).collect();
        for name in [
            "GL_SCISSOR_TEST",
            "GL_BLEND",
            "GL_DEPTH_TEST",
            "GL_STENCIL_TEST",
            "GL_CULL_FACE",
        ] {
            assert!(names.contains(&name), "契约缺少 {name}");
        }
        // 只要这五项：viewport / scissor 矩形 / blend func / clear color 是 mpv 不负责
        // 还原的例外，由 save_state / restore_state 自己处理。
        assert_eq!(names.len(), 5);
    }

    /// 宿主（Slint/femtovg）开着混合时：契约执行必须报告并关掉它；五项都要关。
    #[test]
    fn host_blend_state_is_reported_and_disabled() {
        let disabled = std::cell::RefCell::new(Vec::new());
        let violated = enforce_defaults(
            &MPV_REQUIRED_GL_DEFAULTS,
            |cap| cap == GL_BLEND,
            |cap| disabled.borrow_mut().push(cap),
        );
        assert_eq!(violated, vec!["GL_BLEND"]);
        assert_eq!(
            *disabled.borrow(),
            vec![
                GL_SCISSOR_TEST,
                GL_BLEND,
                GL_DEPTH_TEST,
                GL_STENCIL_TEST,
                GL_CULL_FACE
            ]
        );
    }

    /// 宿主状态本来就是默认值时：不报告任何违规，但仍然显式关一遍（不赌宿主的默认值）。
    #[test]
    fn clean_host_state_reports_no_violation_but_still_resets() {
        let disabled = std::cell::RefCell::new(Vec::new());
        let violated = enforce_defaults(
            &MPV_REQUIRED_GL_DEFAULTS,
            |_| false,
            |cap| disabled.borrow_mut().push(cap),
        );
        assert!(violated.is_empty());
        assert_eq!(
            *disabled.borrow(),
            vec![
                GL_SCISSOR_TEST,
                GL_BLEND,
                GL_DEPTH_TEST,
                GL_STENCIL_TEST,
                GL_CULL_FACE
            ]
        );
    }
}
