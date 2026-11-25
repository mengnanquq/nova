// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 测试共用：无头后端、假数据、程序化封面、PNG 落盘。

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use i_slint_core::graphics::{Rgba8Pixel, SharedPixelBuffer};
use nova_ui::{
    App, CardView, DetailView, HomeView, LibraryRowView, LibraryTab, LibraryView, PlayerView,
    SearchView, SourceOption, TrackItem,
};

/// 初始化 testing backend，内嵌软件光栅器（真实字体，可 `take_snapshot()`）。
pub fn init_backend() {
    let backend = i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
        },
    );
    let _ = slint::platform::set_platform(Box::new(backend));
}

/// 快照目录：workspace 的 `target/snapshots/`。
pub fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots")
}

/// 把像素缓冲写成 PNG。
pub fn write_png(path: &Path, buffer: &SharedPixelBuffer<Rgba8Pixel>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let file = std::fs::File::create(path).unwrap();
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, buffer.width(), buffer.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    let mut bytes = Vec::with_capacity(buffer.as_slice().len() * 4);
    for pixel in buffer.as_slice() {
        bytes.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
    }
    writer.write_image_data(&bytes).unwrap();
}

fn hash(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    })
}

const PALETTE: [[u8; 3]; 6] = [
    [46, 58, 76],
    [92, 58, 48],
    [38, 72, 66],
    [78, 70, 34],
    [58, 44, 78],
    [70, 42, 54],
];

/// 程序化竖版海报：纯色块 + 圆形 + 条纹，用来观察占位/排版而不是美术。
pub fn poster(seed: &str) -> slint::Image {
    let width = 400u32;
    let height = 600u32;
    let seed = hash(seed);
    let palette = PALETTE[(seed as usize) % PALETTE.len()];
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let pixels = buffer.make_mut_slice();
    for y in 0..height {
        let fy = y as f32 / height as f32;
        for x in 0..width {
            let fx = x as f32 / width as f32;
            let brightness = 0.55 + 0.45 * fy;
            let mut rgb = [
                (f32::from(palette[0]) * brightness) as u8,
                (f32::from(palette[1]) * brightness) as u8,
                (f32::from(palette[2]) * brightness) as u8,
            ];
            let dx = fx - 0.5;
            let dy = fy - 0.34;
            let distance = (dx * dx + dy * dy * 1.4).sqrt();
            if distance < 0.22 {
                let glow = 1.0 - distance / 0.22;
                rgb[0] = (f32::from(rgb[0]) + 110.0 * glow).min(255.0) as u8;
                rgb[1] = (f32::from(rgb[1]) + 78.0 * glow).min(255.0) as u8;
                rgb[2] = (f32::from(rgb[2]) + 38.0 * glow).min(255.0) as u8;
            }
            let band = ((fx * 5.0 + (seed % 17) as f32 * 0.13).fract() * 10.0) as u32;
            if band == 0 && (0.15..0.8).contains(&fy) {
                rgb = [
                    rgb[0].saturating_add(22),
                    rgb[1].saturating_add(18),
                    rgb[2].saturating_add(12),
                ];
            }
            if (0.84..0.88).contains(&fy) && (0.16..0.84).contains(&fx) {
                rgb = [210, 206, 196];
            }
            if (0.9..0.93).contains(&fy) && (0.16..0.62).contains(&fx) {
                rgb = [142, 146, 152];
            }
            pixels[(y * width + x) as usize] = Rgba8Pixel {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 255,
            };
        }
    }
    slint::Image::from_rgba8(buffer)
}

/// 程序化横版背景图。
///
/// 尺寸与窗口（1280x800）一致：播放页用 `image-fit: contain` 铺底，1px 对 1px，
/// 所以假字幕带的位置就是真实像素位置。
pub fn backdrop(seed: &str) -> slint::Image {
    let width = 1280u32;
    let height = 800u32;
    let seed = hash(seed);
    let palette = PALETTE[(seed as usize) % PALETTE.len()];
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let pixels = buffer.make_mut_slice();
    for y in 0..height {
        let fy = y as f32 / height as f32;
        for x in 0..width {
            let fx = x as f32 / width as f32;
            let sky = (0.9 + 0.6 * (1.0 - fy)).clamp(0.0, 1.6);
            let mut rgb = [
                (f32::from(palette[0]) * sky + 46.0).min(255.0) as u8,
                (f32::from(palette[1]) * sky + 42.0).min(255.0) as u8,
                (f32::from(palette[2]) * sky + 40.0).min(255.0) as u8,
            ];
            let dx = fx - 0.72;
            let dy = fy - 0.3;
            if (dx * dx + dy * dy).sqrt() < 0.1 {
                rgb = [226, 196, 142];
            }
            if fy > 0.68 {
                let ridge = 0.68 + (fx * 9.0 + (seed % 5) as f32).sin() * 0.035;
                if fy > ridge {
                    rgb = [18, 20, 24];
                }
            }
            if (0.76..0.84).contains(&fx) && (0.55..0.62).contains(&fy) {
                rgb = [12, 13, 16];
            }
            // 假字幕：真实影片的字幕常驻在底部 12%（800px 高窗口 → y ≈ 730–780）。
            // 控制层可见时引擎会把字幕抬起（`sub-margin-y`），这里画的是未抬起的原位。
            let band = (SUBTITLE_BAND_TOP..SUBTITLE_BAND_TOP + 22).contains(&y)
                && (0.24..0.76).contains(&fx)
                || (SUBTITLE_BAND_TOP + 30..SUBTITLE_BAND_TOP + SUBTITLE_BAND_HEIGHT).contains(&y)
                    && (0.34..0.66).contains(&fx);
            if band {
                rgb = [236, 232, 224];
            }
            pixels[(y * width + x) as usize] = Rgba8Pixel {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 255,
            };
        }
    }
    slint::Image::from_rgba8(buffer)
}

