// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 顶栏布局：窄窗口下库标签不能与右侧搜索控件重叠。
//!
//! 回归（800×600 实测）：媒体库页「日番」标签的目标矩形是 x733..761，而搜索按钮是
//! x732..768 —— 两个矩形相交，调试通道按标签坐标点击会点到搜索框。原因是标签行
//! （Flickable 的内容）比标签区宽，`absolute-position` 报的是**未裁剪**的布局坐标，
//! 而放不下的标签其实已经被裁在视口外。
//!
//! 断言用 `App::targets()` 的坐标（与 `--debug-socket` 的 `targets` 同一份数据）：
//! 在 800 / 1024 / 1280 三个宽度下，任何 `*.nav.tab.*` 目标都不得与任何
//! `*.nav.search*` 目标相交。

mod common;

use std::time::Duration;

use common::{home_filled, init_backend, library_view, search_view};
use nova_ui::{App, AppWindow, LibraryTab, Page, TargetInfo, Theme};
use slint::ComponentHandle;

/// 真实媒体库的标签行（10 个库 + 「首页」）：只有这么多标签才会在 800px 下溢出。
fn real_tabs() -> Vec<LibraryTab> {
    [
        "首页",
        "动画电影",
        "儿童节目",
        "国产电视剧",
        "国产动画片",
        "海外电影",
        "华语电影",
        "纪录片",
        "欧美电视剧",
        "日番",
        "日韩电视剧",
    ]
    .iter()
    .enumerate()
    .map(|(index, title)| LibraryTab::new(format!("tab-{index}"), *title))
    .collect()
}

fn setup() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let mut view = library_view();
    view.library_tabs = real_tabs();
    app.set_library(&view);
    app.show_page(Page::Library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    app
}

/// 推进代次让 `for` 里的标签重新登记，再读清单。
fn settle(app: &App) -> Vec<TargetInfo> {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    app.targets()
}

fn rects_intersect(left: &TargetInfo, right: &TargetInfo) -> bool {
    left.x < right.x + right.width
        && right.x < left.x + left.width
        && left.y < right.y + right.height
        && right.y < left.y + left.height
}

#[test]
fn navbar_tabs_never_overlap_the_search_control() {
    let app = setup();

    for (width, height) in [(800.0, 600.0), (1024.0, 576.0), (1280.0, 720.0)] {
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(width, height));
        // testing 后端不一定为 `Window.width` 发 `changed`：同时写镜像值，
        // 保证 `vw` 派生的 gutter / 间距按新宽度重算（见 `app.slint`）。
        app.ui().set_window_w(width);
        app.ui().set_window_h(height);

        let targets = settle(&app);
        let tabs: Vec<&TargetInfo> = targets
            .iter()
            .filter(|target| target.id.contains(".nav.tab."))
            .collect();
        let search: Vec<&TargetInfo> = targets
            .iter()
            .filter(|target| target.id.contains(".nav.search"))
            .collect();

        assert!(
            !search.is_empty(),
            "{width}x{height}: 顶栏搜索入口必须出现在清单里"
        );
        assert!(
            !tabs.is_empty(),
            "{width}x{height}: 标签行不能整个消失（清单：{:?}）",
            targets.iter().map(|t| &t.id).collect::<Vec<_>>()
        );

        for tab in &tabs {
            for control in &search {
                assert!(
                    !rects_intersect(tab, control),
                    "{width}x{height}: 标签 {} 「{}」({}..{}, {}..{}) 与 {} ({:?}) ({}..{}, {}..{}) 重叠",
                    tab.id,
                    tab.label,
                    tab.x,
                    tab.x + tab.width,
                    tab.y,
                    tab.y + tab.height,
                    control.id,
                    control.kind,
                    control.x,
                    control.x + control.width,
                    control.y,
                    control.y + control.height,
                );
                assert!(
                    tab.x + tab.width <= control.x,
                    "{width}x{height}: 标签 {} 的右缘 {} 越过了 {} 的左缘 {}",
                    tab.id,
                    tab.x + tab.width,
                    control.id,
                    control.x,
                );
            }
        }
    }
}

