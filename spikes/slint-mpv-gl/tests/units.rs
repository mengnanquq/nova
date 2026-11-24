// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 不依赖窗口/GPU 的单元测试。
//!
//! 需要真窗口 + mpv 的验证在 `tests/live.rs`（`#[ignore]`，手动跑）。

use std::num::NonZeroU32;

use slint_mpv_gl::gl::{PixelStats, write_png};
use slint_mpv_gl::parse_size;
use slint_mpv_gl::stats::{mean, percentiles};
use slint_mpv_gl::sys::GlTarget;

fn rgb_ramp(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            // 左黑右白、上下对称的渐变：均值/方差可手算。
            let v = (x as f64 * 255.0 / (width.saturating_sub(1).max(1)) as f64) as u8;
            let _ = y;
            out.extend_from_slice(&[v, v, v, 255]);
        }
    }
    out
}

#[test]
fn pixel_stats_on_half_black_half_white() {
    // 2x1：一黑一白 → luma 均值 127.5，方差 127.5^2，非黑比例 0.5
    let data = [0u8, 0, 0, 255, 255, 255, 255, 255];
    let stats = PixelStats::from_rgba(&data);
    assert!((stats.mean_luma - 127.5).abs() < 1e-6, "{stats:?}");
    assert!((stats.variance - 127.5 * 127.5).abs() < 1e-3, "{stats:?}");
    assert!((stats.non_black_ratio - 0.5).abs() < 1e-9, "{stats:?}");
    assert!(stats.looks_like_video());
}

#[test]
fn pixel_stats_rejects_black_frame() {
    let data = vec![0u8; 64 * 4];
    let stats = PixelStats::from_rgba(&data);
    assert_eq!(stats.mean_luma, 0.0);
    assert_eq!(stats.variance, 0.0);
    assert_eq!(stats.non_black_ratio, 0.0);
    assert!(!stats.looks_like_video(), "全黑帧不该被当成视频画面");
}

#[test]
fn pixel_stats_rejects_flat_gray_frame() {
    // 均匀灰：均值够亮但方差为 0 —— "不是黑屏"不等于"有画面"。
    let data = vec![128u8; 100 * 4];
    let stats = PixelStats::from_rgba(&data);
    assert!(stats.mean_luma > 100.0);
    assert_eq!(stats.variance, 0.0);
    assert!(!stats.looks_like_video());
}

#[test]
fn pixel_stats_on_ramp_has_expected_mean_and_variance() {
    let (w, h) = (256u32, 4u32);
    let data = rgb_ramp(w, h);
    let stats = PixelStats::from_rgba(&data);
    // 0..=255 的均匀分布：均值 127.5，方差 ((n^2-1)/12) ≈ 5461
    assert!((stats.mean_luma - 127.5).abs() < 1.0, "{stats:?}");
    assert!((stats.variance - 5461.0).abs() < 60.0, "{stats:?}");
    assert!((stats.non_black_ratio - (255.0 - 16.0) / 255.0).abs() < 0.02);
}

#[test]
fn empty_input_does_not_panic() {
    let stats = PixelStats::from_rgba(&[]);
    assert_eq!(stats.mean_luma, 0.0);
}

#[test]
fn gl_target_validates_dimensions() {
    assert!(GlTarget::new(1, 1920, 1080).is_ok());
    assert!(
        GlTarget::new(0, 1920, 1080).is_ok(),
        "FBO 0 = 默认帧缓冲，合法"
    );
    // 0 尺寸
    assert!(GlTarget::new(1, 0, 1080).is_err());
    assert!(GlTarget::new(1, 1920, 0).is_err());
    // c_int 溢出
    assert!(GlTarget::new(1, u32::MAX, 1).is_err());
    // usize 乘法溢出（2^32 x 2^32 像素）
    assert!(GlTarget::new(1, u32::MAX, u32::MAX).is_err());
    // FBO 名溢出
    assert!(GlTarget::new(u32::MAX, 4, 4).is_err());
}

#[test]
fn gl_target_keeps_dimensions() {
    let t = GlTarget::new(7, 3840, 2160).unwrap();
    assert_eq!(t.fbo(), 7);
    assert_eq!(t.width(), 3840);
    assert_eq!(t.height(), 2160);
}

#[test]
fn percentiles_and_mean() {
    let mut values: Vec<u64> = (1..=100).collect();
    let p = percentiles(&mut values, &[50.0, 95.0]);
    assert!((p[0] - 51.0).abs() <= 1.0, "{p:?}");
    assert!((p[1] - 95.0).abs() <= 1.0, "{p:?}");
    assert_eq!(mean(&[2, 4, 6]), 4.0);
    assert!(mean(&[]).is_nan());
    assert!(percentiles(&mut [], &[50.0])[0].is_nan());
}

#[test]
fn parse_size_accepts_both_separators() {
    assert_eq!(parse_size("960x540").unwrap(), (960, 540));
    assert_eq!(parse_size("1920X1080").unwrap(), (1920, 1080));
    assert_eq!(parse_size(" 1280 x 720 ").unwrap(), (1280, 720));
    assert!(parse_size("960").is_err());
    assert!(parse_size("960x0").is_err());
    assert!(parse_size("abcx540").is_err());
    assert!(parse_size("10000x540").is_err());
}

#[test]
fn write_png_produces_valid_header() {
    let dir = std::env::temp_dir().join(format!("slint-mpv-gl-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stat.png");
    let (w, h) = (17u32, 5u32);
    let data = rgb_ramp(w, h);
    write_png(&path, w, h, &data).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG 签名");
    // IHDR 紧跟签名：长度(4) + "IHDR"(4) + 宽(4) + 高(4) + bit depth + color type
    assert_eq!(&bytes[12..16], b"IHDR");
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), w);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), h);
    assert_eq!(bytes[24], 8, "bit depth");
    assert_eq!(bytes[25], 6, "color type RGBA");
    std::fs::remove_file(&path).ok();
    std::fs::remove_dir(&dir).ok();
}

#[test]
fn write_png_rejects_size_mismatch() {
    let path = std::env::temp_dir().join("slint-mpv-gl-bad.png");
    let err = write_png(&path, 4, 4, &[0u8; 8]).unwrap_err();
    assert!(err.contains("不符"), "{err}");
    assert!(!path.exists(), "失败时不该留下文件");
}

#[test]
fn nonzero_texture_id_is_required_by_slint_builder() {
    // Slint 的 BorrowedOpenGLTextureBuilder 只接受 NonZeroU32；记录一下这个前提。
    assert!(NonZeroU32::new(0).is_none());
    assert!(NonZeroU32::new(1).is_some());
}
