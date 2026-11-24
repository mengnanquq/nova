// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 端到端测试：用 ffmpeg 生成素材，然后真实解码 / 导出 / 走 Slint 图像路径。
//!
//! 素材放在 `<workspace>/target/testdata/`。没有 ffmpeg 时打印跳过而不是失败
//! （README 第 5 节的要求）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use playback_proto::gst_source::GstSource;
use playback_proto::{FrameSource, VideoFrame, dump};

const MEDIA_NAME: &str = "testsrc_320x240_2s.mp4";

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/testdata")
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 生成一次素材，整个测试进程共用。
fn media() -> Option<&'static Path> {
    static MEDIA: OnceLock<Option<PathBuf>> = OnceLock::new();
    MEDIA
        .get_or_init(|| {
            let dir = testdata_dir();
            let path = dir.join(MEDIA_NAME);
            if path.exists() {
                return Some(path);
            }
            if !ffmpeg_available() {
                eprintln!("跳过：本机没有 ffmpeg，无法生成 {MEDIA_NAME}");
                return None;
            }
            std::fs::create_dir_all(&dir).expect("创建 target/testdata 失败");
            let status = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x240:rate=30:duration=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000:duration=2",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-g",
                    "15",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "64k",
                    "-shortest",
                ])
                .arg(&path)
                .status()
                .expect("启动 ffmpeg 失败");
            assert!(status.success(), "ffmpeg 生成测试素材失败");
            assert!(path.exists(), "ffmpeg 没产出 {MEDIA_NAME}");
            Some(path)
        })
        .as_deref()
}

fn open_source(media: &Path) -> GstSource {
    GstSource::open(media.to_str().expect("测试素材路径不是 UTF-8")).expect("打开测试素材失败")
}

fn first_frame(media: &Path) -> VideoFrame {
    let mut source = open_source(media);
    source
        .next_frame()
        .expect("解码失败")
        .expect("第一帧不存在")
}

#[test]
fn description_reports_actual_decoder() {
    let Some(media) = media() else {
        return;
    };
    let source = open_source(media);
    let description = source.description();

    assert_eq!(description.backend, "gstreamer");
    let decoder = description
        .video_decoder
        .as_ref()
        .expect("没有观测到视频解码器");
    assert!(!decoder.element.factory.is_empty());
    // 判定必须与元素本身一致，而不是写死的预期值。
    let in_elements = description
        .elements
        .iter()
        .any(|element| element.name == decoder.element.name);
    assert!(in_elements, "解码器必须出现在实际元素链里");
    assert!(decoder.in_caps.is_some(), "解码器输入 caps 应该可读");
    assert!(decoder.out_caps.is_some(), "解码器输出 caps 应该可读");

    let sink_caps = description.sink_caps.as_deref().unwrap_or_default();
    assert!(
        sink_caps.contains("NV12"),
        "appsink 应该协商到 NV12，实际: {sink_caps}"
    );
    let video = description.video.as_ref().expect("没有视频流信息");
    assert_eq!((video.width, video.height), (320, 240));
}

#[test]
fn decodes_frames_with_expected_geometry_and_pts() {
    let Some(media) = media() else {
        return;
    };
    let mut source = open_source(media);
    let mut count = 0usize;
    let mut pts_ns = Vec::new();
    while let Some(frame) = source.next_frame().expect("解码失败") {
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 240);
        assert_eq!(frame.stride, 320 * VideoFrame::BYTES_PER_PIXEL);
        assert_eq!(
            frame.data.len(),
            frame.stride * frame.height as usize,
            "data.len() 必须等于 stride * height"
        );
        assert!(frame.row(0).is_some() && frame.row(frame.height - 1).is_some());
        assert!(frame.row(frame.height).is_none(), "越界行必须返回 None");
        if let Some(pts) = frame.pts_ns {
            pts_ns.push(pts);
        }
        count += 1;
        if count == 30 {
            break;
        }
    }
    assert!(count >= 30, "只解出 {count} 帧");
    assert!(pts_ns.len() >= 30, "只有 {} 帧带 pts", pts_ns.len());
    assert!(
        pts_ns.windows(2).all(|pair| pair[0] < pair[1]),
        "pts 必须严格递增: {:?}",
        &pts_ns[..pts_ns.len().min(8)]
    );
}

