// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 三段耗时统计：解码出帧、NV12→RGBA、memcpy 进 Slint buffer。

use std::fmt::Write as _;
#[cfg(feature = "slint")]
use std::hint::black_box;
use std::time::{Duration, Instant};

use crate::gst_source::GstSource;
use crate::{Error, FrameSource, Result};

/// 1080p60 的单帧预算。
pub const FRAME_BUDGET_1080P60_NS: u64 = 16_600_000;

/// 一组样本的统计量（纳秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub count: usize,
    pub min_ns: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub max_ns: u64,
    pub mean_ns: u64,
}

impl Stats {
    pub fn p50_ms(&self) -> f64 {
        self.p50_ns as f64 / 1e6
    }

    pub fn p95_ms(&self) -> f64 {
        self.p95_ns as f64 / 1e6
    }

    /// 占 1080p60 单帧预算的百分比。
    pub fn budget_percent(&self) -> f64 {
        self.p50_ns as f64 / FRAME_BUDGET_1080P60_NS as f64 * 100.0
    }
}

/// 计算 p50/p95（最近秩法：`ceil(p*n)` 对应的样本）。空输入返回全 0。
pub fn stats(samples: &[u64]) -> Stats {
    if samples.is_empty() {
        return Stats::default();
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let sum: u128 = sorted.iter().map(|value| *value as u128).sum();
    Stats {
        count: sorted.len(),
        min_ns: sorted[0],
        p50_ns: percentile(&sorted, 0.50),
        p95_ns: percentile(&sorted, 0.95),
        max_ns: *sorted.last().unwrap_or(&0),
        mean_ns: (sum / sorted.len() as u128) as u64,
    }
}

fn percentile(sorted: &[u64], fraction: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let count = sorted.len();
    let rank = (fraction * count as f64).ceil().max(1.0) as usize;
    sorted[rank.min(count) - 1]
}

/// Slint 图像路径的单次拆分耗时。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlintStep {
    /// `SharedPixelBuffer::new`（按 `Pixel::default()` 初始化整块内存）。
    pub alloc_ns: u64,
    /// `make_mut_bytes().copy_from_slice(RGBA)`。
    pub copy_ns: u64,
    /// `Image::from_rgba8`。
    pub construct_ns: u64,
}

impl SlintStep {
    pub fn total_ns(&self) -> u64 {
        self.alloc_ns + self.copy_ns + self.construct_ns
    }
}

/// 一条 Slint 图像路径的统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlintBench {
    pub width: u32,
    pub height: u32,
    pub iterations: usize,
    pub alloc: Stats,
    pub copy: Stats,
    pub construct: Stats,
    pub total: Stats,
    /// `SharedPixelBuffer::clone_from_slice` 一次拷贝的合计耗时（对照）。
    pub single_copy_total: Stats,
    /// 复用同一个 `SharedPixelBuffer` 时纯 memcpy 的耗时（排除每次新建的分配/缺页）。
    pub memcpy_reused: Stats,
}

/// `bench` 的完整结果。
#[derive(Debug, Clone, Default)]
pub struct BenchReport {
    /// 第二趟（转换 + Slint）实际测到的帧数。
    pub frames: usize,
    /// 第一趟（纯解码）实际测到的帧数。
    pub decode_frames: usize,
    pub width: u32,
    pub height: u32,
    /// 第一趟墙钟：只拉帧、不干别的。
    pub decode_wall: Duration,
    /// 第二趟墙钟：转换 + Slint 拷贝。
    pub stage_wall: Duration,
    /// `pull_sample` 阻塞等待下一帧解码结果的时间。
    pub decode: Stats,
    /// NV12→RGBA，复用目标缓冲区（不含分配）。
    pub convert: Stats,
    /// NV12→RGBA，每帧新建目标 `Vec`（含分配 + 首次触碰内存）。
    pub convert_fresh: Stats,
    /// 实际素材尺寸上的 Slint 图像路径。
    pub slint: Option<SlintBench>,
    /// 合成 1080p 缓冲上的 Slint 图像路径。
    pub slint_1080p: Option<SlintBench>,
    /// 合成 4K 缓冲上的 Slint 图像路径。
    pub slint_4k: Option<SlintBench>,
}

