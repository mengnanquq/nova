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
use nova_ui::{App, AppWindow, CardView, FocusLayer, HomeView, Page, Theme};
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
/// 焦点行是 `[2, 30, 4, 3]`：继续观看 2 张、最近添加**一整行** 30 张，之后
/// 两个库行（`home_filled` 的「电影」4 / 「剧集」3）。横向滚动的量足够大。
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

    // 1 次 Down：继续观看 → 最近添加第 1 张（focus = 2）。最近添加整行在
    // 800×600 的首屏之外，页面必须跟着纵向滚。
    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 2, "Down 应落到最近添加的第 1 张");
    assert!(
        f64::from(app.ui().get_home_scroll_y()) < 0.0,
        "焦点行在首屏之外时必须滚动（scroll-y = {}）",
        app.ui().get_home_scroll_y()
    );
    assert_focus_card_visible(
        "首页（最近添加第 1 张）",
        app.ui().get_home_viewport_h(),
        app.ui().get_home_focus_top(),
        app.ui().get_home_focus_bottom(),
        vpad,
    );

    // 行内 20 次 Right：焦点沿「最近添加」这一行横move（2 → 22），横向滚动必须跟上。
    // 体检 R7-2：以前 Down / Up 被当成“换行网格”（每次 +6 张），Right 反而是唯一
    // 能在行内前进的键；现在行内前进只归 ← / →。
    for _ in 0..20 {
        press(&app, Key::RightArrow);
    }
    assert_eq!(app.focus_index(), 22, "Right 每次在图内前进一张（+1）");
    assert!(
        f64::from(app.ui().get_home_row_scroll_x()) < 0.0,
        "焦点卡横向超出视口时必须横向滚动（row content-x = {}）",
        app.ui().get_home_row_scroll_x()
    );
    assert_focus_card_visible_horizontally(
        "首页（最近添加第 21 张）",
        app.ui().get_home_row_viewport_w(),
        app.ui().get_home_focus_left(),
        app.ui().get_home_focus_right(),
        hpad,
    );
    assert_focus_card_visible(
        "首页（最近添加第 21 张，纵向）",
        app.ui().get_home_viewport_h(),
        app.ui().get_home_focus_top(),
        app.ui().get_home_focus_bottom(),
        vpad,
    );

    // Down 进入库行：列号夹到该行的卡片数（「电影」行只有 4 张 → 第 4 列）。
    // 库行是 `for` 重复元素：纵向由 `HomePage.scroll-to-index` 按行号算，横向由页面
    // 下发 `keyboard-scroll` 给行自己执行（R7-2），用目标清单断言。
    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 35, "Down 应进入第一个库行的第 4 张卡");
    // 库行的横向跟随在行自己的 `changed keyboard-scroll` 里（下一帧生效）。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    app.refresh_targets();
    let card = app
        .target("home.row.lib.0.card.3")
        .expect("库行焦点卡必须滚进窗口");
    assert!(
        f64::from(card.y) >= 0.0 && f64::from(card.y + card.height) <= f64::from(H),
        "库行焦点卡纵向必须在窗口里：y={} h={}",
        card.y,
        card.height
    );
    // 横向也要跟着走：焦点卡整张落在窗口宽度里（行自己的横向 Flickable 已跟随）。
    assert!(
        f64::from(card.x) >= -0.5 && f64::from(card.x + card.width) <= f64::from(W) + 0.5,
        "库行焦点卡横向必须在窗口里：x={} w={}（窗口宽 {W}）",
        card.x,
        card.width
    );

    // Up 回「最近添加」的同一列（第 4 列 → focus = 2 + 3）。
    press(&app, Key::UpArrow);
    assert_eq!(app.focus_index(), 5, "Up 保持最接近的列");
    assert_focus_card_visible(
        "首页（最近添加第 4 张）",
        app.ui().get_home_viewport_h(),
        app.ui().get_home_focus_top(),
        app.ui().get_home_focus_bottom(),
        vpad,
    );

    // Left 到行首，再 Up 回继续观看第 1 张。
    for _ in 0..5 {
        press(&app, Key::LeftArrow);
    }
    assert_eq!(app.focus_index(), 2, "Left 夹在行首");
    press(&app, Key::UpArrow);
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