/// 假字幕带的几何（真实影片字幕的常驻区：底部 12%）。
pub const SUBTITLE_BAND_TOP: u32 = 730;
/// 假字幕带高度（两行字幕）。
pub const SUBTITLE_BAND_HEIGHT: u32 = 50;

/// 亮色假帧：米白 / 天空色，用来验证控制层遮罩在亮画面上的文字可读性。
///
/// `subtitle_top` 是烧在画面里的字幕带顶边：控制层可见时引擎会把字幕抬起
/// （`sub-margin-y` ≈ 280px），快照里直接用抬升后的位置画，验证“字幕不再被传输行盖住”。
pub fn bright_frame(subtitle_top: u32) -> slint::Image {
    let width = 1280u32;
    let height = 800u32;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let pixels = buffer.make_mut_slice();
    for y in 0..height {
        let fy = y as f32 / height as f32;
        for x in 0..width {
            let fx = x as f32 / width as f32;
            // 天空→米白的竖向渐变：整体亮度 0.82~0.95，是最难压字的极端场景。
            let sky = 1.0 - fy;
            let mut rgb = [
                (238.0 - 26.0 * sky) as u8,
                (243.0 - 20.0 * sky) as u8,
                (247.0 - 12.0 * sky) as u8,
            ];
            if fy > 0.62 {
                // 下半部分是米白的沙地，更亮（最难读的白底白字区）。
                rgb = [246, 240, 228];
            }
            // 一个亮黄的光斑，模拟过曝的窗户。
            let dx = fx - 0.28;
            let dy = fy - 0.3;
            if (dx * dx + dy * dy).sqrt() < 0.12 {
                rgb = [253, 246, 214];
            }
            let in_band_top =
                (subtitle_top..subtitle_top + 22).contains(&y) && (0.22..0.78).contains(&fx);
            let in_band_bottom = (subtitle_top + 30..subtitle_top + SUBTITLE_BAND_HEIGHT)
                .contains(&y)
                && (0.32..0.68).contains(&fx);
            if in_band_top || in_band_bottom {
                // 字幕本身是白字 + 黑描边，用两行像素块模拟。
                rgb = [22, 22, 26];
            }
            pixels[(y * width + x) as usize] = Rgba8Pixel {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 255,
            };
        }
    }
    slint::Image::from_rgba8(buffer)
}

/// 顶部标签。
pub fn tabs() -> Vec<LibraryTab> {
    vec![
        LibraryTab::new("movies", "电影"),
        LibraryTab::new("shows", "剧集"),
        LibraryTab::new("anime", "动漫"),
        LibraryTab::new("docs", "纪录片"),
    ]
}

