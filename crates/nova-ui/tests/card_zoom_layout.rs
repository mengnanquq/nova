// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 卡片几何与放大的断言（官方 Netflix 结构：**标题在卡片下方** + 悬停只放大 1.05）。
//!
//! 一、「放大不跳布局」：同一行在放大前后，其它卡的像素必须逐位一致。
//!
//! 做法：headless 渲染两张快照——放大前（焦点不在行内）与放大后（焦点落在
//! 继续观看第 2 张卡，mock 时间推过 400ms 延迟 + 200ms 过渡）——逐像素求差异，
//! 每个差异像素必须落在 Slint 侧报告的「放大覆盖层 ∪ 行左右箭头」几何内
//! （±2px 光栅余量）。箭头是放大态才出现的合法元素；除它之外，任何超出
//! 覆盖层几何的差异都意味着布局被放大卡跳动了。
//!
//! 二、放大卡不得盖住行标题（旧 1.35 + 悬停信息面板的直接 bug）：
//! 断言覆盖层上沿严格低于行标题下沿（两者矩形不相交）。
//!
//! 三、卡片总高 = 图高 + 卡下标题块（官方 `li` 结构），标题单行不溢出。
//!
//! 官方真值来自 `/tmp/genre_inline.css`：
//! - 放大：`.…-dx6epf:hover{transform:scale(1.05);z-index:2}`；
//! - 卡下标题：`p.…-xyhsll`，`@media (min-width:1280px){margin-top:0.25rem}`、
//!   `@media (min-width:1920px){margin-top:0.5rem;line-height:1.5rem;font-size:1rem}`，
//!   即标题块高 1280–1919 = 4 + 21 = 25px、≥1920 = 8 + 24 = 32px。

mod common;

use std::time::Duration;

use common::{home_filled, home_image_ids, init_backend, install_posters, poster};
use nova_ui::{App, AppWindow, CardView, HomeView, Page, Theme};
use slint::ComponentHandle;

/// 行内翻页箭头的尺寸：官方 CSS 原文 `width:1.5rem;height:5rem`（24×80），
/// 与 `Theme.arrow-width` / `Theme.arrow-height` 同值（见 docs/ui-netflix-1to1.md §10）。
const ARROW_W: f64 = 24.0;
const ARROW_H: f64 = 80.0;

/// 官方悬停放大量：`transform: scale(1.05)`。
const ZOOM: f64 = 1.05;

/// 官方卡下标题块高（`margin-top` + `line-height`）：
/// 1280–1919 → 4 + 21 = 25；≥1920 → 8 + 24 = 32。
const CAPTION_H_1280: f64 = 25.0;
const CAPTION_H_1920: f64 = 32.0;

/// 官方行标题下距（h2 到卡片上沿）：12px。
const TITLE_GAP: f64 = 12.0;

/// 一个像素是否落在「覆盖层 ∪ 左右箭头」内（逻辑坐标 + 2px 余量）。
fn in_expected_region(x: f64, y: f64, zoom: (f64, f64, f64, f64), arrows: [(f64, f64); 2]) -> bool {
    let (zx, zy, zw, zh) = zoom;
    let in_zoom = x >= zx - 2.0 && x <= zx + zw + 2.0 && y >= zy - 2.0 && y <= zy + zh + 2.0;
    let in_arrow = arrows.iter().any(|&(ax, ay)| {
        x >= ax - 2.0 && x <= ax + ARROW_W + 2.0 && y >= ay - 2.0 && y <= ay + ARROW_H + 2.0
    });
    in_zoom || in_arrow
}

/// 按指定逻辑尺寸建一个窗口（testing 后端必须在 `show` 之前定好尺寸才会重排）。
fn app_of(width: f32, height: f32) -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(width);
    app.ui().set_window_h(height);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    app
}

