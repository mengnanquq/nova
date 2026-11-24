// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 把帧导出成 PNG（验证"帧真的解出来了、像素真的写进去了"）。

use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::{Error, FrameSource, Result, VideoFrame};

/// 写一帧 RGBA8 为 PNG。`stride > width*4` 时逐行压缩。
pub fn write_frame_png(frame: &VideoFrame, path: &Path) -> Result<()> {
    if frame.width == 0 || frame.height == 0 {
        return Err(Error::Format("帧宽高为 0，无法写 PNG".into()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|err| Error::Png(format!("写 PNG 头失败: {err}")))?;
    let row_bytes = frame.width as usize * VideoFrame::BYTES_PER_PIXEL;
    let encoded = if frame.stride == row_bytes {
        writer
            .write_image_data(&frame.data)
            .map_err(|err| Error::Png(format!("写 PNG 数据失败: {err}")))
    } else {
        let mut tight = Vec::with_capacity(row_bytes * frame.height as usize);
        for row in 0..frame.height {
            let row = frame
                .row(row)
                .ok_or_else(|| Error::Format(format!("第 {row} 行越界")))?;
            tight.extend_from_slice(row);
        }
        writer
            .write_image_data(&tight)
            .map_err(|err| Error::Png(format!("写 PNG 数据失败: {err}")))
    };
    encoded?;
    Ok(())
}

/// 从 `source` 取最多 `count` 帧，写到 `out_dir/frame_0000.png` 等文件。
///
/// 返回实际写出的文件列表；流比 `count` 短时提前结束。
pub fn dump_frames(
    source: &mut impl FrameSource,
    count: usize,
    out_dir: &Path,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let mut written = Vec::new();
    for index in 0..count {
        let Some(frame) = source.next_frame()? else {
            break;
        };
        let path = out_dir.join(format!("frame_{index:04}.png"));
        write_frame_png(&frame, &path)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用伪随机噪声填充，避免 PNG 压得太小让"字节数"断言失去意义。
    fn noise_frame(width: u32, height: u32) -> VideoFrame {
        let stride = width as usize * 4;
        let mut state = 0x1234_5678u32;
        let mut data = Vec::with_capacity(stride * height as usize);
        for _ in 0..(width as usize * height as usize) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            data.extend_from_slice(&[
                (state >> 8) as u8,
                (state >> 16) as u8,
                (state >> 24) as u8,
                255,
            ]);
        }
        VideoFrame {
            width,
            height,
            stride,
            data,
            pts_ns: Some(0),
        }
    }

    #[test]
    fn png_has_signature_and_is_not_trivial() {
        let dir = std::env::temp_dir().join("playback-proto-png-unit");
        let path = dir.join("solid.png");
        let frame = noise_frame(64, 64);
        write_frame_png(&frame, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert!(bytes.len() > 1024, "PNG 只有 {} 字节", bytes.len());
        // 宽高写在 IHDR 里（大端）。
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 64);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn padded_rows_are_repacked() {
        let width = 4u32;
        let height = 2u32;
        let stride = width as usize * 4 + 8;
        let mut data = vec![0u8; stride * height as usize];
        for row in 0..height as usize {
            for col in 0..width as usize {
                let offset = row * stride + col * 4;
                data[offset..offset + 4].copy_from_slice(&[1, 2, 3, 255]);
            }
        }
        let frame = VideoFrame {
            width,
            height,
            stride,
            data,
            pts_ns: None,
        };
        let dir = std::env::temp_dir().join("playback-proto-png-unit");
        let path = dir.join("padded.png");
        write_frame_png(&frame, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(&path);
    }
}