/// 填满数据的首页。
pub fn home_filled() -> HomeView {
    let hero = CardView::new("dune", "沙丘：第二部")
        .with_subtitle("2024 · 科幻 / 冒险 · 2 小时 46 分")
        .with_badge("4K");
    let continue_watching = vec![
        CardView::new("c1", "最后生还者")
            .with_subtitle("第 2 季 · 第 5 集")
            .with_progress(0.37)
            .with_badge("HDR"),
        CardView::new("c2", "三体")
            .with_subtitle("第 1 季 · 第 8 集")
            .with_progress(0.62)
            .with_badge("S01E08"),
        CardView::new("c3", "奥本海默")
            .with_subtitle("2023 · 传记")
            .with_progress(0.21),
        CardView::new("c4", "繁花")
            .with_subtitle("第 1 季 · 第 12 集")
            .with_progress(0.84),
        CardView::new("c5", "沙丘")
            .with_subtitle("2021 · 科幻")
            .with_progress(0.08),
    ];
    let recently_added = vec![
        CardView::new("r1", "银翼杀手 2049")
            .with_subtitle("2017 · 科幻")
            .with_badge("4K"),
        CardView::new("r2", "降临")
            .with_subtitle("2016 · 科幻")
            .with_badge("HDR"),
        CardView::new("r3", "边境杀手").with_subtitle("2015 · 犯罪"),
        CardView::new("r4", "燃烧").with_subtitle("2018 · 剧情"),
        CardView::new("r5", "花样年华")
            .with_subtitle("2000 · 爱情")
            .with_badge("4K"),
        CardView::new("r6", "寄生虫").with_subtitle("2019 · 剧情"),
        CardView::new("r7", "一一").with_subtitle("2000 · 家庭"),
        CardView::new("r8", "潜行者")
            .with_subtitle("1979 · 科幻")
            .with_badge("HDR"),
    ];
    // 各库前 20 条（16:9，标题 = 库名）。
    let library_rows = vec![
        LibraryRowView {
            title: "电影".to_owned(),
            items: vec![
                CardView::new("m1", "银翼杀手").with_subtitle("1982 · 科幻"),
                CardView::new("m2", "2001 太空漫游").with_subtitle("1968 · 科幻"),
                CardView::new("m3", "异形").with_subtitle("1979 · 恐怖"),
                CardView::new("m4", "银翼杀手 2049").with_subtitle("2017 · 科幻"),
            ],
        },
        LibraryRowView {
            title: "剧集".to_owned(),
            items: vec![
                CardView::new("s1", "黑镜").with_subtitle("2011 · 科幻"),
                CardView::new("s2", "西部世界").with_subtitle("2016 · 科幻"),
                CardView::new("s3", "切尔诺贝利").with_subtitle("2019 · 剧情"),
            ],
        },
    ];
    HomeView {
        hero: Some(hero),
        continue_watching,
        recently_added,
        library_rows,
        library_tabs: tabs(),
    }
}

/// 空首页。
pub fn home_empty() -> HomeView {
    HomeView::empty(tabs())
}

/// 媒体库。
pub fn library_view() -> LibraryView {
    let mut view = LibraryView::new("movies", "电影");
    view.sort_label = "排序：最近添加".to_string();
    view.library_tabs = tabs();
    view.current_tab = 0;
    view.cards = vec![
        CardView::new("l01", "沙丘：第二部")
            .with_subtitle("2024 · 科幻")
            .with_badge("4K"),
        CardView::new("l02", "奥本海默")
            .with_subtitle("2023 · 传记")
            .with_badge("HDR"),
        CardView::new("l03", "银翼杀手 2049").with_subtitle("2017 · 科幻"),
        CardView::new("l04", "降临").with_subtitle("2016 · 科幻"),
        CardView::new("l05", "边境杀手").with_subtitle("2015 · 犯罪"),
        CardView::new("l06", "燃烧").with_subtitle("2018 · 剧情"),
        CardView::new("l07", "花样年华").with_subtitle("2000 · 爱情"),
        CardView::new("l08", "寄生虫").with_subtitle("2019 · 剧情"),
        CardView::new("l09", "一一").with_subtitle("2000 · 家庭"),
        CardView::new("l10", "潜行者").with_subtitle("1979 · 科幻"),
        CardView::new("l11", "地球最后的夜晚").with_subtitle("2018 · 剧情"),
        CardView::new("l12", "沙丘").with_subtitle("2021 · 科幻"),
    ];
    view
}

/// 搜索页：固定中文查询词 + 6 张结果（正好一行 6 列网格）。
///
/// `total` 故意大于本页卡片数：验证结果头用的是"命中数"（服务端总数），
/// 而不是本页卡片数。
pub fn search_view() -> SearchView {
    let mut view = SearchView::from_cards(
        "沙丘",
        vec![
            CardView::new("s01", "沙丘：第二部")
                .with_subtitle("2024 · 科幻")
                .with_badge("4K"),
            CardView::new("s02", "沙丘")
                .with_subtitle("2021 · 科幻")
                .with_badge("HDR"),
            CardView::new("s03", "沙丘：预言").with_subtitle("2024 · 剧集"),
            CardView::new("s04", "沙丘之子").with_subtitle("2003 · 剧集"),
            CardView::new("s05", "沙海").with_subtitle("2018 · 剧集"),
            CardView::new("s06", "沙丘魔堡").with_subtitle("2000 · 剧集"),
        ],
    );
    view.total = 12;
    view
}

/// 搜索页空态（查询词没有命中）。
pub fn search_empty() -> SearchView {
    SearchView::from_cards("没有这部片", Vec::new())
}

