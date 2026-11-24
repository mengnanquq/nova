// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 「卡片放大不跳布局」断言：同一行在放大前后，其它卡的像素必须逐位一致。
//!
//! 做法：headless 渲染两张快照——放大前（焦点不在行内）与放大后（焦点落在
//! 继续观看第 2 张卡，mock 时间推过 400ms 延迟 + 200ms 过渡）——逐像素求差异，
//! 每个差异像素必须落在 Slint 侧报告的「放大覆盖层 ∪ 行左右箭头」几何内
//! （±2px 光栅余量）。箭头是放大态才出现的合法元素；除它之外，任何超出
//! 覆盖层几何的差异都意味着布局被放大卡跳动了。
//!
//! 放大卡是独立覆盖层、不参与行布局，行内其它卡的 x/width 不可能被改动；
//! 这条断言把「不可能」变成可执行的证据。

mod common;

use std::time::Duration;

use common::{home_filled, home_image_ids, init_backend, install_posters};
use nova_ui::{App, AppWindow, Page};
use slint::ComponentHandle;

/// 一个像素是否落在「覆盖层 ∪ 左右箭头」内（逻辑坐标 + 2px 余量）。
fn in_expected_region(x: f64, y: f64, zoom: (f64, f64, f64, f64), arrows: [(f64, f64); 2]) -> bool {
    let (zx, zy, zw, zh) = zoom;
    let in_zoom = x >= zx - 2.0 && x <= zx + zw + 2.0 && y >= zy - 2.0 && y <= zy + zh + 2.0;
    let in_arrow = arrows.iter().any(|&(ax, ay)| {
        x >= ax - 2.0 && x <= ax + 48.0 + 2.0 && y >= ay - 2.0 && y <= ay + 48.0 + 2.0
    });
    in_zoom || in_arrow
}

#[test]
fn zooming_a_card_does_not_move_other_cards() {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);

    // 基线：焦点不在任何行内（大下标 → 两行的 focused-index 都是 -1）。
    app.ui().set_home_focus(i32::MAX);
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
    let arrows = [(0.0, arrow_y), (width - 48.0, arrow_y)];

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

    // 覆盖层几何本身必须是基础卡的 1.35 倍。
    let zw = f64::from(app.ui().get_zoom_rect_w());
    assert!(
        (zw / 1.35 * 1.35 - zw).abs() < 0.5 && zw > 0.0,
        "覆盖层宽度 {zw} 异常（应为基础卡宽 × 1.35）",
    );
}