/// A1：滚轮把一行横向滚走之后，再 ↑ 到 hero、↓ 回到**同一张卡**，行必须重新滚回来。
///
/// 修复前：`CardRow.changed keyboard-scroll` 只在**值变化**时触发。滚轮只改行的
/// `content-x`（不改 `keyboard-scroll`），回到同一格时请求的 row/local 与上次相同 →
/// 不滚 → 焦点卡留在视口外（按 Enter 打开的是一张看不见的卡）。行首按 ←、末行按 ↓
/// 也是同一类「请求没变但必须重滚」。
#[test]
fn returning_to_the_same_focused_card_scrolls_the_row_back() {
    let app = app_800x600();
    let mut view = home_filled();
    view.hero = view
        .hero
        .map(|hero| hero.with_overview("保罗·厄崔迪与弗雷曼人并肩作战。"));
    view.continue_watching = (0..12)
        .map(|index| CardView::new(format!("c{index:02}"), format!("第 {index} 集")))
        .collect();
    // 只留一行：纵向不必滚，断言只关心行内横向。
    view.recently_added.clear();
    view.library_rows.clear();
    app.set_home(&view);
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
    app.refresh_targets();
    let theme = app.ui().global::<Theme>();
    let hpad = theme.get_zoom_allowance_x() / 2.0;

    // 一次 Right：焦点落在继续观看第 2 张，`nav-active` 生效（用户已开始导航）。
    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 1, "焦点应在继续观看第 2 张");
    let row_w = app.ui().get_home_row_viewport_w();

    // 滚轮把这一行横向滚走（与真机一样：只动 content-x，焦点不变）。
    //
    // 指针停在两张卡之间的 `card-gap` 里，而且每一步滚的都是**整数张卡**
    // （卡宽 + 间距 = 212px），所以指针始终落在同一个「缝」里——没有卡被悬停，
    // `focus-hover` 不会把焦点改掉（悬停会同步焦点，那是另一条语义）。
    let card = app
        .target("home.row.continue.card.1")
        .expect("第 2 张卡在视口里");
    let (x, y) = card.center();
    let gap_x = card.x + card.width + 6.0;
    assert!(gap_x > x, "缝隙在卡片右侧（x={gap_x}）");
    for _ in 0..3 {
        app.ui()
            .window()
            .dispatch_event_with_result(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(gap_x, y),
                delta_x: -212.0,
                delta_y: 0.0,
            })
            .expect("派发 PointerScrolled");
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
    assert_eq!(
        app.focus_index(),
        1,
        "滚轮不该改变焦点（指针停在两张卡之间的缝隙里）"
    );
    assert!(
        f64::from(app.ui().get_home_row_scroll_x()) < 0.0,
        "滚轮必须真的把这一行横向滚出去（row content-x = {}）",
        app.ui().get_home_row_scroll_x()
    );
    assert!(
        f64::from(app.ui().get_home_focus_left()) < 0.0,
        "前提：焦点卡已被滚出视口左沿（focus_left = {}）",
        app.ui().get_home_focus_left()
    );

    // ↑ 到 hero、↓ 回到内容层的同一张卡：焦点请求与上次完全一样。
    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Hero, "首页首行 ↑ 先进 hero");
    press(&app, Key::DownArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Content);
    assert_eq!(app.focus_index(), 1, "↓ 回到同一格（焦点没变）");

    // 重新请求滚动后焦点卡必须重新完全可见。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    assert_focus_card_visible_horizontally(
        "首页（滚轮后回到同一格）",
        row_w,
        app.ui().get_home_focus_left(),
        app.ui().get_home_focus_right(),
        hpad,
    );
}
