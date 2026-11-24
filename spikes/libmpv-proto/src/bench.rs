// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 每帧成本的统计与 `bench` 子命令。
//!
//! 分三段计时（同一帧内先后测量，互不重叠）：
//! 1. `frame_gap`：相邻两次渲染完成之间的间隔 = 拉帧 + 解码 + 渲染（端到端吞吐视角）；
//! 2. `render`：`mpv_render_context_render()` 的耗时 = SW 后端的缩放/色彩转换/写内存；
//! 3. `copy`：把帧从 mpv 写入的缓冲区 memcpy 到另一块“我们的”缓冲区；
//! 4. `alpha_fix`：把第 4 字节（mpv 的 rgb0 里那个 "0"，实测为 0）改成 255，
//!    因为消费端一般要求 RGBA 的 alpha 有效。
//!
//! SW 后端允许 mpv 直接写我们最终使用的缓冲区，所以第 3 段在实践中可以是 0 字节；
//! 这里仍然显式做一次 memcpy，用来量化“如果必须拷贝”要付多少钱。

use std::time::{Duration, Instant};

use crate::player::{Session, SessionOptions};
use crate::sys::{SurfaceFormat, SwRenderer};

/// 一组耗时的分布。
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub count: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub min: Duration,
    pub max: Duration,
    pub mean: Duration,
}

impl Stats {
    /// 样本数不足时返回 `None`。
    pub fn summarize(samples: &[Duration]) -> Option<Stats> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let pick = |q: f64| sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)];
        let total: Duration = sorted.iter().sum();
        Some(Stats {
            count: sorted.len(),
            p50: pick(0.50),
            p95: pick(0.95),
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            mean: total / sorted.len() as u32,
        })
    }
}

/// `bench` 的输入参数。
pub struct BenchOptions {
    pub frames: usize,
    pub format: SurfaceFormat,
    /// 前若干帧不计入统计（启动抖动）。
    pub warmup: usize,
    /// 循环结束后在同一帧上重复渲染这么多次，用于隔离“纯渲染”成本。
    pub repeat: usize,
    /// 渲染目标尺寸；`None` 表示用视频解码尺寸（不产生额外缩放/黑边）。
    pub size: Option<(u32, u32)>,
    /// 播放速度倍率。默认取 mpv 允许的最大值 100：
    /// vo_libmpv 不接受 `--untimed`（实测该选项对帧调度无效），帧总是被播放时钟
    /// 节流成“帧间隔”，所以测吞吐必须把时钟拉快。剩余节流 ≈ 帧间隔/speed。
    pub speed: f64,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            frames: 300,
            format: SurfaceFormat::Rgb0,
            warmup: 10,
            repeat: 30,
            size: None,
            speed: 100.0,
        }
    }
}

/// `bench` 的结果。
pub struct BenchReport {
    pub frames: usize,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub aligned: bool,
    pub frame_gap: Stats,
    pub render: Stats,
    pub copy: Stats,
    /// 把 alpha 字节写成 255 的耗时（Slint 一类消费者需要）。
    pub alpha_fix: Stats,
    /// 同一帧重复渲染（无解码、无新帧）的耗时。
    pub repeat_render: Stats,
    /// 暂停逐帧 step：从下发 `frame-step` 到渲染完成（不含播放时钟影响）。
    pub step: Stats,
    pub wall: Duration,
    pub hwdec_current: Option<String>,
    /// 本次测量使用的播放速度倍率（见 `BenchOptions::speed`）。
    pub speed: f64,
    pub dropped_frames: Option<i64>,
    pub decoder_dropped_frames: Option<i64>,
}

impl BenchReport {
    /// 由 `frame_gap` 推出来的端到端帧率上限（单线程串行渲染）。
    pub fn fps_from_p50(&self) -> f64 {
        1.0 / self.frame_gap.p50.as_secs_f64()
    }

    /// 从 `frame_gap` 里扣掉 `render` 得到的“每帧解码/取帧”时间（p50 相减）。
    pub fn decode_p50(&self) -> Duration {
        self.frame_gap.p50.saturating_sub(self.render.p50)
    }

    pub fn decode_p95(&self) -> Duration {
        self.frame_gap.p95.saturating_sub(self.render.p95)
    }
}

