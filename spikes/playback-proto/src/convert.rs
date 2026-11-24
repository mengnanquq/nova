// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! NV12 / I420 → RGBA8 的纯 Rust 转换。
//!
//! 这里刻意不用 `videoconvert`：bench 要单独量出"NV12→RGBA"这一段在 CPU 上的成本。
//!
//! 色彩矩阵：`>= 720` 行走 BT.709，SD 走 BT.601，都是 limited range（视频常见的
//! `tv` range）。没有读 caps 里的 `colorimetry` 字段，这是一个已知简化。

use crate::{Error, Result};

/// YUV → RGB 的系数集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
}

impl ColorMatrix {
    /// 按分辨率猜矩阵（原型简化：不读 caps 的 colorimetry）。
    pub fn guess(height: u32) -> Self {
        if height >= 720 {
            ColorMatrix::Bt709
        } else {
            ColorMatrix::Bt601
        }
    }

    /// (r_v, g_u, g_v, b_u)，10 位定点。
    fn coeffs(self) -> (i32, i32, i32, i32) {
        match self {
            // R = 1.402 V, G = -0.344136 U - 0.714136 V, B = 1.772 U
            ColorMatrix::Bt601 => (1436, -352, -731, 1815),
            // R = 1.5748 V, G = -0.1873 U - 0.4681 V, B = 1.8556 U
            ColorMatrix::Bt709 => (1613, -192, -479, 1900),
        }
    }
}

#[inline(always)]
fn clamp_u8(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v as u8
    }
}

#[inline(always)]
fn yuv_to_rgba(y: u8, u: u8, v: u8, matrix: ColorMatrix) -> [u8; 4] {
    // limited range: Y' = 1.164 * (Y - 16)，u/v 以 128 为中心。
    let c = (y as i32 - 16).max(0) * 1192;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let (r_v, g_u, g_v, b_u) = matrix.coeffs();
    // +512 = 四舍五入到最近整数（`>> 10` 之前），否则 235 只会映射到 254。
    [
        clamp_u8((c + r_v * e + 512) >> 10),
        clamp_u8((c + g_u * d + g_v * e + 512) >> 10),
        clamp_u8((c + b_u * d + 512) >> 10),
        255,
    ]
}

/// 一帧 NV12 的只读视图（`stride` 是字节，可能带对齐 padding）。
#[derive(Debug, Clone, Copy)]
pub struct Nv12View<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub uv: &'a [u8],
    pub uv_stride: usize,
}

/// 一帧 I420 的只读视图。
#[derive(Debug, Clone, Copy)]
pub struct I420View<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub u: &'a [u8],
    pub u_stride: usize,
    pub v: &'a [u8],
    pub v_stride: usize,
}

fn check_plane(
    name: &str,
    plane_len: usize,
    stride: usize,
    rows: usize,
    row_bytes: usize,
) -> Result<()> {
    let needed = if rows == 0 {
        0
    } else {
        stride * (rows - 1) + row_bytes
    };
    if stride < row_bytes {
        return Err(Error::Format(format!(
            "{name} 平面 stride={stride} 小于一行像素字节数 {row_bytes}"
        )));
    }
    if plane_len < needed {
        return Err(Error::Format(format!(
            "{name} 平面只有 {plane_len} 字节，至少需要 {needed}"
        )));
    }
    Ok(())
}

fn out_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// NV12（Y 平面 + 交织 UV 平面，4:2:0）→ RGBA8。
///
/// `out` 会被 resize 到 `width*height*4`；`stride` 恒等于 `width*4`。
pub fn nv12_to_rgba(src: &Nv12View<'_>, out: &mut Vec<u8>) -> Result<()> {
    let w = src.width as usize;
    let h = src.height as usize;
    if w == 0 || h == 0 {
        return Err(Error::Format("NV12 帧宽高为 0".into()));
    }
    check_plane("Y", src.y.len(), src.y_stride, h, w)?;
    let chroma_rows = h.div_ceil(2);
    check_plane(
        "UV",
        src.uv.len(),
        src.uv_stride,
        chroma_rows,
        w.div_ceil(2) * 2,
    )?;

    let matrix = ColorMatrix::guess(src.height);
    out.clear();
    out.resize(out_len(src.width, src.height), 0);

    for row in 0..h {
        let y_row = &src.y[row * src.y_stride..row * src.y_stride + w];
        let uv_row = &src.uv[(row / 2) * src.uv_stride..(row / 2) * src.uv_stride + w];
        let dst = &mut out[row * w * 4..(row + 1) * w * 4];
        for (col, (&y, dst_px)) in y_row.iter().zip(dst.chunks_exact_mut(4)).enumerate() {
            let uv_idx = col & !1;
            let u = uv_row[uv_idx];
            let v = uv_row[uv_idx + 1];
            dst_px.copy_from_slice(&yuv_to_rgba(y, u, v, matrix));
        }
    }
    Ok(())
}

