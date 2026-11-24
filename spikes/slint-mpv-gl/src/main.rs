// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 命令行入口：`slint-mpv-gl <file> [选项]`。
//!
//! 一次运行 = 打开一个 960x540 的 Slint 窗口，用 libmpv 的 OpenGL render API 把视频
//! 渲染进我们自己的 FBO/纹理，交给 Slint 合成显示，`--seconds` 后自动退出并打印报告。

use std::path::PathBuf;

use slint_mpv_gl::app::{Options, Report, run};
use slint_mpv_gl::parse_size;

const USAGE: &str = "\
slint-mpv-gl —— Slint 窗口 + libmpv OpenGL render API 零拷贝原型（Gate 1.5-A）

用法:
  slint-mpv-gl <file> [选项]

选项:
  --seconds <n>        运行多少秒后自动退出（默认 8；播到结尾会提前退出）
  --hwdec <mode>       mpv 的 hwdec 值：vaapi | no | auto | …（默认 vaapi）
  --size <WxH>         窗口逻辑尺寸（默认 960x540）
  --frames-out <dir>   把回读到的窗口帧写成 PNG（每次运行最多 6 张）
  -v, --verbose        打开 mpv 自己的日志
  -h, --help           显示本帮助
";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") || argv.is_empty() {
        print!("{USAGE}");
        if argv.is_empty() {
            std::process::exit(2);
        }
        return;
    }
    let opts = match parse_args(&argv) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("参数错误: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    match run(opts) {
        Ok(report) => {
            print_report(&report);
            if !report.pixels_ok() {
                eprintln!(
                    "FAIL: 回读到的窗口像素不像视频画面（全黑/过于平坦），详见上面的 readback 行"
                );
                std::process::exit(3);
            }
        }
        Err(err) => {
            eprintln!("错误: {err}");
            std::process::exit(1);
        }
    }
}

fn parse_args(argv: &[String]) -> Result<Options, String> {
    let mut file: Option<PathBuf> = None;
    let mut seconds = 8.0f64;
    let mut hwdec = "vaapi".to_string();
    let mut size = (960u32, 540u32);
    let mut frames_out: Option<PathBuf> = None;
    let mut verbose = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            argv.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} 缺少取值"))
        };
        match arg {
            "--seconds" => {
                let v = value("--seconds")?;
                seconds = v
                    .parse::<f64>()
                    .map_err(|e| format!("--seconds 取值 `{v}` 非法: {e}"))?;
            }
            "--hwdec" => hwdec = value("--hwdec")?,
            "--size" => {
                let v = value("--size")?;
                size = parse_size(&v)?;
            }
            "--frames-out" => frames_out = Some(PathBuf::from(value("--frames-out")?)),
            "-v" | "--verbose" => verbose = true,
            other if other.starts_with("--") => {
                return Err(format!("未知参数 `{other}`"));
            }
            other => {
                if file.is_some() {
                    return Err(format!("多余的参数 `{other}`（只接受一个文件）"));
                }
                file = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }

    let file = file.ok_or("缺少视频文件路径")?;
    if !file.exists() {
        return Err(format!("文件不存在: {}", file.display()));
    }
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err("--seconds 必须是正数".to_string());
    }
    Ok(Options {
        file,
        seconds,
        hwdec,
        size,
        frames_out,
        verbose,
    })
}

