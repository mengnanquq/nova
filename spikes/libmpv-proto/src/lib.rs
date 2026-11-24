// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! libmpv **software 渲染 API**（`MPV_RENDER_API_TYPE_SW`）在无显示环境下的解帧原型。
//!
//! 目标：给播放引擎选型 ADR 提供与 GStreamer 方案可比的数据，而不是做一个播放器。
//! 相关结论见同目录 `FINDINGS.md`。

pub mod bench;
pub mod dump;
pub mod player;
pub mod probe;
pub mod sys;
pub mod testmedia;

pub use sys::SurfaceFormat;

/// 把 `"rgb0"` / `"bgr0"` 解析成 [`SurfaceFormat`]。
pub fn parse_format(name: &str) -> Result<SurfaceFormat, String> {
    match name {
        "rgb0" | "rgba" | "rgb" => Ok(SurfaceFormat::Rgb0),
        "bgr0" | "bgra" | "bgr" => Ok(SurfaceFormat::Bgr0),
        other => Err(format!("不支持的格式 `{other}`（只支持 rgb0 / bgr0）")),
    }
}
