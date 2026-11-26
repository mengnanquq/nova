// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 首页的键盘焦点必须"看得见"：纵向滚到焦点行、行内横向滚到焦点卡（R2-2）。
//!
//! 回归背景：媒体库 / 搜索页在 N5 里给网格加了 `scroll-to-index`，首页行没有
//! 同等处理——`move_focus_by` 的 `Page::Home` 分支是空的。8 次 Down 之后焦点
//! 跑到屏幕外，`scroll_y` 仍是 0，按 Enter 打开的是看不见的条目。
//!
//! 断言与 `grid_focus_scroll.rs` 同一套口径：焦点卡的矩形（含放大余量）必须
//! 完全落在视口里；纵向看首页外层 Flickable，横向看该行自己的 Flickable。

mod common;

use std::time::Duration;

use common::{home_filled, init_backend};
use nova_ui::{App, AppWindow, CardView, HomeView, Page, Theme};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

const W: f32 = 800.0;
const H: f32 = 600.0;

fn app_800x600() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(W);
    app.ui().set_window_h(H);
    app.ui().window().set_size(slint::LogicalSize::new(W, H));
    app
}

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

/// 首页：2 张「继续观看」+ 30 张「最近添加」。
///
/// 焦点行是 `[2, 6, 6, 6, 6, 6]`（最近添加按 6 列切块，见 `HomeView::focus_rows`），
/// 所以 Down 会把焦点推到最近添加的后续块——横向滚动的量足够大。
fn home_with_many_recent() -> HomeView {
    let mut home = home_filled();
    home.continue_watching.truncate(2);
    home.recently_added = (1..=30)
        .map(|index| CardView::new(format!("h{index:02}"), format!("第 {index} 部")))
        .collect();
    home
}

/// 焦点卡必须完全落在视口里。
///
/// 探针报的是**视口相对**坐标（体检 R3-2：内容坐标 + 页面 / 行滚动偏移），所以
/// 直接与 `[0, viewport_h]` 比；`scroll_y` / `row_scroll_x` 只用于确认"真的滚了"。
fn assert_focus_card_visible(
    label: &str,
    viewport_h: f32,
    focus_top: f32,
    focus_bottom: f32,
    pad: f32,
) {
    let view_bottom = f64::from(viewport_h);
    let top = f64::from(focus_top);
    let bottom = f64::from(focus_bottom);
    assert!(
        top - f64::from(pad) >= -0.5,
        "{label}: 焦点卡上沿 {top}（含 {pad}px 放大余量）在视口上沿之上（视口高 {viewport_h}）"
    );
    assert!(
        bottom + f64::from(pad) <= view_bottom + 0.5,
        "{label}: 焦点卡下沿 {bottom}（含 {pad}px 放大余量）超出视口下沿 {view_bottom}"
    );
}

fn assert_focus_card_visible_horizontally(
    label: &str,
    row_viewport_w: f32,
    focus_left: f32,
    focus_right: f32,
    pad: f32,
) {
    let view_right = f64::from(row_viewport_w);
    let left = f64::from(focus_left);
    let right = f64::from(focus_right);
    assert!(
        left - f64::from(pad) >= -0.5,
        "{label}: 焦点卡左沿 {left} 在行视口左沿之外（行视口宽 {row_viewport_w}）"
    );
    assert!(
        right + f64::from(pad) <= view_right + 0.5,
        "{label}: 焦点卡右沿 {right} 超出视口右沿 {view_right}（行视口宽 {row_viewport_w}）"
    );
}

#[test]
fn home_keyboard_focus_scrolls_the_page_and_the_row() {
    let app = app_800x600();
    app.set_home(&home_with_many_recent());
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    assert_eq!(
        f64::from(app.ui().get_home_scroll_y()),
        0.0,
        "首屏不该预先滚动"
    );

    let theme = app.ui().global::<Theme>();
    let vpad = theme.get_zoom_allowance_y() / 2.0;
    let hpad = theme.get_zoom_allowance_x() / 2.0;

    // 1 次 Down：继续观看 → 最近添加第 1 块（focus = 2）。最近添加整行在
    // 800×600 的首屏之外，页面必须跟着纵向滚。
    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 2, "Down 应落到最近添加的第 1 块");
    assert!(
        f64::from(app.ui().get_home_scroll_y()) < 0.0,
        "焦点行在首屏之外时必须滚动（scroll-y = {}）",
        app.ui().get_home_scroll_y()
    );
    assert_focus_card_visible(
        "首页（最近添加第 1 块）",
        app.ui().get_home_viewport_h(),
        app.ui().get_home_focus_top(),
        app.ui().get_home_focus_bottom(),
        vpad,
    );

    // 再 4 次 Down：焦点沿最近添加的块前进（2 → 8 → 14 → 20 → 26），
    // 行内横向滚动必须跟上。
    for _ in 0..4 {
        press(&app, Key::DownArrow);
    }
    assert_eq!(app.focus_index(), 26, "每次 Down = 最近添加前进一块（+6）");
    assert!(
        f64::from(app.ui().get_home_row_scroll_x()) < 0.0,
        "焦点卡横向超出视口时必须横向滚动（row content-x = {}）",
        app.ui().get_home_row_scroll_x()
    );
    assert_focus_card_visible_horizontally(
        "首页（第 26 张）",
        app.ui().get_home_row_viewport_w(),
        app.ui().get_home_focus_left(),
        app.ui().get_home_focus_right(),
        hpad,
    );
    assert_focus_card_visible(
        "首页（第 26 张，纵向）",
        app.ui().get_home_viewport_h(),
        app.ui().get_home_focus_top(),
        app.ui().get_home_focus_bottom(),
        vpad,
    );

    // 5 次 Up 回到继续观看第 1 张：焦点卡仍然必须可见。
    for _ in 0..5 {
        press(&app, Key::UpArrow);
    }
    assert_eq!(app.focus_index(), 0, "Up 应回到继续观看第 1 张");
    assert_focus_card_visible(
        "首页（回到继续观看行）",
        app.ui().get_home_viewport_h(),
        app.ui().get_home_focus_top(),
        app.ui().get_home_focus_bottom(),
        vpad,
    );
    assert_focus_card_visible_horizontally(
        "首页（回到继续观看行）",
        app.ui().get_home_row_viewport_w(),
        app.ui().get_home_focus_left(),
        app.ui().get_home_focus_right(),
        hpad,
    );
}