fn print_report(report: &Report) {
    println!("==================================================================");
    println!("文件          : {}", report.file.display());
    println!(
        "hwdec         : 请求 `{}` → 实际 `{}`",
        report.hwdec_requested,
        report.hwdec_current.as_deref().unwrap_or("（未读到）")
    );
    println!(
        "pixelformat   : {}",
        report.pixelformat.as_deref().unwrap_or("（未读到）")
    );
    match report.video_size {
        Some((w, h)) => println!("video-params  : {w}x{h}（纹理尺寸 = 视频尺寸）"),
        None => println!("video-params  : （未读到）"),
    }
    println!(
        "窗口物理尺寸  : {}x{}",
        report.window_size.0, report.window_size.1
    );
    if let Some(gl) = &report.gl {
        println!(
            "GL            : renderer=`{}`  vendor=`{}`  version=`{}`{}",
            gl.renderer,
            gl.vendor,
            gl.version,
            if gl.is_embedded { " (OpenGL ES)" } else { "" }
        );
        println!("GLSL          : {}", gl.shading_language_version);
    } else {
        println!("GL            : （未初始化）");
    }
    println!(
        "帧            : 渲染 {} 帧，计入统计的时长 {:.2}s → 端到端 {:.1} fps",
        report.frames, report.duration_s, report.e2e_fps
    );
    println!(
        "mpv_render_context_render : p50 {:.0}us  p95 {:.0}us  均值 {:.0}us",
        report.render_p50_us, report.render_p95_us, report.render_mean_us
    );
    println!(
        "丢帧          : frame-drop-count={:?}  decoder-frame-drop-count={:?}",
        report.frame_drop, report.decoder_frame_drop
    );
    println!(
        "CPU           : {:.2}s / 墙钟 {:.2}s = 平均 {:.2} 个核",
        report.cpu_seconds,
        report.wall_seconds,
        report.cpu_cores()
    );
    println!(
        "渲染驱动      : {}（window=合成器帧回调 + 读窗口 back buffer；snapshot=take_snapshot 离屏渲染）",
        report.render_mode
    );
    println!(
        "像素回读      : {} 次，p50 {:.0}us / 次",
        report.readbacks.len(),
        report.readback_p50_us
    );
    println!(
        "GL 符号       : 经 Slint get_proc_address 解析 {} 个，兜底 {:.0}%",
        report.loader_symbols,
        if report.loader_symbols == 0 {
            0.0
        } else {
            100.0 * report.loader_fallbacks as f64 / report.loader_symbols as f64
        }
    );
    for rb in &report.readbacks {
        println!(
            "  - 第 {:3} 帧  luma 均值 {:6.1}  方差 {:9.0}  非黑占比 {:5.1}%  rgb=({:.0},{:.0},{:.0})  {}",
            rb.frame,
            rb.stats.mean_luma,
            rb.stats.variance,
            rb.stats.non_black_ratio * 100.0,
            rb.stats.mean_rgb[0],
            rb.stats.mean_rgb[1],
            rb.stats.mean_rgb[2],
            rb.path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
    }
    let target = match report.render_mode {
        "window" => "窗口 back buffer",
        _ => "Slint 离屏 FBO（take_snapshot 路径，等价的一次完整渲染）",
    };
    println!(
        "判定          : {}",
        if report.pixels_ok() {
            format!("PASS —— {target} 里确实有非黑的视频画面")
        } else {
            format!("FAIL —— 从 {target} 回读到的像素不像视频画面")
        }
    );
    // 机器可读的一行，方便把多次运行拼成对比表。
    println!(
        "RESULT hwdec={} hwdec_current={} pixfmt={} video={}x{} window={}x{} gl_renderer=\"{}\" \
frames={} duration_s={:.3} e2e_fps={:.1} render_p50_us={:.1} render_p95_us={:.1} \
readback_p50_us={:.1} frame_drop={} decoder_frame_drop={} luma={:.1} variance={:.0} \
nonblack={:.3} loader_fallbacks={} loader_symbols={} pass={} render_mode={} cpu_seconds={:.2} \
wall_seconds={:.2} cpu_cores={:.2}",
        report.hwdec_requested,
        report.hwdec_current.as_deref().unwrap_or("-"),
        report.pixelformat.as_deref().unwrap_or("-"),
        report.video_size.map(|s| s.0).unwrap_or(0),
        report.video_size.map(|s| s.1).unwrap_or(0),
        report.window_size.0,
        report.window_size.1,
        report
            .gl
            .as_ref()
            .map(|g| g.renderer.as_str())
            .unwrap_or("-"),
        report.frames,
        report.duration_s,
        report.e2e_fps,
        report.render_p50_us,
        report.render_p95_us,
        report.readback_p50_us,
        report
            .frame_drop
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into()),
        report
            .decoder_frame_drop
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into()),
        report
            .readbacks
            .last()
            .map(|r| r.stats.mean_luma)
            .unwrap_or(f64::NAN),
        report
            .readbacks
            .last()
            .map(|r| r.stats.variance)
            .unwrap_or(f64::NAN),
        report
            .readbacks
            .last()
            .map(|r| r.stats.non_black_ratio)
            .unwrap_or(f64::NAN),
        report.loader_fallbacks,
        report.loader_symbols,
        report.pixels_ok(),
        report.render_mode,
        report.cpu_seconds,
        report.wall_seconds,
        report.cpu_cores(),
    );
    println!("==================================================================");
}