impl BenchReport {
    /// 拼出人读的表格。
    pub fn render(&self) -> String {
        let mut out = String::new();
        let decode_seconds = self.decode_wall.as_secs_f64();
        let decode_fps = if decode_seconds > 0.0 {
            self.decode_frames as f64 / decode_seconds
        } else {
            0.0
        };
        let stage_seconds = self.stage_wall.as_secs_f64();
        let stage_fps = if stage_seconds > 0.0 {
            self.frames as f64 / stage_seconds
        } else {
            0.0
        };
        let _ = writeln!(
            out,
            "帧数            : {} 帧解码 / {} 帧转换 @ {}x{}",
            self.decode_frames, self.frames, self.width, self.height
        );
        let _ = writeln!(
            out,
            "第一趟(纯解码)  : {:.3} s → {:.2} fps（sync=false，不受播放时钟限制）",
            decode_seconds, decode_fps
        );
        let _ = writeln!(
            out,
            "第二趟(转换+Slint): {:.3} s → {:.2} fps（被本进程 CPU 工作限速）",
            stage_seconds, stage_fps
        );
        let _ = writeln!(out, "{:<24}p50 / p95 / mean / max（ms）", "阶段");
        let _ = writeln!(
            out,
            "{:<24}{:.3} / {:.3} / {:.3} / {:.3}   [{} 帧]",
            "解码出帧(帧间隔+NV12)",
            self.decode.p50_ms(),
            self.decode.p95_ms(),
            self.decode.mean_ns as f64 / 1e6,
            self.decode.max_ns as f64 / 1e6,
            self.decode.count
        );
        let _ = writeln!(
            out,
            "{:<24}{:.3} / {:.3} / {:.3} / {:.3}   [{:.1}% of 16.6ms]",
            "NV12→RGBA(复用缓冲)",
            self.convert.p50_ms(),
            self.convert.p95_ms(),
            self.convert.mean_ns as f64 / 1e6,
            self.convert.max_ns as f64 / 1e6,
            self.convert.budget_percent()
        );
        let _ = writeln!(
            out,
            "{:<24}{:.3} / {:.3} / {:.3} / {:.3}   [{:.1}% of 16.6ms]",
            "NV12→RGBA(每帧新建)",
            self.convert_fresh.p50_ms(),
            self.convert_fresh.p95_ms(),
            self.convert_fresh.mean_ns as f64 / 1e6,
            self.convert_fresh.max_ns as f64 / 1e6,
            self.convert_fresh.budget_percent()
        );
        if let Some(slint) = &self.slint {
            out.push_str(&render_slint("Slint 图像路径(素材)", slint));
        }
        if let Some(slint) = &self.slint_1080p {
            out.push_str(&render_slint("Slint 图像路径(1080p)", slint));
        }
        if let Some(slint) = &self.slint_4k {
            out.push_str(&render_slint("Slint 图像路径(4K)", slint));
        }
        if self.slint.is_none() {
            let _ = writeln!(out, "Slint 图像路径  : 未启用（feature `slint` 关闭）");
        }
        out
    }
}

fn render_slint(label: &str, bench: &SlintBench) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{label} {}x{}，{} 次：",
        bench.width, bench.height, bench.iterations
    );
    for (name, stats) in [
        ("  SharedPixelBuffer::new", &bench.alloc),
        ("  memcpy → Slint buffer", &bench.copy),
        ("  Image::from_rgba8", &bench.construct),
        ("  合计(new+copy)", &bench.total),
        ("  合计(clone_from_slice)", &bench.single_copy_total),
        ("  纯 memcpy(复用目标缓冲)", &bench.memcpy_reused),
    ] {
        if stats.count == 0 {
            continue;
        }
        let _ = writeln!(
            out,
            "{:<26}{:.3} / {:.3} / {:.3} / {:.3} ms   [{:.1}% of 16.6ms]",
            name,
            stats.p50_ms(),
            stats.p95_ms(),
            stats.mean_ns as f64 / 1e6,
            stats.max_ns as f64 / 1e6,
            stats.budget_percent()
        );
    }
    out
}

