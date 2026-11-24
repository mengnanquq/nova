// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `probe` 子命令：打印容器/轨道信息、实际 `hwdec-current`、色彩参数与 HDR 元数据。
//!
//! 所有数值都来自 mpv 的属性树（`mpv_get_property_string`），不解析 ffprobe 输出。
//! `hwdec-current` 只有在真正解出帧之后才有意义，所以这里先渲染一帧再读。

use std::path::Path;
use std::time::Duration;

use crate::player::{Session, SessionOptions};
use crate::sys::SurfaceFormat;

/// `video-params/*` 里与色彩/HDR 相关的子属性。
const COLOR_PROPS: &[&str] = &[
    "video-params/pixelformat",
    "video-params/hw-pixelformat",
    "video-params/w",
    "video-params/h",
    "video-params/dw",
    "video-params/dh",
    "video-params/aspect",
    "video-params/par",
    "video-params/colormatrix",
    "video-params/colorlevels",
    "video-params/primaries",
    "video-params/gamma",
    "video-params/chroma-location",
    "video-params/rotate",
    "video-params/stereo-in",
    // HDR / 亮度元数据（SDR 源上通常是 sig-peak=1.0，其余为空）
    "video-params/sig-peak",
    "video-params/light",
    "video-params/min-luma",
    "video-params/max-luma",
    "video-params/max-cll",
    "video-params/max-fall",
    "video-params/scene-max-r",
    "video-params/scene-max-g",
    "video-params/scene-max-b",
    "video-params/scene-avg-r",
    "video-params/scene-avg-g",
    "video-params/scene-avg-b",
];

/// 容器/流级别的标量属性。
const CONTAINER_PROPS: &[&str] = &[
    "file-format",
    "duration",
    "container-fps",
    "estimated-vf-fps",
];

/// 视频解码器/VO 级别的标量属性。
const VIDEO_PROPS: &[&str] = &[
    "video-codec",
    "video-format",
    "video-bitrate",
    "width",
    "height",
    "dwidth",
    "dheight",
    "hwdec-current",
    "hwdec-image-format",
    "video-rotate",
    "video-crop",
];

const AUDIO_PROPS: &[&str] = &[
    "audio-codec",
    "audio-codec-name",
    "audio-params/channel-count",
    "audio-params/samplerate",
];

fn show(value: Option<String>) -> String {
    match value {
        Some(v) if !v.is_empty() => v,
        _ => "-".to_string(),
    }
}

fn print_pairs(title: &str, player: &crate::sys::Handle, keys: &[&str]) {
    println!("{title}");
    for key in keys {
        println!("  {:<32} {}", key, show(player.get_string(key)));
    }
}

/// `probe` 结果里给测试/报告复用的部分。
pub struct ProbeInfo {
    pub width: u32,
    pub height: u32,
    pub hwdec_current: Option<String>,
    pub primaries: Option<String>,
    pub gamma: Option<String>,
    pub pixelformat: Option<String>,
    /// 依据 primaries/gamma 判断源是否为 HDR（PQ/HLG 或 BT.2020）。
    pub hdr: bool,
    pub track_count: Option<i64>,
}

pub fn run(file: &Path, session_opts: &SessionOptions<'_>) -> Result<ProbeInfo, String> {
    let session = Session::open(file, session_opts)?;
    session.wait_loaded(Duration::from_secs(20))?;
    // 渲染一帧，让解码器/硬件解码真正初始化；否则 hwdec-current 还是空的。
    let img = session.first_frame(SurfaceFormat::Rgb0, Duration::from_secs(20))?;
    session.drain_events()?;

    let player = session.player();
    println!("文件: {}", file.display());
    println!();
    print_pairs("【容器/流】", player, CONTAINER_PROPS);
    println!(
        "  {:<32} {}x{}",
        "decoded-frame (w x h)",
        img.width(),
        img.height()
    );

    let track_count = player.get_i64("track-list/count").unwrap_or(0);
    println!();
    println!("【轨道】共 {track_count} 条");
    for i in 0..track_count {
        let get = |suffix: &str| {
            player
                .get_string(&format!("track-list/{i}/{suffix}"))
                .filter(|v| !v.is_empty())
        };
        let kind = get("type").unwrap_or_else(|| "?".to_string());
        let mut line = format!(
            "  [{i}] {:<9} codec={:<24}",
            kind,
            get("codec").unwrap_or_else(|| "-".to_string())
        );
        if let Some(v) = get("demux-w") {
            line.push_str(&format!(" {v}x"));
        }
        if let Some(v) = get("demux-h") {
            line.push_str(&v);
        }
        if let Some(v) = get("demux-fps") {
            line.push_str(&format!(" @{v}fps"));
        }
        if let Some(v) = get("demux-channel-count") {
            line.push_str(&format!(" ch={v}"));
        }
        if let Some(v) = get("demux-samplerate") {
            line.push_str(&format!(" sr={v}"));
        }
        for (key, label) in [("lang", "lang"), ("title", "title")] {
            if let Some(v) = get(key) {
                line.push_str(&format!(" {label}={v}"));
            }
        }
        println!("{line}");
    }

    println!();
    print_pairs("【视频】", player, VIDEO_PROPS);
    print_pairs("【音频】", player, AUDIO_PROPS);
    println!();
    print_pairs("【色彩/HDR（video-params）】", player, COLOR_PROPS);

    let primaries = player.get_string("video-params/primaries");
    let gamma = player.get_string("video-params/gamma");
    let pixelformat = player.get_string("video-params/pixelformat");
    let hdr = is_hdr(primaries.as_deref(), gamma.as_deref());
    let max_cll = player.get_string("video-params/max-cll");
    let max_fall = player.get_string("video-params/max-fall");
    println!();
    println!("【结论】");
    println!(
        "  源色彩: primaries={} gamma={} pixelformat={}",
        show(primaries.clone()),
        show(gamma.clone()),
        show(pixelformat.clone())
    );
    println!("  判定为 HDR: {}", if hdr { "是" } else { "否" });
    println!(
        "  HDR10 元数据: max-cll={} max-fall={}（来自流的 mastering/content-light 信息）",
        show(max_cll),
        show(max_fall)
    );
    println!(
        "  实际解码路径: hwdec-current={}  hwdec-image-format={}",
        show(session.hwdec_current()),
        show(player.get_string("hwdec-image-format"))
    );

    Ok(ProbeInfo {
        width: img.width(),
        height: img.height(),
        hwdec_current: session.hwdec_current(),
        primaries,
        gamma,
        pixelformat,
        hdr,
        track_count: Some(track_count),
    })
}

/// PQ / HLG 传输函数，或 BT.2020 原色，都视为 HDR 源。
fn is_hdr(primaries: Option<&str>, gamma: Option<&str>) -> bool {
    let gamma = gamma.unwrap_or("").to_ascii_lowercase();
    let primaries = primaries.unwrap_or("").to_ascii_lowercase();
    gamma.contains("pq") || gamma.contains("hlg") || primaries.contains("2020")
}
