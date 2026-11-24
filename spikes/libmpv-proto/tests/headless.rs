// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 集成测试：全部自包含（用 ffmpeg 生成素材），机器上没有 ffmpeg 时跳过。
//!
//! 这些测试同时是“SW 渲染真的能 headless 跑”的证据：进程里没有 DISPLAY/WAYLAND，
//! 也不会打开任何窗口。
//!
//! 同一个进程里同时跑多个 mpv core 是允许的（每个 core 一个 render context），
//! 但为了让计时与日志可读，这里用一把全局锁串行化。

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use libmpv_proto::sys::AlignedImage;
use libmpv_proto::{SurfaceFormat, dump, player::Session, player::SessionOptions, testmedia};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn open(path: &std::path::Path) -> Session {
    let opts = SessionOptions {
        hwdec: "auto-safe",
        extra: &[],
    };
    let session = Session::open(path, &opts).expect("打开会话");
    session
        .wait_loaded(Duration::from_secs(20))
        .expect("加载文件");
    session
}

/// 渲染 `n` 帧，返回每帧 mpv 报告的尺寸，便于断言“跨帧尺寸恒定”。
fn render_frames(
    session: &Session,
    format: SurfaceFormat,
    n: usize,
) -> (AlignedImage, Vec<(u32, u32)>) {
    let mut img = session
        .first_frame(format, Duration::from_secs(20))
        .expect("首帧");
    let mut sizes = vec![(img.width(), img.height())];
    let renderer = session.renderer();
    while sizes.len() < n {
        if libmpv_proto::sys::SwRenderer::has_new_frame(renderer.update()) {
            renderer.render_into(format, &mut img).expect("渲染");
            sizes.push((img.width(), img.height()));
        } else {
            renderer.signal().wait(Duration::from_millis(20));
        }
    }
    (img, sizes)
}

fn media(tag: &str, build: fn(&std::path::Path) -> PathBuf) -> Option<PathBuf> {
    testmedia::ffmpeg_available().then(|| build(&testmedia::workdir(tag)))
}

#[test]
fn sw_render_produces_stable_frame_geometry() {
    let Some(clip) = media("geometry", testmedia::make_testsrc) else {
        eprintln!("跳过：本机没有 ffmpeg");
        return;
    };
    let _guard = serial();
    let session = open(&clip);
    let (img, sizes) = render_frames(&session, SurfaceFormat::Rgb0, 5);

    assert_eq!(sizes.len(), 5);
    // 素材是 320x240；SW 渲染目标就是解码后的尺寸（不做 letterbox 缩放）。
    assert_eq!(sizes[0], (320, 240));
    assert!(
        sizes.iter().all(|s| *s == sizes[0]),
        "跨帧尺寸必须恒定，实际 {sizes:?}"
    );
    assert_eq!((img.width(), img.height()), (320, 240));
    // rgba 4 字节/像素，stride 按 64 对齐（320*4=1280，本身就是 64 的倍数）。
    assert_eq!(img.stride(), 1280);
    assert!(img.is_aligned(), "缓冲区应满足 64 字节对齐建议");
    // 帧内容不应是空白。
    let distinct = img
        .as_slice()
        .chunks_exact(4)
        .map(|p| (p[0], p[1], p[2]))
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct.len() > 8,
        "帧内容几乎单色，distinct={}",
        distinct.len()
    );
}

#[test]
fn pure_red_reveals_byte_order_of_both_formats() {
    let Some(clip) = media("red", testmedia::make_solid_red) else {
        eprintln!("跳过：本机没有 ffmpeg");
        return;
    };
    let _guard = serial();

    // rgb0：内存里前 3 字节是 R,G,B。素材是纯红，yuv420p 往返有少量误差，用宽松阈值。
    let session = open(&clip);
    let (img, _) = render_frames(&session, SurfaceFormat::Rgb0, 2);
    let px = img.row(0)[..4].to_vec();
    eprintln!("rgb0 首像素: {px:?}");
    assert!(px[0] > 200, "rgb0 第 1 字节应是 R，实际 {px:?}");
    assert!(px[1] < 60, "rgb0 第 2 字节应是 G，实际 {px:?}");
    assert!(px[2] < 60, "rgb0 第 3 字节应是 B，实际 {px:?}");
    // 第 4 字节（render.h 里那个 "0"）不保证是有效 alpha：记录实测行为，
    // 但要求它是“统一值”，而不是每像素乱写。
    let all_alpha: std::collections::HashSet<u8> =
        img.as_slice().chunks_exact(4).map(|p| p[3]).collect();
    eprintln!("rgb0 第 4 字节取值集合: {all_alpha:?}");
    assert_eq!(all_alpha.len(), 1, "第 4 字节应统一，实际 {all_alpha:?}");
    drop(session);

    // bgr0：内存里前 3 字节是 B,G,R——所以第 1 字节应当接近 0，第 3 字节接近 255。
    let session = open(&clip);
    let (img, _) = render_frames(&session, SurfaceFormat::Bgr0, 2);
    let px = img.row(0)[..4].to_vec();
    eprintln!("bgr0 首像素: {px:?}");
    assert!(px[0] < 60, "bgr0 第 1 字节应是 B，实际 {px:?}");
    assert!(px[1] < 60, "bgr0 第 2 字节应是 G，实际 {px:?}");
    assert!(px[2] > 200, "bgr0 第 3 字节应是 R，实际 {px:?}");
}