/// 跑 `frames` 帧的三段测量。
///
/// 分两趟：
/// 1. 新开一个 source，只做 `pull_decoded()`，量"解码出帧"的**帧间隔**（消费者不干活，
///    所以这个间隔就是 demux/parse/解码/转 NV12 的生产速率）。
/// 2. 在调用方给的 source 上，量 NV12→RGBA 与 Slint 图像路径。
///
/// 合成一帧解码的耗时放进同一趟会互相污染（`pull_sample` 变成 0，测的其实是消费者限速）。
pub fn run(source: &mut GstSource, frames: usize) -> Result<BenchReport> {
    let description = source.description();
    let video = description
        .video
        .as_ref()
        .ok_or_else(|| Error::Gst("探测不到视频流，无法 bench".into()))?;
    let width = video.width;
    let height = video.height;

    // 第一趟：解码吞吐。
    let decode_source = GstSource::open(&description.uri)?;
    let mut decode_ns = Vec::with_capacity(frames);
    decode_source.pull_decoded()?; // 预热：丢掉首帧（包含 preroll 后首次出帧的额外等待）
    let mut last = Instant::now();
    let decode_started = Instant::now();
    while decode_ns.len() < frames {
        if decode_source.pull_decoded()?.is_none() {
            break;
        }
        let now = Instant::now();
        decode_ns.push(now.duration_since(last).as_nanos() as u64);
        last = now;
    }
    let decode_wall = decode_started.elapsed();
    drop(decode_source);

    // 第二趟：转换与 Slint。
    let mut convert_ns = Vec::with_capacity(frames);
    let mut fresh_ns = Vec::with_capacity(frames);
    #[cfg(feature = "slint")]
    let mut slint_steps: Vec<SlintStep> = Vec::with_capacity(frames);
    #[cfg(feature = "slint")]
    let mut single_copy_ns: Vec<u64> = Vec::with_capacity(frames);
    let mut scratch = Vec::new();
    let stage_started = Instant::now();
    while convert_ns.len() < frames {
        let Some(sample) = source.pull_decoded()? else {
            break;
        };
        let caps = sample
            .caps()
            .ok_or_else(|| Error::Format("sample 没有 caps".into()))?;
        let info = gstreamer_video::VideoInfo::from_caps(caps)
            .map_err(|err| Error::Format(format!("caps 无法解析: {err}")))?;

        let convert_started = Instant::now();
        crate::gst_source::sample_to_rgba(&sample, &info, &mut scratch)?;
        convert_ns.push(convert_started.elapsed().as_nanos() as u64);

        // 按真实播放顺序：转换完立刻交给 Slint（源缓冲在缓存里是热的）。
        #[cfg(feature = "slint")]
        {
            let step = crate::slint_bridge::measure_step(&scratch, info.width(), info.height())?;
            slint_steps.push(step);

            let started = Instant::now();
            let image = crate::slint_bridge::image_from_rgba_single_copy(
                &scratch,
                info.width(),
                info.height(),
            )?;
            single_copy_ns.push(started.elapsed().as_nanos() as u64);
            black_box(image);
        }

        // 对照实验放在后面：`FrameSource::next_frame()` 每帧都会新建 Vec，
        // 这一步量出"分配 + 首次触碰内存"的额外成本。
        let mut fresh = Vec::new();
        let fresh_started = Instant::now();
        crate::gst_source::sample_to_rgba(&sample, &info, &mut fresh)?;
        fresh_ns.push(fresh_started.elapsed().as_nanos() as u64);
    }
    let stage_wall = stage_started.elapsed();

    let slint = {
        #[cfg(feature = "slint")]
        {
            (!slint_steps.is_empty()).then(|| SlintBench {
                width: width.max(1),
                height: height.max(1),
                iterations: slint_steps.len(),
                alloc: stats(
                    &slint_steps
                        .iter()
                        .map(|step| step.alloc_ns)
                        .collect::<Vec<_>>(),
                ),
                copy: stats(
                    &slint_steps
                        .iter()
                        .map(|step| step.copy_ns)
                        .collect::<Vec<_>>(),
                ),
                construct: stats(
                    &slint_steps
                        .iter()
                        .map(|step| step.construct_ns)
                        .collect::<Vec<_>>(),
                ),
                single_copy_total: stats(&single_copy_ns),
                memcpy_reused: Stats::default(),
                total: stats(
                    &slint_steps
                        .iter()
                        .map(SlintStep::total_ns)
                        .collect::<Vec<_>>(),
                ),
            })
        }
        #[cfg(not(feature = "slint"))]
        {
            None
        }
    };

    #[cfg(feature = "slint")]
    let (slint_1080p, slint_4k) = {
        let iterations = 40;
        let full_hd = crate::slint_bridge::bench_path(1920, 1080, iterations)?;
        let uhd = crate::slint_bridge::bench_path(3840, 2160, iterations)?;
        (Some(full_hd), Some(uhd))
    };
    #[cfg(not(feature = "slint"))]
    let (slint_1080p, slint_4k) = (None, None);

    Ok(BenchReport {
        frames: convert_ns.len(),
        decode_frames: decode_ns.len(),
        width,
        height,
        decode_wall,
        stage_wall,
        decode: stats(&decode_ns),
        convert: stats(&convert_ns),
        convert_fresh: stats(&fresh_ns),
        slint,
        slint_1080p,
        slint_4k,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_of_simple_series() {
        let values: Vec<u64> = (1..=100).collect();
        let stats = stats(&values);
        assert_eq!(stats.count, 100);
        assert_eq!(stats.min_ns, 1);
        assert_eq!(stats.max_ns, 100);
        assert_eq!(stats.p50_ns, 50);
        assert_eq!(stats.p95_ns, 95);
        assert_eq!(stats.mean_ns, 50);
    }

    #[test]
    fn empty_stats_are_zero() {
        assert_eq!(stats(&[]), Stats::default());
    }

    #[test]
    fn budget_percentage_uses_16_6ms() {
        let stats = stats(&[8_300_000]);
        assert!((stats.budget_percent() - 50.0).abs() < 0.001);
    }
}
