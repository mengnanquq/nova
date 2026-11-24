// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 假数据 + 真实窗口。运行：
//!
//! ```text
//! cargo run -p nova-ui --example demo
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use i_slint_core::graphics::{Rgba8Pixel, SharedPixelBuffer};
use nova_ui::{
    App, CardView, DetailView, HomeView, LibraryTab, LibraryView, PlayerView, SourceOption,
    TrackItem,
};
use slint::ComponentHandle;

const MOVIES: &[(&str, &str, &str)] = &[
    ("dune", "沙丘：第二部", "2024 · 科幻 / 冒险"),
    ("oppenheimer", "奥本海默", "2023 · 传记 / 剧情"),
    ("blade2049", "银翼杀手 2049", "2017 · 科幻"),
    ("arrival", "降临", "2016 · 科幻"),
    ("sicario", "边境杀手", "2015 · 犯罪"),
    ("burning", "燃烧", "2018 · 剧情"),
    ("mood", "花样年华", "2000 · 爱情"),
    ("parasite", "寄生虫", "2019 · 剧情"),
    ("yiyi", "一一", "2000 · 家庭"),
    ("stalker", "潜行者", "1979 · 科幻"),
    ("longday", "地球最后的夜晚", "2018 · 剧情"),
    ("dune1", "沙丘", "2021 · 科幻"),
];

const CONTINUE: &[(&str, &str, &str, f32)] = &[
    ("c1", "最后生还者", "第 2 季 · 第 5 集", 0.37),
    ("c2", "三体", "第 1 季 · 第 8 集", 0.62),
    ("c3", "奥本海默", "2023 · 传记", 0.21),
    ("c4", "繁花", "第 1 季 · 第 12 集", 0.84),
    ("c5", "沙丘", "2021 · 科幻", 0.08),
];

