// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `--play … --capture <png> --seconds <n>`：真实窗口里的端到端验证。
//!
//! 流程：直达播放页 → 起播 → 播 `seconds` 秒 → 在 `AfterRendering` 里 `glReadPixels`
//! 读窗口 back buffer → 写 PNG → 打印五项指标 → 退出码 0（通过）/ 3（不通过）。
//!
//! 五项验收标准（`docs/ui-architecture.md` 播放器规范 + ADR-0001）：
//!
//! | 指标 | 阈值 |
//! |---|---|
//! | 首帧耗时 | < 1.5 s |
//! | 媒体位置推进 | ≥ `seconds - 1` 秒 |
//! | `hwdec-current` | `vaapi` |
//! | 捕获画面 | 非全黑（均值 > 8、方差 > 100） |
//! | 丢帧 | ≤ 5 |

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 捕获参数（命令行）。
#[derive(Clone, Debug)]
pub struct Options {
    /// PNG 落盘路径。
    pub(crate) path: PathBuf,
    /// 要播放多久之后截图。
    pub(crate) seconds: f64,
    /// 首帧预算。局域网默认 1.5s；真实 WAN 上的 4K HDR 需要放宽（实测 4.7s）。
    pub(crate) max_first_frame: std::time::Duration,
}

/// 一次捕获的运行状态。
#[derive(Debug)]
pub(crate) struct Capture {
    options: Options,
    /// 首帧时刻（会话第一次离开 `Opening` 之后）。
    first_frame_at: Option<Instant>,
    /// 首帧时的媒体位置（推进量的基准）。
    start_position: Duration,
    /// 已经在 `AfterRendering` 里做过回读。
    captured: bool,
}

/// 一次捕获的结果。
#[derive(Clone, Debug)]
pub(crate) struct Outcome {
    /// 首帧耗时（秒）。
    pub(crate) first_frame_s: f64,
    /// 渲染进纹理的帧数。
    pub(crate) frames: u64,
    /// mpv 报告的累计丢帧。
    pub(crate) dropped: u64,
    /// `hwdec-current`。
    pub(crate) hwdec: String,
    /// 媒体位置推进（秒）。
    pub(crate) advance_s: f64,
    /// 首帧之后实际经过的墙钟时间（秒）。
    pub(crate) wall_s: f64,
    /// 捕获画面平均亮度。
    pub(crate) mean_luma: f64,
    /// 捕获画面亮度方差。
    pub(crate) variance: f64,
    /// 捕获画面非黑像素占比。
    pub(crate) non_black: f64,
    /// 窗口尺寸（物理像素）。
    pub(crate) window: (u32, u32),
    /// PNG 路径。
    pub(crate) path: PathBuf,
    /// 未达标的指标（空 = 通过）。
    pub(crate) failures: Vec<String>,
}

impl Options {
    /// 由命令行参数构造。
    pub(crate) fn new(path: PathBuf, seconds: f64) -> Self {
        Self {
            path,
            seconds: if seconds.is_finite() && seconds > 0.0 {
                seconds
            } else {
                5.0
            },
            max_first_frame: std::time::Duration::from_millis(1500),
        }
    }

    /// 覆盖首帧预算（`--max-first-frame`）。
    pub(crate) fn with_max_first_frame(mut self, max: std::time::Duration) -> Self {
        self.max_first_frame = max;
        self
    }
}

impl Capture {
    /// 新建一次捕获。
    pub(crate) fn new(options: Options) -> Self {
        Self {
            options,
            first_frame_at: None,
            start_position: Duration::ZERO,
            captured: false,
        }
    }