#[test]
fn zooming_a_card_does_not_move_other_cards() {
    let app = app_of(1280.0, 800.0);

    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);

    // 基线：焦点不在任何行内（大下标 → 两行的 focused-index 都是 -1）。
    app.ui().set_home_focus(i32::MAX);
    // 先把数据入场动画（淡入 + 上浮 6px）推到终态，否则基线会停在动画中间帧，
    // 与后面的帧的差异会被误判成"放大跳了布局"。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));
    let before = app.ui().window().take_snapshot().expect("基线快照");

    // 放大：焦点落在继续观看行第 2 张卡；推过 400ms 延迟 + 200ms 过渡。
    app.ui().set_home_focus(1);
    assert!(app.ui().get_row0_zoom_on(), "焦点落在行内后覆盖层必须可见");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(700));
    let after = app.ui().window().take_snapshot().expect("放大后快照");

    let scale = f64::from(app.ui().window().scale_factor());
    // Slint 报告的覆盖层几何（逻辑坐标，窗口原点）→ 视口像素判定。
    let zoom = (
        f64::from(app.ui().get_zoom_rect_x()),
        f64::from(app.ui().get_zoom_rect_y()),
        f64::from(app.ui().get_zoom_rect_w()),
        f64::from(app.ui().get_zoom_rect_h()),
    );
    let arrow_y = f64::from(app.ui().get_row0_arrow_y());
    // 行宽 = 窗口宽；左右箭头分别贴住窗口左右缘。
    let width = f64::from(before.width()) / scale;
    let arrows = [(0.0, arrow_y), (width - ARROW_W, arrow_y)];

    let (pa, pb) = (before.as_slice(), after.as_slice());
    let w = before.width();
    let mut strays = 0usize;
    let mut first_stray: Option<(u32, u32)> = None;
    for (index, (a, b)) in pa.iter().zip(pb.iter()).enumerate() {
        if a != b {
            let x = ((index % w as usize) as f64 + 0.5) / scale;
            let y = ((index / w as usize) as f64 + 0.5) / scale;
            if !in_expected_region(x, y, zoom, arrows) {
                strays += 1;
                if first_stray.is_none() {
                    first_stray = Some(((index % w as usize) as u32, (index / w as usize) as u32));
                }
            }
        }
    }

    assert_eq!(
        strays, 0,
        "有 {strays} 个差异像素落在覆盖层/箭头几何之外（首个在 {first_stray:?}）：\
         放大卡跳动了行布局",
    );

    // ---- 覆盖层几何：官方 1.05 倍（±1px 光栅容差）----
    let poster_w = f64::from(app.ui().get_row0_poster_width());
    let poster_h = f64::from(app.ui().get_row0_poster_height());
    let card_h = f64::from(app.ui().get_row0_card_height());
    let (_, zy, zw, zh) = zoom;
    assert!(
        (zw - poster_w * ZOOM).abs() <= 1.0,
        "覆盖层宽 {zw} 应为图宽 {poster_w} × {ZOOM}（官方 scale 1.05）",
    );
    assert!(
        (zh - card_h * ZOOM).abs() <= 1.0,
        "覆盖层高 {zh} 应为卡片总高 {card_h} × {ZOOM}（官方 scale 1.05）",
    );

    // ---- 卡片总高 = 图高 + 卡下标题块（1280 档：4 + 21 = 25）----
    assert!(
        (card_h - (poster_h + CAPTION_H_1280)).abs() <= 1.0,
        "卡片总高 {card_h} 应等于图高 {poster_h} + 标题块 {CAPTION_H_1280}",
    );
    let theme = app.ui().global::<Theme>();
    assert!(
        (f64::from(theme.get_zoom_factor()) - ZOOM).abs() < 0.001,
        "Theme.zoom-factor 必须是官方 1.05（收到 {}）",
        theme.get_zoom_factor(),
    );
    assert!(
        (f64::from(theme.get_card_caption_height()) - CAPTION_H_1280).abs() <= 1.0,
        "1280 档标题块高应为 4 + 21 = {CAPTION_H_1280}（收到 {}）",
        theme.get_card_caption_height(),
    );

    // ---- 放大卡绝不与行标题相交（旧 1.35 + 信息面板的 bug 回归断言）----
    let title_bottom = f64::from(app.ui().get_row0_title_bottom());
    let row_top = f64::from(app.ui().get_row0_y());
    assert!(
        title_bottom > row_top,
        "行标题下沿 {title_bottom} 必在行顶 {row_top} 之下（探针顺序不对）",
    );
    assert!(
        zy >= title_bottom,
        "放大卡上沿 {zy} 压住了行标题（下沿 {title_bottom}）：覆盖层与行标题矩形相交",
    );
    // 放大卡必须还在卡片区里（不是被钳到别处），上下各只溢出 allowance/2。
    let card_top = title_bottom + TITLE_GAP;
    assert!(
        (zy - (card_top - (zh - card_h) / 2.0)).abs() <= 1.0,
        "放大卡应以卡片中心对齐（上沿 {zy}，期望 {}）",
        card_top - (zh - card_h) / 2.0,
    );
}

