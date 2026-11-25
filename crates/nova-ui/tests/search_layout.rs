// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 搜索页结果网格的几何断言：6 列、5:7 竖版卡、左起点 = `content-gutter`。
//!
//! 探针来源与 `card_zoom_layout.rs` 同一套做法：几何由 `.slint` 自己算，
//! 测试只读 `AppWindow` 暴露的 `search-*` 属性（`PosterGrid` → `SearchPage` →
//! `AppWindow` 三层透传），不在这里重写网格公式——否则就是第二份"真相"。
//!
//! 搜索页与媒体库页共用 `ui/poster_grid.slint`，这条测试同时证明"抽组件后
//! 仍是 6 列 / 5:7"。

mod common;

use common::{init_backend, install_posters, search_view};
use nova_ui::{App, AppWindow, Page};
use slint::ComponentHandle;

/// 1280×800 基准窗口：与快照、媒体库断言同一尺寸。
const WINDOW_W: f64 = 1280.0;
const WINDOW_H: f64 = 800.0;

#[test]
fn search_grid_is_six_columns_of_five_by_seven_posters() {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    // 无头 testing 后端不会为创建后变更重排布局：尺寸必须在 show() 之前定好。
    app.ui().set_window_w(WINDOW_W as f32);
    app.ui().set_window_h(WINDOW_H as f32);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(WINDOW_W as f32, WINDOW_H as f32));

    let view = search_view();
    let ids: Vec<&str> = view
        .cards
        .iter()
        .map(|card| card.item_id.as_str())
        .collect();
    install_posters(&app, &ids);
    app.set_search(&view);
    app.show_page(Page::Search);
    app.ui().show().expect("显示窗口");

    let ui = app.ui();
    let columns = ui.get_search_columns();
    let card_w = f64::from(ui.get_search_card_w());
    // 图高（5:7 的海报本体）与卡片总高（图高 + 卡下标题块）是两个语义，别混。
    let poster_h = f64::from(ui.get_search_poster_h());
    let card_h = f64::from(ui.get_search_card_h());
    let row_h = f64::from(ui.get_search_row_h());
    let card_step = f64::from(ui.get_search_card_step());
    let gutter = f64::from(ui.get_search_content_gutter());
    let first_x = f64::from(ui.get_search_first_card_x());

    // 1) 6 列（与媒体库页同一断点下的列数）。
    assert_eq!(columns, 6, "1280 宽下搜索页必须是 6 列");

    // 2) 卡片左起点 = content-gutter（±1px 光栅容差）。
    assert!(
        (first_x - gutter).abs() <= 1.0,
        "首列左起点 {first_x} 应等于 content-gutter {gutter}"
    );

    // 3) 6 列 + 5 个 card-gap 正好铺满「窗口宽 - 两侧 gutter」。
    let span = f64::from(columns) * card_w + (f64::from(columns) - 1.0) * (card_step - card_w);
    let body = WINDOW_W - gutter * 2.0;
    assert!(
        (span - body).abs() <= 1.0,
        "6 列总宽 {span} 应等于正文列宽 {body}（列数或间距算错了）"
    );

    // 4) 竖版海报 5:7：**图高** = 卡宽 × 7 / 5（±1px），卡下标题不算在图里。
    let expected_h = card_w * 7.0 / 5.0;
    assert!(
        (poster_h - expected_h).abs() <= 1.0,
        "图高 {poster_h} 应为卡宽 {card_w} 的 7/5（期望 {expected_h}）"
    );
    assert!(card_w > 0.0 && poster_h > 0.0, "网格尺寸不能为 0");

    // 5) 卡片总高 = 图高 + 卡下标题块（1280 档官方 4 + 21 = 25px，±1px）。
    let caption_h = 25.0;
    assert!(
        (card_h - (poster_h + caption_h)).abs() <= 1.0,
        "卡片总高 {card_h} 应等于图高 {poster_h} + 标题块 {caption_h}"
    );

    // 6) 行高 = 卡片总高 + 行间距（12px）；行间距必须 ≥ 放大卡竖向溢出的一半（10px），
    //    否则 1.05 放大后会压到下一行。
    let row_spacing = row_h - card_h;
    assert!(
        (row_spacing - 12.0).abs() <= 1.0,
        "行间距 {row_spacing} 应为 12px（官方未给网格行距，沿用 card-gap）"
    );
    assert!(
        row_spacing >= 10.0,
        "行间距 {row_spacing} 小于 1.05 放大的竖向溢出（20px / 2），放大卡会压到下一行"
    );
}
