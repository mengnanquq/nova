// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 窗口可自由缩放：尺寸必须跟着窗口系统走，布局按新宽度重排。
//!
//! 回归的坑：`app.slint` 里曾经写着 `width: root.window-w; height: root.window-h;`
//! ——那是把窗口尺寸**绑死**在 1280×800 上，用户拖边框时窗口系统写进来的新尺寸
//! 立刻被绑定拉回去，表现就是"窗口拉不大"。这里断言真正的不变量：
//! 窗口尺寸变化后，`Theme.viewport-width` 必须等于新宽度（`vw` 派生的所有尺寸
//! 都从它来）。

mod common;

use common::*;
use nova_ui::App;
use slint::ComponentHandle;
use std::time::Duration;

/// 断点派生的正文留白：1280 档 152、1920 档 200。
fn probe_gutter(app: &App) -> f32 {
    app.ui().get_probe_content_gutter()
}

#[test]
fn window_resize_updates_viewport_and_layout() {
    init_backend();
    let app = App::new().expect("窗口");
    app.set_home(&home_filled());
    app.show_page(nova_ui::Page::Home);
    // 先按 1280 显示（默认值）。
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let small = probe_gutter(&app);
    let viewport_small = app.ui().get_probe_viewport_width();
    assert!(
        (viewport_small - 1280.0).abs() < 1.0,
        "1280 宽下 viewport-width 应为 1280，实际 {viewport_small}"
    );

    // 放大到 1920×1080：断点从 1280 档切到 1920 档，`vw` 派生尺寸必须跟着变。
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1920.0, 1080.0));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let viewport_large = app.ui().get_probe_viewport_width();
    assert!(
        (viewport_large - 1920.0).abs() < 1.0,
        "放大到 1920 后 viewport-width 必须是 1920（窗口尺寸被绑死就会停在上一个值），实际 {viewport_large}"
    );

    let large = probe_gutter(&app);
    assert!(
        large > small,
        "断点切换后正文留白应变大：1280 档 {small}、1920 档 {large}"
    );
}