/// 跑一次基准。
pub fn run(
    file: &std::path::Path,
    opts: &BenchOptions,
    session_opts: &SessionOptions<'_>,
) -> Result<BenchReport, String> {
    // 素材可能比 N 帧短；循环播放保证能凑齐 N 帧（每圈只有一次 seek）。
    let speed_value = format!("{:.3}", opts.speed);
    let mut extra: Vec<(&str, &str)> = session_opts.extra.to_vec();
    extra.push(("loop-file", "inf"));
    extra.push(("speed", speed_value.as_str()));
    let session_opts = SessionOptions {
        hwdec: session_opts.hwdec,
        extra: &extra,
    };
    let session = Session::open(file, &session_opts)?;
    session.wait_loaded(Duration::from_secs(20))?;
    // 测“每帧成本”必须排除“等该帧的显示时间”：否则 render() 会一直阻塞到
    // 帧间隔（实测 30fps 素材就是 33.3ms），把节流误当成渲染成本。
    // renderer 不可变借用，所以用内部可变的 Cell 来翻转这个开关。
    let format = opts.format;
    let mut img = session.first_frame_with_size(format, Duration::from_secs(20), opts.size)?;
    let (width, height) = (img.width(), img.height());

    let player = session.player();
    let renderer = session.renderer();
    // render() 默认会阻塞到该帧的“目标显示时间”，测成本时必须关掉。
    renderer.set_block_for_target_time(false);
    let hwdec_current = session.hwdec_current();

    // “拷进我们的 RGBA buffer”那一段的参照实现：一块不带 padding 的紧凑缓冲区。
    let mut tight = vec![0u8; width as usize * height as usize * 4];

    let mut gap_samples: Vec<Duration> = Vec::with_capacity(opts.frames);
    let mut render_samples: Vec<Duration> = Vec::with_capacity(opts.frames);
    let mut copy_samples: Vec<Duration> = Vec::with_capacity(opts.frames);
    let mut alpha_samples: Vec<Duration> = Vec::with_capacity(opts.frames);

    let mut rendered = 0usize;
    let mut last_frame_end = Instant::now();
    let started = Instant::now();
    // 兜底超时：素材/机器异常时不要让 bench 永久挂住。
    let deadline = started + Duration::from_secs(120);

    while rendered < opts.frames {
        if session.drain_events()? {
            // loop-file=inf 理论上不会走到这里，容错一下。
            eprintln!(
                "warning: 播放提前结束，只渲染了 {rendered}/{} 帧",
                opts.frames
            );
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "超时：{deadline:?} 内只渲染了 {rendered}/{} 帧",
                opts.frames
            ));
        }
        if !SwRenderer::has_new_frame(renderer.update()) {
            renderer.signal().wait(Duration::from_millis(20));
            continue;
        }
        let t0 = Instant::now();
        renderer.render_into(format, &mut img)?;
        let t1 = Instant::now();
        // 参照拷贝：stride 带 padding，所以逐行拷到紧凑缓冲区。
        for y in 0..height as usize {
            let src = img.row(y);
            let dst = &mut tight[y * width as usize * 4..(y + 1) * width as usize * 4];
            dst.copy_from_slice(src);
        }
        let t2 = Instant::now();
        img.fill_alpha(255);
        let t3 = Instant::now();

        if rendered >= opts.warmup {
            gap_samples.push(t1 - last_frame_end);
            render_samples.push(t1 - t0);
            copy_samples.push(t2 - t1);
            alpha_samples.push(t3 - t2);
        }
        last_frame_end = t1;
        rendered += 1;
    }
    let wall = started.elapsed();

    // 隔离“纯渲染”：暂停后同一帧反复渲染，不涉及解码。
    session.pause()?;
    // 让已经排队的帧走完，避免下面的重复渲染里混进解码。
    let drain_deadline = Instant::now() + Duration::from_secs(2);
    while SwRenderer::has_new_frame(renderer.update()) && Instant::now() < drain_deadline {
        renderer.render_into(format, &mut img)?;
        session.drain_events()?;
    }
    let mut repeat_samples = Vec::with_capacity(opts.repeat);
    for _ in 0..opts.repeat {
        let t0 = Instant::now();
        renderer.render_into(format, &mut img)?;
        repeat_samples.push(t0.elapsed());
    }

    // 另一条完全不受播放时钟影响的路径：暂停后逐帧 step，测量“下发命令→拿到帧→渲染”。
    // 这是 ADR 里“抽出单帧”的用法（seek/截图），与上面的吞吐数字不可直接相加。
    let mut step_samples = Vec::with_capacity(opts.repeat);
    for _ in 0..opts.repeat {
        let t0 = Instant::now();
        session.player().command(&["frame-step"])?;
        let step_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if SwRenderer::has_new_frame(renderer.update()) {
                renderer.render_into(format, &mut img)?;
                break;
            }
            if Instant::now() >= step_deadline {
                break;
            }
            renderer.signal().wait(Duration::from_millis(5));
        }
        step_samples.push(t0.elapsed());
    }

    let dropped_frames = player.get_i64("frame-drop-count");
    let decoder_dropped_frames = player.get_i64("decoder-frame-drop-count");

    Ok(BenchReport {
        frames: render_samples.len(),
        width,
        height,
        stride: img.stride(),
        aligned: img.is_aligned(),
        frame_gap: Stats::summarize(&gap_samples).ok_or("没有帧间隔样本")?,
        render: Stats::summarize(&render_samples).ok_or("没有渲染样本")?,
        copy: Stats::summarize(&copy_samples).ok_or("没有拷贝样本")?,
        alpha_fix: Stats::summarize(&alpha_samples).ok_or("没有 alpha 修正样本")?,
        repeat_render: Stats::summarize(&repeat_samples).ok_or("没有重复渲染样本")?,
        step: Stats::summarize(&step_samples).ok_or("没有 step 样本")?,
        wall,
        speed: opts.speed,
        hwdec_current,
        dropped_frames,
        decoder_dropped_frames,
    })
}