fn tabs() -> Vec<LibraryTab> {
    vec![
        LibraryTab::new("movies", "电影"),
        LibraryTab::new("shows", "剧集"),
        LibraryTab::new("anime", "动漫"),
        LibraryTab::new("docs", "纪录片"),
    ]
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

fn poster(seed: &str) -> slint::Image {
    let (width, height) = (400u32, 600u32);
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

fn backdrop(seed: &str) -> slint::Image {
    let (width, height) = (1280u32, 720u32);
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

fn find_title(item_id: &str) -> String {
    MOVIES
        .iter()
        .find(|(id, _, _)| *id == item_id)
        .map(|(_, title, _)| (*title).to_string())
        .or_else(|| {
            CONTINUE
                .iter()
                .find(|(id, _, _, _)| *id == item_id)
                .map(|(_, title, _, _)| (*title).to_string())
        })
        .unwrap_or_else(|| item_id.to_string())
}

fn detail_for(item_id: &str) -> DetailView {
    let title = find_title(item_id);
    let mut detail = DetailView::new(item_id, title);
    detail.original_title = Some("Demo Title".to_string());
    detail.meta_line = nova_ui::viewmodel::detail_meta_line(
        Some(2024),
        Some(std::time::Duration::from_secs(2 * 3600 + 46 * 60)),
        &["科幻".to_string(), "冒险".to_string(), "剧情".to_string()],
    );
    detail.overview = Some(
        "这是一段用于演示排版的简介文字。界面不下载图片、不访问网络，所有数据由上层压成视图模型后塞进来。\
         键盘左右移动焦点，Enter 打开详情，Esc 返回。"
            .to_string(),
    );
    detail.play_label = "从 1:12:34 继续".to_string();
    detail.badge = Some("4K".to_string());
    detail.genres = vec!["科幻".to_string(), "冒险".to_string(), "剧情".to_string()];
    detail.sources = vec![
        SourceOption::new("s1", "4K HDR · 简体中文")
            .with_subtitle("HEVC · 22.4 Mbps · 杜比全景声")
            .with_selected(true),
        SourceOption::new("s2", "1080p · 简体中文").with_subtitle("H.264 · 8.1 Mbps · AAC 5.1"),
        SourceOption::new("s3", "4K · 原声").with_subtitle("HEVC · 20.0 Mbps · 英文字幕"),
    ];
    detail
}

fn player_for(item_id: &str) -> PlayerView {
    PlayerView::new(find_title(item_id))
        .with_subtitle("2024 · 科幻 / 冒险")
        .with_duration(std::time::Duration::from_secs(2 * 3600 + 18 * 60))
        .with_position(std::time::Duration::from_secs(12 * 60 + 34))
        .with_buffered(std::time::Duration::from_secs(52 * 60))
        .with_playing(true)
        .with_volume(0.6)
        .with_audio_tracks(vec![
            TrackItem::new(1, "国语 · 杜比全景声 7.1")
                .with_detail("8 声道 · 48 kHz")
                .with_selected(true),
            TrackItem::new(2, "英语 · AC3 5.1").with_detail("6 声道 · 48 kHz"),
        ])
        .with_subtitle_tracks(vec![
            TrackItem::new(10, "简体中文 (SRT)")
                .with_detail("文本")
                .with_selected(true),
            TrackItem::new(12, "简体中文 (PGS)")
                .with_detail("图形")
                .with_bitmap_subtitle(),
        ])
        .with_engine_line("直连 · hwdec=vaapi · 1080p60")
}

fn home_view() -> HomeView {
    let mut home = HomeView::new();
    home.library_tabs = tabs();
    home.hero = Some(
        CardView::new("dune", "沙丘：第二部")
            .with_subtitle("2024 · 科幻 / 冒险 · 2 小时 46 分")
            .with_badge("4K"),
    );
    home.continue_watching = CONTINUE
        .iter()
        .map(|(id, title, subtitle, progress)| {
            CardView::new(*id, *title)
                .with_subtitle(*subtitle)
                .with_progress(*progress)
        })
        .collect();
    home.recently_added = MOVIES
        .iter()
        .map(|(id, title, subtitle)| CardView::new(*id, *title).with_subtitle(*subtitle))
        .collect();
    home
}

fn library_view() -> LibraryView {
    let mut library = LibraryView::new("movies", "电影");
    library.library_tabs = tabs();
    library.cards = MOVIES
        .iter()
        .map(|(id, title, subtitle)| CardView::new(*id, *title).with_subtitle(*subtitle))
        .collect();
    library
}

fn main() -> Result<(), slint::PlatformError> {
    let app = Rc::new(App::new()?);

    let home = home_view();
    let library = library_view();
    for (id, _, _) in MOVIES {
        app.set_poster(*id, poster(id));
        app.set_backdrop(*id, backdrop(id));
    }
    for (id, _, _, _) in CONTINUE {
        app.set_poster(*id, poster(id));
    }
    app.set_home(&home);
    app.set_library(&library);

    let current_detail = Rc::new(RefCell::new(detail_for("dune")));
    app.set_detail(&current_detail.borrow());
    app.ui().set_detail_focus(0);
    app.show_home();

    {
        let handle = app.clone();
        let current_detail = current_detail.clone();
        app.on_open_detail(move |item_id| {
            let detail = detail_for(item_id);
            *current_detail.borrow_mut() = detail.clone();
            handle.set_detail(&detail);
        });
    }
    {
        let handle = app.clone();
        app.on_play(move |item_id| {
            handle.set_player(&player_for(item_id));
        });
    }
    {
        let handle = app.clone();
        let current_detail = current_detail.clone();
        app.on_source_selected(move |source_id| {
            let mut detail = current_detail.borrow().clone();
            for source in &mut detail.sources {
                source.selected = source.source_id == source_id;
            }
            *current_detail.borrow_mut() = detail.clone();
            handle.set_detail(&detail);
        });
    }
    {
        let handle = app.clone();
        let library = library.clone();
        app.on_tab_activated(move |_| {
            handle.open_library(&library);
        });
    }

    app.ui().run()
}