    /// PNG 路径。
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.options.path
    }

    /// 记号：首帧到了（只在第一次生效）。
    pub(crate) fn note_first_frame(&mut self, position: Duration) {
        if self.first_frame_at.is_none() {
            self.first_frame_at = Some(Instant::now());
            self.start_position = position;
        }
    }

    /// 是否该回读了。
    pub(crate) fn due(&self) -> bool {
        match self.first_frame_at {
            Some(at) => at.elapsed().as_secs_f64() >= self.options.seconds,
            None => false,
        }
    }

    /// 是否已经回读过。
    pub(crate) fn captured(&self) -> bool {
        self.captured
    }

    /// 标记已回读。
    pub(crate) fn mark_captured(&mut self) {
        self.captured = true;
    }

    /// 起播至今的墙钟时间（首帧之后）。
    fn wall(&self) -> Duration {
        self.first_frame_at
            .map(|at| at.elapsed())
            .unwrap_or_default()
    }

    /// 首帧以来媒体位置推进了多少。
    fn advance(&self, position: Duration) -> Duration {
        position.saturating_sub(self.start_position)
    }

    /// 组装结果并判定是否通过。
    ///
    /// 参数多是有意的：这是一次性诊断的终点，把 8 个已测好的值塞进结构体只会多一层间接。
    #[expect(
        clippy::too_many_arguments,
        reason = "捕获模式的一次性判定函数，参数都是刚测出的独立指标"
    )]
    pub(crate) fn finish(
        &self,
        position: Duration,
        frames: u64,
        dropped: u64,
        hwdec: String,
        first_frame: Option<Duration>,
        window: (u32, u32),
        duration: Option<std::time::Duration>,
        stats: &super::gl::PixelStats,
    ) -> Outcome {
        let mut failures = Vec::new();
        let first_frame_s = first_frame.map(|value| value.as_secs_f64()).unwrap_or(-1.0);
        // 首帧预算按链路区分：局域网/本机是几十毫秒，真实 WAN 上的 4K HDR 起播是几秒
        // （实测远程 Emby：4.658s）。把 LAN 的阈值当普适标准只会让工具说谎，
        // 所以阈值可配（`--max-first-frame`），默认仍是 LAN 口径。
        let budget = self.options.max_first_frame.as_secs_f64();
        if !(0.0..budget).contains(&first_frame_s) {
            failures.push(format!(
                "首帧耗时 {first_frame_s:.3}s ≥ {budget:.2}s 或未知"
            ));
        }
        let advance_s = self.advance(position).as_secs_f64();
        // 起播那段本来就不可能推进：WAN 上 4K HDR 首帧要几秒（实测 4.7s）。
        // 所以按"首帧之后还剩多少时间"来要求，再留 10% 给缓冲抖动。
        let playable_s = (self.options.seconds - first_frame_s.max(0.0)).max(0.0);
        let required = playable_s * 0.9;
        // 播到结尾就算通过：`keep-open=yes` 会把最后一帧停在 EOF，位置不再推进，
        // 这时"推进不足"是必然的，不是缺陷（实测：660s 的请求碰上 400s 的素材，
        // 位置停在 400s，被误判成"未通过"）。
        let reached_end = duration.is_some_and(|total| {
            !total.is_zero() && position + std::time::Duration::from_secs(1) >= total
        });
        if reached_end {
            println!(
                "[capture] 备注           : 已播到素材结尾（位置停在 {:.2}s）",
                position.as_secs_f64()
            );
        } else if advance_s < required {
            failures.push(format!(
                "位置只推进了 {advance_s:.2}s（要求 ≥ {required:.2}s）"
            ));
        }
        if !hwdec.eq_ignore_ascii_case("vaapi") {
            failures.push(format!("hwdec-current={hwdec}（要求 vaapi）"));
        }
        if !stats.looks_like_video() {
            failures.push(format!(
                "捕获画面全黑或接近全黑（均值 {:.1}，方差 {:.0}）",
                stats.mean_luma, stats.variance
            ));
        }
        if dropped > 5 {
            failures.push(format!("丢帧 {dropped} 帧（要求 ≤ 5）"));
        }
        Outcome {
            first_frame_s,
            frames,
            dropped,
            hwdec,
            advance_s,
            wall_s: self.wall().as_secs_f64(),
            mean_luma: stats.mean_luma,
            variance: stats.variance,
            non_black: stats.non_black_ratio,
            window,
            path: self.options.path.clone(),
            failures,
        }
    }
}

impl Outcome {
    /// 是否通过全部验收。
    pub(crate) fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// 打印人可读的统计（stdout，脚本可以直接 grep）。
    pub(crate) fn print(&self) {
        println!("[capture] PNG            : {}", self.path.display());
        println!(
            "[capture] 窗口尺寸       : {}x{}",
            self.window.0, self.window.1
        );
        println!("[capture] 首帧耗时       : {:.3}s", self.first_frame_s);
        println!("[capture] 渲染帧数       : {}", self.frames);
        println!("[capture] 丢帧           : {}", self.dropped);
        println!("[capture] hwdec-current  : {}", self.hwdec);
        println!(
            "[capture] 位置推进       : {:.2}s（{:.2}s 墙钟）",
            self.advance_s, self.wall_s
        );
        println!(
            "[capture] 画面           : 均值 {:.1}，方差 {:.0}，非黑 {:.1}%",
            self.mean_luma,
            self.variance,
            self.non_black * 100.0
        );
        if self.passed() {
            println!("[capture] 结论           : 通过");
        } else {
            println!("[capture] 结论           : 未通过");
            for failure in &self.failures {
                println!("[capture]   - {failure}");
            }
        }
    }

    /// 失败原因文本（退出码 3 时给 [`AppError`](crate::error::AppError) 用）。
    pub(crate) fn failure_text(&self) -> String {
        self.failures.join("；")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::gl::PixelStats;

    fn video_stats() -> PixelStats {
        PixelStats {
            mean_luma: 100.0,
            variance: 900.0,
            non_black_ratio: 0.9,
        }
    }

    fn capture() -> Capture {
        let mut capture = Capture::new(Options::new(PathBuf::from("/tmp/x.png"), 5.0));
        capture.note_first_frame(Duration::from_secs(10));
        capture
    }

    #[test]
    fn happy_path_passes() {
        let capture = capture();
        let outcome = capture.finish(
            Duration::from_secs(15),
            300,
            0,
            "vaapi".into(),
            Some(Duration::from_millis(700)),
            (1280, 800),
            Some(std::time::Duration::from_secs(60)),
            &video_stats(),
        );
        assert!(outcome.passed(), "{:?}", outcome.failures);
        assert!((outcome.advance_s - 5.0).abs() < 1e-6);
    }

    #[test]
    fn every_metric_can_fail() {
        let capture = capture();
        let outcome = capture.finish(
            Duration::from_millis(11_000),
            30,
            40,
            "no".into(),
            Some(Duration::from_secs(2)),
            (1280, 800),
            None,
            &PixelStats::default(),
        );
        assert!(!outcome.passed());
        let text = outcome.failure_text();
        for expected in ["首帧", "位置", "hwdec", "全黑", "丢帧"] {
            assert!(text.contains(expected), "缺少 {expected}：{text}");
        }
        assert_eq!(outcome.first_frame_s, 2.0);
        assert_eq!(outcome.frames, 30);
    }

    #[test]
    fn seconds_must_be_positive() {
        assert_eq!(
            Options::new(PathBuf::from("x"), -1.0).seconds,
            5.0,
            "非法秒数回落到 5 秒"
        );
        assert_eq!(Options::new(PathBuf::from("x"), 2.5).seconds, 2.5);
    }
}