#[test]
fn decoding_is_deterministic() {
    let Some(media) = media() else {
        return;
    };
    let first = first_frame(media);
    let second = first_frame(media);
    assert_eq!(first.width, second.width);
    assert_eq!(first.height, second.height);
    assert_eq!(first.stride, second.stride);
    assert_eq!(first.pts_ns, second.pts_ns);
    assert_eq!(first.data, second.data, "两次解码的第 1 帧必须像素一致");
}

#[test]
fn dump_writes_valid_png_files() {
    let Some(media) = media() else {
        return;
    };
    let out_dir = testdata_dir().join("dump-test");
    let mut source = open_source(media);
    let files = dump::dump_frames(&mut source, 3, &out_dir).expect("导出 PNG 失败");
    assert_eq!(files.len(), 3, "应该导出 3 张 PNG");
    for path in files {
        let bytes = std::fs::read(&path).expect("读回 PNG 失败");
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "{} 不是合法 PNG 文件头",
            path.display()
        );
        assert!(
            bytes.len() > 1024,
            "{} 只有 {} 字节",
            path.display(),
            bytes.len()
        );
        assert_eq!(
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            320,
            "PNG 宽度不正确"
        );
        assert_eq!(
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            240,
            "PNG 高度不正确"
        );
    }
}

#[cfg(feature = "slint")]
#[test]
fn slint_image_holds_the_same_pixels() {
    let Some(media) = media() else {
        return;
    };
    let frame = first_frame(media);
    let image =
        playback_proto::slint_bridge::frame_to_image(&frame).expect("构造 Slint Image 失败");
    let size = image.size();
    assert_eq!(size.width as u32, frame.width);
    assert_eq!(size.height as u32, frame.height);
    let back = image.to_rgba8().expect("Image::to_rgba8 返回 None");
    assert_eq!(back.as_bytes(), frame.data.as_slice(), "像素必须原样保留");
}

#[cfg(feature = "slint")]
#[test]
fn slint_image_path_does_not_need_a_backend() {
    // 这个断言把"无 backend 不 panic"钉成事实；FINDINGS.md 记录了实测结论。
    let steps = playback_proto::slint_bridge::probe_no_backend();
    let panicked: Vec<&str> = steps
        .iter()
        .filter(|step| step.outcome.is_panic())
        .map(|step| step.name)
        .collect();
    assert!(
        panicked.is_empty(),
        "以下操作在没有 backend 时 panic 了: {panicked:?}"
    );
}

/// 单独量 Slint 图像路径（不起管线、不占解码线程）。
/// 手动跑：`cargo test -p playback-proto --test proto -- --ignored --nocapture slint_path_isolated`
#[cfg(feature = "slint")]
#[test]
#[ignore = "手动跑的隔离测量，输出数字用，不参与 cargo test"]
fn slint_path_isolated() {
    for (width, height) in [(1920u32, 1080u32), (3840, 2160)] {
        let bench =
            playback_proto::slint_bridge::bench_path(width, height, 60).expect("bench_path 失败");
        println!(
            "{width}x{height} p50/p95 ms: alloc {:.3}/{:.3} copy {:.3}/{:.3} new+copy {:.3}/{:.3} clone_from_slice {:.3}/{:.3} memcpy(reuse) {:.3}/{:.3}",
            bench.alloc.p50_ms(),
            bench.alloc.p95_ms(),
            bench.copy.p50_ms(),
            bench.copy.p95_ms(),
            bench.total.p50_ms(),
            bench.total.p95_ms(),
            bench.single_copy_total.p50_ms(),
            bench.single_copy_total.p95_ms(),
            bench.memcpy_reused.p50_ms(),
            bench.memcpy_reused.p95_ms(),
        );
    }
}

#[cfg(not(feature = "slint"))]
#[test]
fn slint_feature_disabled_still_decodes() {
    let Some(media) = media() else {
        return;
    };
    let frame = first_frame(media);
    assert_eq!(frame.data.len(), 320 * 240 * 4);
}
