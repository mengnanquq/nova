// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Slint 窗口 + libmpv OpenGL render API 的零拷贝原型（Gate 1.5-A）。
//!
//! 数据路径（全程在 GPU 上，没有任何“下载到系统内存再上传”的往返）：
//!
//! ```text
//! mpv (vo=libmpv, hwdec=vaapi) → mpv_render_context_render(FBO=我们的纹理)
//!   → slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(纹理, 尺寸)
//!   → Slint 界面的 Image 元素 → femtovg 合成到窗口
//! ```

pub mod app;
pub mod cli;

pub use cli::parse_size;
pub mod gl;
pub mod glproc;
pub mod sys;

/// 报告用的百分位（输入是纳秒）。
pub mod stats {
    /// `values` 会被就地排序。
    pub fn percentiles(values: &mut [u64], ps: &[f64]) -> Vec<f64> {
        if values.is_empty() {
            return vec![f64::NAN; ps.len()];
        }
        values.sort_unstable();
        ps.iter()
            .map(|p| {
                let idx = ((values.len() as f64 - 1.0) * p / 100.0).round() as usize;
                values[idx] as f64
            })
            .collect()
    }

    pub fn mean(values: &[u64]) -> f64 {
        if values.is_empty() {
            return f64::NAN;
        }
        values.iter().map(|v| *v as f64).sum::<f64>() / values.len() as f64
    }
}