/// I420（Y/U/V 三个平面，4:2:0）→ RGBA8。
pub fn i420_to_rgba(src: &I420View<'_>, out: &mut Vec<u8>) -> Result<()> {
    let w = src.width as usize;
    let h = src.height as usize;
    if w == 0 || h == 0 {
        return Err(Error::Format("I420 帧宽高为 0".into()));
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    check_plane("Y", src.y.len(), src.y_stride, h, w)?;
    check_plane("U", src.u.len(), src.u_stride, ch, cw)?;
    check_plane("V", src.v.len(), src.v_stride, ch, cw)?;

    let matrix = ColorMatrix::guess(src.height);
    out.clear();
    out.resize(out_len(src.width, src.height), 0);

    for row in 0..h {
        let y_row = &src.y[row * src.y_stride..row * src.y_stride + w];
        let u_row = &src.u[(row / 2) * src.u_stride..(row / 2) * src.u_stride + cw];
        let v_row = &src.v[(row / 2) * src.v_stride..(row / 2) * src.v_stride + cw];
        let dst = &mut out[row * w * 4..(row + 1) * w * 4];
        for (col, (&y, dst_px)) in y_row.iter().zip(dst.chunks_exact_mut(4)).enumerate() {
            let u = u_row[col / 2];
            let v = v_row[col / 2];
            dst_px.copy_from_slice(&yuv_to_rgba(y, u, v, matrix));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2 纯黑 NV12 应该得到 4 个 (0,0,0,255)。
    #[test]
    fn nv12_black_is_opaque_black() {
        let y = [16u8; 4];
        let uv = [128u8; 2];
        let view = Nv12View {
            width: 2,
            height: 2,
            y: &y,
            y_stride: 2,
            uv: &uv,
            uv_stride: 2,
        };
        let mut out = Vec::new();
        nv12_to_rgba(&view, &mut out).unwrap();
        assert_eq!(out.len(), 2 * 2 * 4);
        for px in out.chunks_exact(4) {
            assert_eq!(px, [0, 0, 0, 255]);
        }
    }

    /// 带 padding 的 stride 与紧凑 stride 必须给出同样的像素。
    #[test]
    fn nv12_padded_stride_matches_compact() {
        let y_compact: Vec<u8> = (0..16).map(|i| 16 + i as u8 * 7).collect();
        let uv_compact: Vec<u8> = (0..8).map(|i| 100 + i as u8 * 3).collect();
        let compact_view = Nv12View {
            width: 4,
            height: 4,
            y: &y_compact,
            y_stride: 4,
            uv: &uv_compact,
            uv_stride: 4,
        };
        let mut compact = Vec::new();
        nv12_to_rgba(&compact_view, &mut compact).unwrap();

        // 每行多 8 字节 padding。
        let y_stride = 12;
        let mut y_padded = vec![0xffu8; y_stride * 4];
        for row in 0..4 {
            y_padded[row * y_stride..row * y_stride + 4]
                .copy_from_slice(&y_compact[row * 4..row * 4 + 4]);
        }
        let uv_stride = 12;
        let mut uv_padded = vec![0xffu8; uv_stride * 2];
        for row in 0..2 {
            uv_padded[row * uv_stride..row * uv_stride + 4]
                .copy_from_slice(&uv_compact[row * 4..row * 4 + 4]);
        }
        let padded_view = Nv12View {
            width: 4,
            height: 4,
            y: &y_padded,
            y_stride,
            uv: &uv_padded,
            uv_stride,
        };
        let mut padded = Vec::new();
        nv12_to_rgba(&padded_view, &mut padded).unwrap();
        assert_eq!(compact, padded);
    }

    #[test]
    fn i420_black_is_opaque_black() {
        let y = [16u8; 4];
        let chroma = [128u8; 2];
        let view = I420View {
            width: 2,
            height: 2,
            y: &y,
            y_stride: 2,
            u: &chroma,
            u_stride: 1,
            v: &chroma,
            v_stride: 1,
        };
        let mut out = Vec::new();
        i420_to_rgba(&view, &mut out).unwrap();
        assert_eq!(
            out,
            vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
    }

    #[test]
    fn short_plane_is_rejected() {
        let y = [16u8; 3];
        let uv = [128u8; 2];
        let view = Nv12View {
            width: 4,
            height: 4,
            y: &y,
            y_stride: 4,
            uv: &uv,
            uv_stride: 4,
        };
        let mut out = Vec::new();
        assert!(matches!(
            nv12_to_rgba(&view, &mut out),
            Err(Error::Format(_))
        ));
    }

    /// 白色 (235,128,128) 必须在 BT.601/709 下都饱和到 255。
    #[test]
    fn white_saturates() {
        let px = yuv_to_rgba(235, 128, 128, ColorMatrix::Bt709);
        assert_eq!(px, [255, 255, 255, 255]);
        let px = yuv_to_rgba(235, 128, 128, ColorMatrix::Bt601);
        assert_eq!(px, [255, 255, 255, 255]);
    }
}
