// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Slint 图像路径：`VideoFrame(RGBA)` → `SharedPixelBuffer<Rgba8Pixel>` → `slint::Image`。
//!
//! 这里**不建窗口、不初始化 backend**：Slint 只当图片容器用。是否会因为没有
//! backend 而 panic，由 [`probe_no_backend`] 用 `catch_unwind` 实测，不靠猜。

use std::any::Any;
use std::hint::black_box;
use std::panic::{self, AssertUnwindSafe};
use std::time::Instant;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::bench::{SlintBench, SlintStep, stats};
use crate::{Error, Result, VideoFrame};

/// 把紧凑 RGBA8 拷进 Slint 的像素缓冲。
///
/// `SharedPixelBuffer::new` 会先按 `Pixel::default()` 整块初始化（RGBA8 即清零），
/// 之后 `copy_from_slice` 再写一遍——所以这一步实际是"两次写满整帧"。
pub fn buffer_from_rgba(
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<SharedPixelBuffer<Rgba8Pixel>> {
    let expected = width as usize * height as usize * VideoFrame::BYTES_PER_PIXEL;
    if data.len() != expected {
        return Err(Error::Format(format!(
            "RGBA 数据长度 {} 与 {}x{}x4={} 不符",
            data.len(),
            width,
            height,
            expected
        )));
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    buffer.make_mut_bytes().copy_from_slice(data);
    Ok(buffer)
}

/// 另一条构造路径：`SharedPixelBuffer::clone_from_slice` 一次拷贝完成
/// （内部直接按像素类型 clone，不像 `new` + `copy_from_slice` 那样先清零再写一遍）。
pub fn image_from_rgba_single_copy(data: &[u8], width: u32, height: u32) -> Result<Image> {
    let expected = width as usize * height as usize * VideoFrame::BYTES_PER_PIXEL;
    if data.len() != expected {
        return Err(Error::Format(format!(
            "RGBA 数据长度 {} 与 {}x{}x4={} 不符",
            data.len(),
            width,
            height,
            expected
        )));
    }
    Ok(Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
        data, width, height,
    )))
}

/// 紧凑 RGBA8 → `slint::Image`。
pub fn image_from_rgba(data: &[u8], width: u32, height: u32) -> Result<Image> {
    Ok(Image::from_rgba8(buffer_from_rgba(data, width, height)?))
}

/// `VideoFrame` → `slint::Image`。`stride > width*4` 时逐行压缩。
pub fn frame_to_image(frame: &VideoFrame) -> Result<Image> {
    if frame.stride == frame.width as usize * VideoFrame::BYTES_PER_PIXEL {
        return image_from_rgba(&frame.data, frame.width, frame.height);
    }
    let mut tight = Vec::with_capacity(
        frame.width as usize * frame.height as usize * VideoFrame::BYTES_PER_PIXEL,
    );
    for row in 0..frame.height {
        let row = frame
            .row(row)
            .ok_or_else(|| Error::Format(format!("第 {row} 行越界")))?;
        tight.extend_from_slice(row);
    }
    image_from_rgba(&tight, frame.width, frame.height)
}

/// 拆开量一次：分配 / 拷贝 / 构造 `Image`。
pub fn measure_step(data: &[u8], width: u32, height: u32) -> Result<SlintStep> {
    let expected = width as usize * height as usize * VideoFrame::BYTES_PER_PIXEL;
    if data.len() != expected {
        return Err(Error::Format(format!(
            "RGBA 数据长度 {} 与 {}x{}x4={} 不符",
            data.len(),
            width,
            height,
            expected
        )));
    }
    let started = Instant::now();
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let alloc_ns = started.elapsed().as_nanos() as u64;

    let started = Instant::now();
    buffer.make_mut_bytes().copy_from_slice(data);
    let copy_ns = started.elapsed().as_nanos() as u64;

    let started = Instant::now();
    let image = Image::from_rgba8(buffer);
    let construct_ns = started.elapsed().as_nanos() as u64;
    // 防止优化器把整条链优化掉。
    black_box(image.size());
    black_box(image);

    Ok(SlintStep {
        alloc_ns,
        copy_ns,
        construct_ns,
    })
}

