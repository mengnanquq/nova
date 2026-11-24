// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 的 libmpv 播放引擎：`nova_playback::PlaybackEngine` 的第一个真实实现。
//!
//! 走 **libmpv 的 OpenGL render API 零拷贝路径**（ADR-0001 的决策）：
//!
//! ```text
//! mpv（vaapi 硬解，GPU 上的 surface）
//!   → EGL dmabuf interop → 采样进我们的 GL_RGBA8 纹理（自有 FBO）
//!   → nova-app 用 BorrowedOpenGLTextureBuilder 把同一个纹理交给 Slint 合成
//! ```
//!
//! 中间没有 GPU→CPU 回读：4K 下一次回读要 16 ms/帧，比 60 Hz 的整个帧预算还多。
//!
//! # 谁负责什么
//!
//! - **本 crate**：mpv core 与 render context 的生命周期、事件翻译、硬解断言、帧 watchdog、
//!   外挂字幕落盘、GL 纹理与 FBO。
//! - **nova-app**：窗口、`set_rendering_notifier`、每帧调用 [`MpvEngine::render`]，
//!   以及把 [`MpvEngine::texture`] 换成 Slint 的 `Image`。
//!
//! 这个 crate **不建窗口**，也不链接 `libGL`/`libEGL`：GL 入口点全部来自宿主
//! （Slint 的 `GraphicsAPI::NativeOpenGL::get_proc_address`）。
//!
//! # 最小用法
//!
//! ```no_run
//! use nova_playback::{OpenRequest, PlaybackEngine};
//! use nova_playback_mpv::{EngineOptions, GlFunctions, MpvEngine};
//!
//! # async fn demo(gl: GlFunctions) -> Result<(), nova_playback::PlaybackError> {
//! // 1) RenderingSetup：GL 上下文 current
//! let mut engine = MpvEngine::new(EngineOptions::new())?;
//! engine.attach_gl(gl)?;
//! // 2) 打开（认证头由上层塞进 headers）
//! engine
//!     .open(OpenRequest::new("http://127.0.0.1:8096/Videos/1/stream?static=true")
//!         .with_header("Authorization", "MediaBrowser Token=\"…\""))
//!     .await?;
//! // 3) BeforeRendering：有新画面就画一帧
//! let _new_frame = engine.render(0, 1920, 1080)?;
//! // 4) 交给 Slint 的纹理
//! let _texture = engine.texture();
//! Ok(())
//! # }
//! ```
//!
//! # 两条硬规则（ADR-0001「两个必须落进产品代码的坑」）
//!
//! 1. **硬解断言**：`open` 之后比对 `hwdec-current` 与请求值，不一致就 `tracing::warn!`
//!    并把实际值写进 [`MpvEngine::diagnostics`]；界面用
//!    [`EngineDiagnostics::hwdec_notice`](nova_playback::EngineDiagnostics::hwdec_notice)
//!    提示"硬件解码未生效"。绝不静默降级。
//! 2. **帧回调 watchdog**：[`MpvEngine::needs_redraw`] 在"播放中且超过
//!    [`REDRAW_WATCHDOG`] 没有渲染"时返回 `true`，上层据此主动驱动，不依赖合成器回调。
//!
//! # unsafe 的位置
//!
//! 全 crate 的 `unsafe` **只在 [`ffi`] 一个模块**里（libmpv C API、GL 入口点装载、
//! `mpv_node` 遍历），每块都有 `SAFETY`/`# Safety` 注释；其余模块零 `unsafe`。

#![warn(missing_docs)]

mod engine;
mod error;
mod events;
mod ffi;
mod gl;
mod options;
mod subtitles;
mod worker;

pub use engine::{EventStream, MpvEngine, REDRAW_WATCHDOG};
pub use gl::GlFunctions;
pub use options::{EngineOptions, default_hwdec, drm_render_node_candidates};
