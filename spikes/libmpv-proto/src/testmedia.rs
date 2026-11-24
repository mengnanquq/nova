// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 测试素材生成（ffmpeg lavfi），供集成测试与人工复现使用。
//!
//! 若本机没有 ffmpeg，调用方应当**跳过**依赖素材的断言，而不是失败。

use std::path::{Path, PathBuf};
use std::process::Command;

/// ffmpeg 是否可用。
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 每个测试进程独立的临时目录（多次运行会复用并覆盖其中的文件）。
pub fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("libmpv-proto-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

/// 用 ffmpeg 生成一段 h264/aac mp4：`testsrc2` 视频 + 正弦音频。
pub fn make_testsrc(dir: &Path) -> PathBuf {
    make_mp4(
        dir,
        "testsrc2.mp4",
        "testsrc2=size=320x240:rate=30:duration=1",
        "sine=frequency=440:duration=1",
    )
}

/// 纯红 1 秒视频（用来断言 SW 渲染出来的字节序）。
pub fn make_solid_red(dir: &Path) -> PathBuf {
    // 不带音频；10fps 足够，解码成本可忽略。
    make_mp4(
        dir,
        "red.mp4",
        "color=c=red:size=64x48:rate=10:duration=1",
        "",
    )
}

/// 纯红、但宽度需要为 stride 补 padding 的视频（1300*4=5200 → 5248）。
pub fn make_solid_red_padded(dir: &Path) -> PathBuf {
    make_mp4(
        dir,
        "red_padded.mp4",
        "color=c=red:size=1300x48:rate=10:duration=1",
        "",
    )
}

fn make_mp4(dir: &Path, name: &str, video: &str, audio: &str) -> PathBuf {
    let out = dir.join(name);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", video]);
    if !audio.is_empty() {
        cmd.args(["-f", "lavfi", "-i", audio]);
    }
    cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    if !audio.is_empty() {
        cmd.args(["-c:a", "aac", "-shortest"]);
    }
    cmd.arg(&out);
    let status = cmd.status().expect("执行 ffmpeg");
    assert!(status.success(), "ffmpeg 生成 {:?} 失败", out);
    assert!(out.exists(), "ffmpeg 没有产出 {:?}", out);
    out
}