/// R7-C1：搜索页顶栏不能给**藏起来的**胶囊搜索框留位置。
///
/// 修复前（800×600 真机）：搜索页 `tabs_viewport_w` = 383，首页是 587。差值 204 =
/// 240（`Theme.search-box-width`，搜索框**展开态**宽度——搜索页的页内大输入框持有
/// 焦点时 `search-focused` 为真）− 36（收起态图标宽）。`visible: false` 的元素在
/// `HorizontalLayout` 里**照样占位**，于是搜索页少了两三个标签：「海外电…」被切一半，
/// 其后的标签整段不可见。
///
/// 期望：搜索页没有胶囊搜索框时它不得占位（标签区至少不比首页窄），页内大输入框
/// 横跨正文列、始终完整可见。
#[test]
fn search_page_navbar_does_not_reserve_space_for_the_hidden_search_box() {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(800.0, 600.0));
    app.ui().set_window_w(800.0);
    app.ui().set_window_h(600.0);

    let mut home = home_filled();
    home.library_tabs = real_tabs();
    app.set_home(&home);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    let home_w = f64::from(app.ui().get_nav_tabs_viewport_w());

    let search = search_view();
    app.set_search(&search);
    app.open_search(&search);
    // 真机到搜索页时页内大输入框持有焦点（顶栏的 `search-focused` 为真）。
    app.ui().invoke_focus_search_input();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    let search_w = f64::from(app.ui().get_nav_tabs_viewport_w());

    assert!(
        app.ui().get_search_focused(),
        "前提：页内大输入框持有焦点（`search-focused`）"
    );
    assert!(
        search_w >= home_w - 0.5,
        "搜索页标签区 {search_w} 比首页 {home_w} 窄：藏起来的搜索框还在按展开态（240px）占位"
    );

    // 搜索入口本身不能被裁：页内大输入框横跨正文列（左右各留一个 content-gutter）。
    let gutter = f64::from(app.ui().global::<Theme>().get_content_gutter());
    let input = settle(&app)
        .into_iter()
        .find(|target| target.id == "search.input")
        .expect("页内搜索框必须在目标清单里");
    assert!(
        (f64::from(input.x) - gutter).abs() <= 1.0,
        "页内搜索框左沿 {} 应等于正文留白 {gutter}",
        input.x
    );
    // 输入框右侧留出「×」按钮的位置（`search.slint` 的 clear-button + space-4），
    // 所以右沿停在窗口右侧留白之内，不能越出去（越出就是被窗口裁掉）。
    assert!(
        f64::from(input.x + input.width) <= 800.0 - gutter + 0.5,
        "页内搜索框右沿 {} 越过了右侧留白（窗口 800 − {gutter}）",
        input.x + input.width
    );
    assert!(
        f64::from(input.width) >= (800.0 - gutter * 2.0) * 0.8,
        "页内搜索框宽 {} 太窄（正文列宽 {}）",
        input.width,
        800.0 - gutter * 2.0
    );
}

/// 800px 下确实有一部分标签放不下（否则上面的断言是空的）：
/// 清单里少了标签，且留下的标签都在搜索控件左侧。
#[test]
fn narrow_window_drops_the_tabs_that_do_not_fit() {
    let app = setup();
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(800.0, 600.0));
    app.ui().set_window_w(800.0);
    app.ui().set_window_h(600.0);

    let targets = settle(&app);
    let tabs: Vec<&TargetInfo> = targets
        .iter()
        .filter(|target| target.id.contains(".nav.tab."))
        .collect();
    let search = targets
        .iter()
        .find(|target| target.id.contains(".nav.search"))
        .expect("顶栏搜索入口");

    assert!(
        tabs.len() < real_tabs().len(),
        "800px 下应有标签放不下：清单里却有 {} 个（总共 {} 个）",
        tabs.len(),
        real_tabs().len()
    );
    for tab in tabs {
        assert!(
            tab.x + tab.width <= search.x,
            "{} 的右缘 {} 必须停在搜索控件左缘 {} 之前",
            tab.id,
            tab.x + tab.width,
            search.x
        );
    }
}
