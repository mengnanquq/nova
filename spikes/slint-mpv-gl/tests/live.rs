// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 需要真实显示会话 + GPU 的端到端测试，默认 `#[ignore]`。
//!
//! 每个用例都会**另起一个进程**跑 `slint-mpv-gl` 二进制：winit 的事件循环在一个进程里
//! 只能建一次（同一进程里建第二个窗口会报 "EventLoop can't be recreated"），
//! 所以不能在一个测试进程里连开两个窗口。
//!
//! 手动运行（需要有可用的 Wayland 会话）：
//!
//! ```text
//! WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
//!   cargo test -p slint-mpv-gl --test live -- --ignored --nocapture
//! ```
//!
//! 注意：KDE 锁屏时合成器不再发帧回调，程序会自动退到 take_snapshot() 驱动渲染，
//! 仍然能跑完并输出 PASS（`render_mode=snapshot`）。

use std::path::PathBuf;
use std::process::Command;

fn testdata(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/spikes/slint-mpv-gl
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // spikes
    path.pop(); // 仓库根
    path.push("target/testdata");
    path.push(name);
    path
}

/// 跑一次二进制，返回它打印的 `RESULT ...` 那一行。
fn run_once(hwdec: &str, file: &str) -> String {
    let path = testdata(file);
    assert!(
        path.exists(),
        "缺少测试素材 {}（需要先生成到 target/testdata/）",
        path.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_slint-mpv-gl"))
        .arg(&path)
        .args(["--seconds", "4", "--hwdec", hwdec])
        .output()
        .expect("启动 slint-mpv-gl 失败");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    eprintln!("--- {hwdec} stderr ---\n{stderr}");
    assert!(
        output.status.success(),
        "{hwdec} 运行失败（退出码 {:?}）",
        output.status.code()
    );
    stdout
        .lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("没有 RESULT 行，stdout:\n{stdout}"))
        .to_string()
}

fn field<'a>(result: &'a str, key: &str) -> &'a str {
    result
        .split_whitespace()
        .find_map(|token| token.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("RESULT 里没有 {key}: {result}"))
}

/// 硬解零拷贝：窗口里必须出现真正的视频画面，并且 hwdec 生效。
#[test]
#[ignore = "需要真实窗口 + GPU，手动运行"]
fn vaapi_zero_copy_renders_real_video() {
    let result = run_once("vaapi", "hevc10_1080p_3s.mp4");
    eprintln!("{result}");
    assert_eq!(field(&result, "pass"), "true", "像素验证没通过");
    assert_eq!(field(&result, "hwdec_current"), "vaapi", "没有被硬解走");
    assert_eq!(field(&result, "pixfmt"), "vaapi", "解码输出不是 vaapi 表面");
    let frames: usize = field(&result, "frames").parse().unwrap();
    assert!(frames > 10, "渲染帧数太少: {frames}");
    assert!(
        field(&result, "variance").parse::<f64>().unwrap() > 100.0,
        "画面方差太小，可能是黑屏/纯色"
    );
}

/// 软解对照：同样必须出画面。
#[test]
#[ignore = "需要真实窗口 + GPU，手动运行"]
fn software_decode_renders_real_video() {
    let result = run_once("no", "hevc10_1080p_3s.mp4");
    eprintln!("{result}");
    assert_eq!(field(&result, "pass"), "true", "像素验证没通过");
    assert_eq!(field(&result, "hwdec_current"), "no");
    assert_ne!(field(&result, "pixfmt"), "vaapi");
}

/// 4K HEVC 10bit：纹理尺寸必须跟着视频尺寸走（重建 FBO）。
#[test]
#[ignore = "需要真实窗口 + GPU，手动运行"]
fn uhd_zero_copy_renders_real_video() {
    let result = run_once("vaapi", "hevc10_4k_2s.mp4");
    eprintln!("{result}");
    assert_eq!(field(&result, "pass"), "true");
    assert_eq!(field(&result, "video"), "3840x2160");
    assert!(field(&result, "frames").parse::<usize>().unwrap() > 10);
}