/// stride 带 padding（宽度*4 不是 64 的倍数）时，mpv 仍然按行跨度正确落盘，
/// 且第 0 行最后一个像素不能被写到 padding 里。
#[test]
fn padded_stride_keeps_pixels_in_place() {
    let Some(clip) = media("padded", testmedia::make_solid_red_padded) else {
        eprintln!("跳过：本机没有 ffmpeg");
        return;
    };
    let _guard = serial();
    let session = open(&clip);
    let (img, _) = render_frames(&session, SurfaceFormat::Rgb0, 2);

    assert_eq!((img.width(), img.height()), (1300, 48));
    assert_eq!(img.stride(), 5248, "stride 应向上对齐到 64");
    assert!(img.stride() > 1300 * 4, "本用例必须有行尾 padding");

    let first = img.row(0);
    let last_of_row = &first[(1300 - 1) * 4..];
    assert!(
        first[0] > 200 && first[1] < 60 && first[2] < 60,
        "行首像素应为红: {:?}",
        &first[..4]
    );
    assert!(
        last_of_row[0] > 200 && last_of_row[1] < 60,
        "行尾像素应为红: {last_of_row:?}"
    );
    // 下一行同样从行首开始是红（说明跨行没有错位）。
    let second = img.row(1);
    assert!(second[0] > 200 && second[1] < 60, "第二行行首应为红");
}

#[test]
fn dump_writes_valid_png_header() {
    let Some(clip) = media("png", testmedia::make_solid_red) else {
        eprintln!("跳过：本机没有 ffmpeg");
        return;
    };
    let _guard = serial();
    let session = open(&clip);
    let (img, _) = render_frames(&session, SurfaceFormat::Rgb0, 1);
    let frame = dump::to_rgba(&img, SurfaceFormat::Rgb0);
    assert_eq!((frame.width, frame.height), (64, 48));
    // to_rgba 强制 alpha=255。
    assert!(frame.data.chunks_exact(4).all(|p| p[3] == 255));

    let out = testmedia::workdir("png").join("one.png");
    dump::write_png(&out, &frame).expect("写 PNG");
    let bytes = std::fs::read(&out).expect("读 PNG");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG 签名");
    // IHDR：紧跟在签名后的 4 字节长度 + "IHDR"，随后是宽高（大端 u32）。
    assert_eq!(&bytes[12..16], b"IHDR");
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    assert_eq!((width, height), (64, 48));
    // 位深 8、颜色类型 6（RGBA）。
    assert_eq!(bytes[24], 8, "bit depth");
    assert_eq!(bytes[25], 6, "color type RGBA");
    // IEND：长度(4) + "IEND" + CRC(4)，正好是文件最后 12 字节。
    assert_eq!(
        &bytes[bytes.len() - 12..],
        b"\x00\x00\x00\x00IEND\xae\x42\x60\x82"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn probe_reports_software_decoding_in_this_environment() {
    let Some(clip) = media("probe", testmedia::make_testsrc) else {
        eprintln!("跳过：本机没有 ffmpeg");
        return;
    };
    let _guard = serial();
    let opts = SessionOptions {
        hwdec: "auto-safe",
        extra: &[],
    };
    let info = libmpv_proto::probe::run(&clip, &opts).expect("probe");
    assert_eq!((info.width, info.height), (320, 240));
    // h264 8bit 4:2:0 在这台机器上（无 vaapi/nvdec 插件）应回退软解；
    // 若哪天真的拿到硬解，这里不会误报失败，只是把实况打印出来。
    eprintln!("hwdec-current = {:?}", info.hwdec_current);
    eprintln!(
        "primaries={:?} gamma={:?} pixelformat={:?} hdr={}",
        info.primaries, info.gamma, info.pixelformat, info.hdr
    );
    // software 渲染要求帧能被映射到系统内存：hwdec 生效时是 nv12（vaapi-copy 下来的），
    // 纯软解时是 yuv420p。两者都正常，但不应该出现 vaapi/opaque 这类不可读的格式。
    let pf = info.pixelformat.clone().unwrap_or_default();
    assert!(
        pf == "yuv420p" || pf == "nv12",
        "期望系统内存里的 4:2:0 格式，实际 {pf:?}"
    );
    assert!(!info.hdr, "SDR 测试素材不应被判定为 HDR");
    assert_eq!(info.track_count, Some(2), "一个视频轨 + 一个音频轨");
}