/// 在合成缓冲上跑 N 次，得到该分辨率下 Slint 图像路径的统计。
pub fn bench_path(width: u32, height: u32, iterations: usize) -> Result<SlintBench> {
    let len = width as usize * height as usize * VideoFrame::BYTES_PER_PIXEL;
    // 用渐变内容而不是全 0，避免"写零"被内核/优化器特殊对待。
    let mut data = vec![0u8; len];
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    // 预热：4K 缓冲超过 glibc 的 mmap 阈值，前几次分配会走 mmap + 缺页，
    // 直接量会把首次缺页成本算进 p50。丢掉预热样本，只报稳态。
    for _ in 0..3 {
        let _ = measure_step(&data, width, height)?;
    }
    let mut alloc = Vec::with_capacity(iterations);
    let mut copy = Vec::with_capacity(iterations);
    let mut construct = Vec::with_capacity(iterations);
    let mut total = Vec::with_capacity(iterations);
    let mut single_copy = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let step = measure_step(&data, width, height)?;
        alloc.push(step.alloc_ns);
        copy.push(step.copy_ns);
        construct.push(step.construct_ns);
        total.push(step.total_ns());

        let started = Instant::now();
        let image = image_from_rgba_single_copy(&data, width, height)?;
        single_copy.push(started.elapsed().as_nanos() as u64);
        black_box(image);
    }
    // 只量 memcpy 本身：目标缓冲复用（页面已在、不分配），把分配/缺页成本排除掉。
    let mut reusable = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let mut memcpy_reused = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        reusable.make_mut_bytes().copy_from_slice(&data);
        memcpy_reused.push(started.elapsed().as_nanos() as u64);
    }
    black_box(reusable.make_mut_bytes().as_ptr());

    Ok(SlintBench {
        width,
        height,
        iterations,
        alloc: stats(&alloc),
        copy: stats(&copy),
        construct: stats(&construct),
        total: stats(&total),
        single_copy_total: stats(&single_copy),
        memcpy_reused: stats(&memcpy_reused),
    })
}

/// 一次无 backend 探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    Ok(String),
    Panicked(String),
}

impl ProbeOutcome {
    pub fn is_panic(&self) -> bool {
        matches!(self, ProbeOutcome::Panicked(_))
    }

    pub fn text(&self) -> &str {
        match self {
            ProbeOutcome::Ok(text) | ProbeOutcome::Panicked(text) => text,
        }
    }
}

/// 一条探测记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeStep {
    pub name: &'static str,
    pub outcome: ProbeOutcome,
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "<非字符串 panic payload>".to_string()
    }
}

fn capture(name: &'static str, action: impl FnOnce() -> String) -> ProbeStep {
    let outcome = match panic::catch_unwind(AssertUnwindSafe(action)) {
        Ok(text) => ProbeOutcome::Ok(text),
        Err(payload) => ProbeOutcome::Panicked(panic_message(payload)),
    };
    ProbeStep { name, outcome }
}

/// 实测"没有 backend 时这些 API 会不会 panic"。
///
/// 探测期间临时静音 panic hook，避免把预期内的 panic 打到 stderr 里。
pub fn probe_no_backend() -> Vec<ProbeStep> {
    let (width, height) = (64u32, 36u32);
    let data = vec![0x40u8; width as usize * height as usize * 4];

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let steps = vec![
        capture("SharedPixelBuffer::<Rgba8Pixel>::new", || {
            let buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
            format!(
                "ok: {}x{}, {} 字节",
                buffer.size().width,
                buffer.size().height,
                buffer.as_bytes().len()
            )
        }),
        capture("SharedPixelBuffer::make_mut_bytes + copy", || {
            let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
            buffer.make_mut_bytes().copy_from_slice(&data);
            format!("ok: 写入 {} 字节", buffer.as_bytes().len())
        }),
        capture("Image::from_rgba8", || {
            let image = image_from_rgba(&data, width, height).expect("构造 Image 失败");
            format!("ok: Image 构造成功, size={:?}", image.size())
        }),
        capture("Image::size()", || {
            let image = image_from_rgba(&data, width, height).expect("构造 Image 失败");
            let size = image.size();
            format!("ok: {}x{}", size.width, size.height)
        }),
        capture("Image::to_rgba8()", || {
            let image = image_from_rgba(&data, width, height).expect("构造 Image 失败");
            match image.to_rgba8() {
                Some(buffer) => format!(
                    "ok: to_rgba8 返回 {}x{} 缓冲",
                    buffer.size().width,
                    buffer.size().height
                ),
                None => "ok: to_rgba8 返回 None".to_string(),
            }
        }),
        capture("Image::from_rgba8 + clone + PartialEq", || {
            let image = image_from_rgba(&data, width, height).expect("构造 Image 失败");
            let cloned = image.clone();
            format!("ok: clone 后相等={}", cloned == image)
        }),
    ];
    panic::set_hook(previous_hook);
    steps
}

/// 把无 backend 探测结果拼成文本。
pub fn render_no_backend_probe(steps: &[ProbeStep]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Slint 无 backend 探测（未初始化任何 backend / 未建窗口）："
    );
    for step in steps {
        let verdict = if step.outcome.is_panic() {
            "PANIC"
        } else {
            "ok"
        };
        let _ = writeln!(
            out,
            "  [{verdict:>5}] {:<45} {}",
            step.name,
            step.outcome.text()
        );
    }
    out
}
