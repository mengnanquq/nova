// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 网格（媒体库 / 搜索页结果）里的键盘焦点必须"看得见"：
//!
//! 1. **可见指示**与首页完全同一套机制——`PosterGrid` 的 `CardZoom` 覆盖层
//!    （1.05 放大），由 `nav-active` + `focus-index` 驱动；
//! 2. 焦点移出可视区时，网格自动滚动到焦点卡**完全可见**。
//!
//! 回归：800×600 的媒体库里按方向键 Down，焦点跑到屏幕外——画面毫无变化，
//! 按 Enter 打开的却是看不见的那张卡（旧行为：`move_focus_by` 只改焦点索引，
//! 没有任何滚动）。搜索页结果网格共用 `PosterGrid`，同样要成立。

mod common;

use std::time::Duration;

use common::{init_backend, library_view, search_view, snapshots_dir, write_png};
use nova_ui::{App, AppWindow, CardView, Page, Theme};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

/// 出问题的真实窗口尺寸（800×600）。
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

/// 30 张卡 = 5 行（6 列）——足够把最后一行推到 800×600 的可视区之外。
fn many_cards(prefix: &str) -> Vec<CardView> {
    (1..=30)
        .map(|index| CardView::new(format!("{prefix}{index:02}"), format!("第 {index} 部")))
        .collect()
}

/// 焦点卡（含放大余量）必须完全落在可视窗口内。
///
/// 探针报的是**视口相对**坐标（体检 R3-2：内容坐标 + 页面滚动偏移），所以这里
/// 直接与 `[0, viewport_h]` 比，不需要再拿 `scroll_y` 换算。
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

/// 媒体库：焦点下移两屏后，网格自动滚动且焦点卡带放大指示。
#[test]
fn library_grid_zoom_and_scroll_follow_keyboard_focus() {
    let app = app_800x600();
    let mut library = library_view();
    library.cards = many_cards("l");
    app.set_library(&library);
    app.open_library(&library);
    app.ui().set_library_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    // 首屏：焦点卡就在可视区里，不该预先滚动。
    assert_eq!(
        f64::from(app.ui().get_library_scroll_y()),
        0.0,
        "首屏不该预先滚动"
    );

    // 3 次 Down：0 → 6 → 12 → 18（第 3 行，800×600 下在可视区之外）。
    for _ in 0..3 {
        press(&app, Key::DownArrow);
    }
    assert_eq!(app.focus_index(), 18, "6 列网格：一次 Down = 下一行（+6）");
    assert!(
        app.ui().get_library_zoom_on(),
        "键盘焦点卡必须有可见指示（与首页同一套 CardZoom 放大覆盖层）"
    );
    assert!(
        f64::from(app.ui().get_library_scroll_y()) < 0.0,
        "焦点卡在可视区外时必须滚动（scroll-y = {}）",
        app.ui().get_library_scroll_y()
    );

    let theme = app.ui().global::<Theme>();
    assert_focus_card_visible(
        "媒体库",
        app.ui().get_library_viewport_h(),
        app.ui().get_library_focus_top(),
        app.ui().get_library_focus_bottom(),
        theme.get_zoom_allowance_y() / 2.0,
    );

    // 人工复核用的稳定帧（软件光栅器，与窗口模式同一套 .slint 布局）。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(800));
    let snapshot = app.ui().window().take_snapshot().expect("媒体库快照");
    let path = snapshots_dir().join("library-focus-scrolled.png");
    write_png(&path, &snapshot);
    println!("snapshot 媒体库焦点滚动: {}", path.display());

    // 上移回第一行：焦点卡重新完全可见（不要求滚回 0，只要求真的看得见）。
    for _ in 0..3 {
        press(&app, Key::UpArrow);
    }
    assert_eq!(app.focus_index(), 0);
    assert_focus_card_visible(
        "媒体库（回到首行）",
        app.ui().get_library_viewport_h(),
        app.ui().get_library_focus_top(),
        app.ui().get_library_focus_bottom(),
        theme.get_zoom_allowance_y() / 2.0,
    );
}

/// 搜索页结果网格（共用 `PosterGrid`）同样要滚到焦点卡。
#[test]
fn search_grid_zoom_and_scroll_follow_keyboard_focus() {
    let app = app_800x600();
    let mut view = search_view();
    view.cards = many_cards("s");
    view.total = 30;
    app.show_page(Page::Home);
    app.set_search(&view);
    app.open_search(&view);
    app.ui().set_search_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    // 2 次 Down：0 → 6 → 12（第 2 行，搜索结果区在 800×600 下只装得下两行）。
    for _ in 0..2 {
        press(&app, Key::DownArrow);
    }
    assert_eq!(app.focus_index(), 12, "6 列网格：一次 Down = 下一行（+6）");
    assert!(
        app.ui().get_search_zoom_on(),
        "搜索结果网格的键盘焦点卡也必须有可见指示"
    );
    assert!(
        f64::from(app.ui().get_search_scroll_y()) < 0.0,
        "焦点卡在可视区外时必须滚动（scroll-y = {}）",
        app.ui().get_search_scroll_y()
    );

    let theme = app.ui().global::<Theme>();
    assert_focus_card_visible(
        "搜索页",
        app.ui().get_search_viewport_h(),
        app.ui().get_search_focus_top(),
        app.ui().get_search_focus_bottom(),
        theme.get_zoom_allowance_y() / 2.0,
    );
}