/// 打印成人类可读的表格。
pub fn print_report(file: &std::path::Path, report: &BenchReport, format: SurfaceFormat) {
    println!("文件: {}", file.display());
    println!(
        "渲染目标: {}x{} 格式={} stride={} (64 字节对齐: {})",
        report.width,
        report.height,
        format.label(),
        report.stride,
        if report.aligned { "是" } else { "否" }
    );
    println!(
        "统计帧数: {}（已跳过 warmup 帧）  总耗时: {:.3}s  端到端 p50 帧率: {:.1} fps (speed={:.0}x)",
        report.frames,
        report.wall.as_secs_f64(),
        report.fps_from_p50(),
        report.speed
    );
    println!(
        "hwdec-current: {}",
        report.hwdec_current.as_deref().unwrap_or("(空)")
    );
    println!(
        "丢帧: frame-drop-count={:?} decoder-frame-drop-count={:?}",
        report.dropped_frames, report.decoder_dropped_frames
    );
    println!();
    println!(
        "{:<34} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "阶段", "p50(µs)", "p95(µs)", "min(µs)", "max(µs)", "mean(µs)"
    );
    let us = |d: Duration| format!("{:.1}", d.as_secs_f64() * 1e6);
    let row_full = |name: &str, s: &Stats| {
        println!(
            "{:<34} {:>10} {:>10} {:>10} {:>10} {:>10}",
            name,
            us(s.p50),
            us(s.p95),
            us(s.min),
            us(s.max),
            us(s.mean)
        );
    };
    row_full("mpv 解码+渲染 (帧间隔)", &report.frame_gap);
    row_full("  其中 mpv_render_context_render", &report.render);
    // 这一行是差值推算，只有 p50/p95 有意义。
    println!(
        "{:<34} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "  推算: 取帧+解码 = 帧间隔-渲染",
        us(report.decode_p50()),
        us(report.decode_p95()),
        "-",
        "-",
        "-"
    );
    row_full("拷贝到紧凑 buffer (参照)", &report.copy);
    row_full("alpha 字节置 255 (Slint 需要)", &report.alpha_fix);
    row_full("同一帧重复渲染 (无解码)", &report.repeat_render);
    row_full("逐帧 step: 命令→帧→渲染", &report.step);
    println!();
    println!(
        "零拷贝: SW 后端直接把像素写进调用方的缓冲区（本原型里就是 AlignedImage），\
         上表“拷贝”一行只是为了量化“若必须拷贝”的成本；把最终缓冲区直接交给 mpv 时该段为 0。"
    );
}
