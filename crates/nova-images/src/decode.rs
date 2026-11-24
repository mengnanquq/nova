// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 解码：任意支持的图片格式 → 统一 RGBA8，可按最大宽度等比缩小。

use image::imageops::FilterType;
use image::{RgbaImage, imageops};

use crate::error::ImageError;

/// 解码后的图片：RGBA8，行连续（每行 `width * 4` 字节，无行间填充）。
///
/// 这是界面纹理上传唯一需要的形态，所以缓存层直接把"格式差异"消化在这里：
/// 调用方不必关心服务端给的是 PNG 还是 JPEG，也不必关心 ICC、EXIF 方向
/// （`image` crate 默认不旋转；媒体服务器的海报不会有 EXIF 方向）。
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// 宽度（像素）。
    pub width: u32,
    /// 高度（像素）。
    pub height: u32,
    /// RGBA8 像素数据，长度恒为 `width * height * 4`。
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for DecodedImage {
    /// 不打印像素数据：一张 4K 海报是几十 MB，`Debug` 进日志会直接把日志撑爆。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.rgba.len())
            .finish()
    }
}

/// 把图片字节解码成 RGBA8；`max_width` 小于原始宽度时等比缩小。
///
/// `endpoint` 必须是已经脱敏的地址（[`endpoint_of`](crate::endpoint_of) 的输出），
/// 它只会出现在错误信息里。
///
/// 缩放用 [`FilterType::Triangle`]：它比最近邻平滑、比 Lanczos3 快一个数量级，
/// 而海报/背景图缩小到几百像素宽时，肉眼分辨不出 Triangle 与 Lanczos 的差别。
pub(crate) fn decode(
    bytes: &[u8],
    endpoint: &str,
    max_width: Option<u32>,
) -> Result<DecodedImage, ImageError> {
    if bytes.is_empty() {
        return Err(ImageError::EmptyBody {
            endpoint: endpoint.to_owned(),
        });
    }

    // `load_from_memory` 自己嗅探格式；只编进了 png / jpeg / webp 三种解码器，
    // 因此 GIF（会动）、AVIF 之类会走到这里的解码错误分支，而不是被静默当成静态图。
    let decoded = image::load_from_memory(bytes).map_err(|source| ImageError::Decode {
        endpoint: endpoint.to_owned(),
        bytes: bytes.len(),
        source,
    })?;

    let rgba = decoded.to_rgba8();
    let (width, _) = rgba.dimensions();

    let rgba = match max_width {
        Some(limit) if limit > 0 && width > limit => resize_to_width(&rgba, limit),
        _ => rgba,
    };

    let (width, height) = rgba.dimensions();
    Ok(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// 按目标宽度等比缩放（高度四舍五入，至少 1 像素）。
fn resize_to_width(image: &RgbaImage, target_width: u32) -> RgbaImage {
    let (width, height) = image.dimensions();
    // 调用方保证 `width > target_width > 0`；这里仍用 `checked_div`，
    // 免得将来有人改写调用条件后在某台机器上看到除零 panic。
    let target_height = u64::from(height)
        .saturating_mul(u64::from(target_width))
        .checked_div(u64::from(width))
        .unwrap_or(0)
        .max(1)
        .min(u64::from(u32::MAX)) as u32;

    imageops::resize(image, target_width, target_height, FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
            .expect("测试内编码 PNG 不应失败");
        out
    }

    #[test]
    fn decodes_to_rgba8_with_expected_size() {
        let image = decode(&png(40, 30), "https://host/x", None).expect("应能解码");
        assert_eq!((image.width, image.height), (40, 30));
        assert_eq!(image.rgba.len(), 40 * 30 * 4);
    }

    #[test]
    fn scales_down_keeping_aspect_ratio() {
        let image = decode(&png(400, 300), "https://host/x", Some(200)).expect("应能解码");
        assert_eq!((image.width, image.height), (200, 150));
        assert_eq!(image.rgba.len(), 200 * 150 * 4);
    }

    #[test]
    fn never_scales_up() {
        let image = decode(&png(100, 80), "https://host/x", Some(200)).expect("应能解码");
        assert_eq!((image.width, image.height), (100, 80));
    }

    #[test]
    fn rejects_empty_and_garbage() {
        let empty = decode(&[], "https://host/x", None).expect_err("空响应应报错");
        assert!(matches!(empty, ImageError::EmptyBody { .. }));

        let garbage =
            decode(b"<html>500</html>", "https://host/x", None).expect_err("非图片应报错");
        assert!(matches!(garbage, ImageError::Decode { .. }));
    }
}
