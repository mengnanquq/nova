// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `dump` 子命令：把 SW 渲染出来的帧写成 PNG。
//!
//! mpv 的 `rgb0`/`bgr0` 的第 4 个字节在 render.h 里被写成“未初始化的垃圾”，
//! 不保证是 alpha。写 PNG 时这里统一按不透明处理（alpha=255），
//! 需要真实 alpha 的路径（例如交给 Slint 的 `Rgba8Pixel`）必须自己兜底。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::player::{Session, SessionOptions};
use crate::sys::{AlignedImage, SurfaceFormat, SwRenderer};

/// 一帧渲染结果，已展开成 R,G,B,A 顺序。
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// 把 mpv 写进 `img` 的像素转成 RGBA（alpha 强制 255），顺带做 BGR→RGB。
pub fn to_rgba(img: &AlignedImage, format: SurfaceFormat) -> RgbaFrame {
    let (w, h) = (img.width(), img.height());
    let mut data = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        let src = img.row(y);
        let dst = &mut data[y * w as usize * 4..(y + 1) * w as usize * 4];
        for x in 0..w as usize {
            let s = &src[x * 4..x * 4 + 3];
            let d = &mut dst[x * 4..x * 4 + 4];
            match format {
                SurfaceFormat::Rgb0 => d[..3].copy_from_slice(s),
                SurfaceFormat::Bgr0 => {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                }
            }
            d[3] = 255;
        }
    }
    RgbaFrame {
        width: w,
        height: h,
        data,
    }
}

/// 写 PNG（RGBA8）。
pub fn write_png(path: &Path, frame: &RgbaFrame) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("创建 {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(file, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("写 PNG 头: {e}"))?;
    writer
        .write_image_data(&frame.data)
        .map_err(|e| format!("写 PNG 数据: {e}"))?;
    Ok(())
}

/// 导出前 `n` 个连续帧到 `out_dir`，文件名 `<stem>_000.png` 开始。
pub fn run(
    file: &Path,
    out_dir: &Path,
    frames: usize,
    format: SurfaceFormat,
    session_opts: &SessionOptions<'_>,
) -> Result<Vec<std::path::PathBuf>, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("创建 {}: {e}", out_dir.display()))?;
    let session = Session::open(file, session_opts)?;
    session.wait_loaded(Duration::from_secs(20))?;
    let mut img = session.first_frame(format, Duration::from_secs(20))?;
    let renderer = session.renderer();
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "frame".to_string());

    let mut written = Vec::with_capacity(frames);
    let mut index = 0usize;
    let deadline = Instant::now() + Duration::from_secs(60);
    // 第 0 帧就是 first_frame 已经渲染好的那一帧。
    loop {
        let path = out_dir.join(format!("{stem}_{index:03}.png"));
        let frame = to_rgba(&img, format);
        write_png(&path, &frame)?;
        println!(
            "  {} ({}x{}, 左上角像素 RGB={:?})",
            path.display(),
            frame.width,
            frame.height,
            &frame.data[..3]
        );
        written.push(path);
        index += 1;
        if index >= frames {
            break;
        }
        // 等下一帧
        let mut got = false;
        while Instant::now() < deadline {
            if session.drain_events()? {
                break;
            }
            if SwRenderer::has_new_frame(renderer.update()) {
                renderer.render_into(format, &mut img)?;
                got = true;
                break;
            }
            renderer.signal().wait(Duration::from_millis(20));
        }
        if !got {
            println!("  （没有更多帧，提前结束：已写 {} 帧）", written.len());
            break;
        }
    }
    Ok(written)
}