#[test]
fn card_caption_block_matches_official_1920_geometry() {
    let app = app_of(1920.0, 1080.0);

    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");

    let theme = app.ui().global::<Theme>();
    // ≥1920 档官方标题：`margin-top:0.5rem`(8) + `line-height:1.5rem`(24) = 32。
    assert!(
        (f64::from(theme.get_card_caption_height()) - CAPTION_H_1920).abs() <= 1.0,
        "1920 档标题块高应为 8 + 24 = {CAPTION_H_1920}（收到 {}）",
        theme.get_card_caption_height(),
    );
    // 「最近添加」是竖版卡行：官方 240×336（≥1920 档）+ 32 标题块。
    let poster_h = f64::from(app.ui().get_row1_poster_height());
    let card_h = f64::from(app.ui().get_row1_card_height());
    assert!(
        (poster_h - 336.0).abs() <= 1.0,
        "1920 竖版图高应为官方 336（收到 {poster_h}）",
    );
    assert!(
        (card_h - (poster_h + CAPTION_H_1920)).abs() <= 1.0,
        "卡片总高 {card_h} 应等于图高 {poster_h} + 标题块 {CAPTION_H_1920}",
    );
    // 官方 `li` 竖向合计：10.4 + 336 + 32 + 10.4 = 388.8（我们不含 li 的上下 padding）。
    assert!(
        (card_h - 368.0).abs() <= 1.0,
        "卡片内容高应为 336 + 32 = 368（收到 {card_h}）；加上官方 li 上下 padding 20.8 = 388.8",
    );
}

#[test]
fn caption_is_one_elided_line_inside_the_card() {
    // 只有一张卡 + 一个超长标题：标题必须单行、左/右/下都不溢出卡片。
    let mut home = HomeView::new();
    home.continue_watching = vec![CardView::new(
        "long",
        "一个长到不可能在 327 像素宽度里放得下的中文标题 \
         ——这句话只是为了把省略号挤出来，绝不允许折成第二行或者画到卡片外面去",
    )];
    let app = app_of(1280.0, 800.0);
    app.set_poster("long", poster("long"));
    app.set_home(&home);
    app.show_page(Page::Home);
    // 焦点移出行外：这张快照只测"基础卡 + 卡下标题"，不能拍进放大覆盖层。
    app.ui().set_home_focus(i32::MAX);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    let snapshot = app.ui().window().take_snapshot().expect("快照");
    let scale = f64::from(app.ui().window().scale_factor());
    let width = snapshot.width();
    let height = snapshot.height();
    let pixel = |x: f64, y: f64| -> (u8, u8, u8) {
        let px = (x * scale) as usize;
        let py = (y * scale) as usize;
        let p = snapshot.as_slice()[py * width as usize + px];
        (p.r, p.g, p.b)
    };

    let poster_h = f64::from(app.ui().get_row0_poster_height());
    let card_h = f64::from(app.ui().get_row0_card_height());
    let card_top = f64::from(app.ui().get_row0_title_bottom()) + TITLE_GAP;
    // 行卡从正文列起点（`content-gutter` = 152px @1280）开始。
    let card_x = 152.0;
    let card_w = f64::from(app.ui().get_row0_poster_width());
    // 标题块 = margin-top(4) + 行高(21)：文字行盒在块的最后 21px。
    let caption_top = card_top + poster_h + (card_h - poster_h) - 21.0;

    // 1) 标题确实画出来了：标题行里存在非背景（黑）像素。
    let mut text_pixels = 0;
    let mut y = caption_top;
    while y < caption_top + 21.0 {
        let mut x = card_x;
        while x < card_x + card_w {
            if pixel(x, y) != (0, 0, 0) {
                text_pixels += 1;
            }
            x += 1.0;
        }
        y += 1.0;
    }
    assert!(
        text_pixels > 40,
        "标题行里应该有文字像素（实测 {text_pixels}）"
    );

    // 2) 单行：卡片下沿之下（0..4px 的带子里）绝不能有文字像素。
    let card_bottom = card_top + card_h;
    let mut below = 0;
    for dy in 0..4 {
        let y = card_bottom + 0.5 + dy as f64;
        let mut x = card_x;
        while x < card_x + card_w {
            if pixel(x, y) != (0, 0, 0) {
                below += 1;
            }
            x += 1.0;
        }
    }
    assert_eq!(
        below, 0,
        "卡片下沿之下出现了 {below} 个像素：标题折行溢出了卡片"
    );

    // 3) 省略号：标题行的左右卡外区域（正文列左边与卡片右边）不能有文字像素。
    let mut outside = 0;
    let mut y = caption_top;
    while y < caption_top + 21.0 {
        let mut x = 0.0;
        while x < card_x - 4.0 {
            if pixel(x, y) != (0, 0, 0) {
                outside += 1;
            }
            x += 1.0;
        }
        let mut x = card_x + card_w + 4.0;
        while x < 900.0 && x * scale < width as f64 {
            if pixel(x, y) != (0, 0, 0) {
                outside += 1;
            }
            x += 1.0;
        }
        y += 1.0;
    }
    assert_eq!(
        outside, 0,
        "卡片左右外侧出现了 {outside} 个像素：标题没被省略号截断"
    );

    assert!(
        (card_h - (poster_h + CAPTION_H_1280)).abs() <= 1.0,
        "卡片总高 {card_h} 应等于图高 {poster_h} + 标题块 {CAPTION_H_1280}",
    );
    assert!(height > 0);
}