/// 详情页。
pub fn detail_view() -> DetailView {
    let mut view = DetailView::new("dune", "沙丘：第二部");
    view.original_title = Some("Dune: Part Two".to_string());
    view.meta_line = nova_ui::viewmodel::detail_meta_line(
        Some(2024),
        Some(std::time::Duration::from_secs(2 * 3600 + 46 * 60)),
        &["科幻".to_string(), "冒险".to_string(), "剧情".to_string()],
    );
    view.overview = Some(
        "保罗·厄崔迪与契妮及弗雷曼人并肩作战，走上复仇之路。他必须阻止一场只有他能预见、\
         却无法避免的可怕未来。家族、爱情与宇宙命运在沙海之上交汇。"
            .to_string(),
    );
    view.play_label = "从 1:12:34 继续".to_string();
    view.badge = Some("4K".to_string());
    view.genres = vec!["科幻".to_string(), "冒险".to_string(), "剧情".to_string()];
    view.sources = vec![
        SourceOption::new("s1", "4K HDR · 简体中文")
            .with_subtitle("HEVC · 22.4 Mbps · 杜比全景声")
            .with_selected(true),
        SourceOption::new("s2", "1080p · 简体中文").with_subtitle("H.264 · 8.1 Mbps · AAC 5.1"),
        SourceOption::new("s3", "4K · 原声").with_subtitle("HEVC · 20.0 Mbps · 英文字幕"),
    ];
    view
}

/// 程序化音轨列表（编号、详情、选中项都像真引擎枚举）。
pub fn audio_tracks() -> Vec<TrackItem> {
    vec![
        TrackItem::new(1, "国语 · 杜比全景声 7.1")
            .with_detail("8 声道 · 48 kHz · 默认")
            .with_selected(true),
        TrackItem::new(2, "英语 · AC3 5.1").with_detail("6 声道 · 48 kHz"),
        TrackItem::new(3, "英语 · AAC 2.0 评论音轨").with_detail("2 声道 · 44.1 kHz"),
    ]
}

/// 程序化字幕列表（含一条图形字幕，用来演示"样式不可改"）。
pub fn subtitle_tracks() -> Vec<TrackItem> {
    vec![
        TrackItem::new(10, "简体中文 (SRT)").with_detail("文本 · 21 KB"),
        TrackItem::new(11, "English (SRT)").with_detail("文本 · 24 KB"),
        TrackItem::new(12, "简体中文 (PGS)")
            .with_detail("图形")
            .with_bitmap_subtitle(),
    ]
}

/// 播放页：正常播放中，控制层可见。
pub fn player_view() -> PlayerView {
    PlayerView::new("沙丘：第二部")
        .with_subtitle("2024 · 科幻 / 冒险")
        .with_duration(std::time::Duration::from_secs(2 * 3600 + 18 * 60))
        .with_position(std::time::Duration::from_secs(12 * 60 + 34))
        .with_buffered(std::time::Duration::from_secs(52 * 60))
        .with_playing(true)
        .with_volume(0.6)
        .with_audio_tracks(audio_tracks())
        .with_subtitle_tracks(subtitle_tracks())
        .with_engine_line("直连 · hwdec=vaapi · 1080p60")
}

/// 播放页：3 秒静默后控制层已隐藏。
pub fn player_controls_hidden() -> PlayerView {
    player_view().with_controls_visible(false)
}

/// 播放页：缓冲中（画面保留，角落提示，控件不隐藏）。
pub fn player_buffering() -> PlayerView {
    player_view().with_buffering(true)
}

/// 播放页：网络错误卡。
pub fn player_error() -> PlayerView {
    player_view().with_error(
        "服务器不可达，检查网络或服务器状态；已下载的部分仍可继续播放。",
        "重试",
    )
}

/// 播放页：音轨菜单展开。
pub fn player_audio_menu() -> PlayerView {
    let mut view = player_view();
    view.open_audio_menu();
    view
}

/// 播放页：字幕菜单展开（含"关闭"与图形字幕说明）。
pub fn player_subtitle_menu() -> PlayerView {
    let mut view = player_view();
    view.open_subtitle_menu();
    view
}

/// 给一组条目推入程序化海报。
pub fn install_posters(app: &App, ids: &[&str]) {
    for id in ids {
        app.set_poster(*id, poster(id));
    }
}

/// 推入 hero / 详情的背景图。
pub fn install_backdrops(app: &App, ids: &[&str]) {
    for id in ids {
        app.set_backdrop(*id, backdrop(id));
    }
}

/// 首页需要图片的全部 id。
pub fn home_image_ids(view: &HomeView) -> Vec<&str> {
    let mut ids = Vec::new();
    if let Some(hero) = &view.hero {
        ids.push(hero.item_id.as_str());
    }
    ids.extend(
        view.continue_watching
            .iter()
            .map(|card| card.item_id.as_str()),
    );
    ids.extend(view.recently_added.iter().map(|card| card.item_id.as_str()));
    for row in &view.library_rows {
        ids.extend(row.items.iter().map(|card| card.item_id.as_str()));
    }
    ids
}
