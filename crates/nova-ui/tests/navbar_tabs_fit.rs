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
use nova_ui::{App, AppWindow, FocusLayer, LibraryTab, Page, TargetInfo, Theme};
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

/// R8-A1：最后一个标签滚到底后必须**完整**落在标签区里，首页与搜索页表现一致。
///
/// 修复前：探针记的是 `absolute-position`（已含滚动偏移），又加了一遍 `content-x`，
/// 报出的左右沿随"按下方向键那一刻已经滚到哪儿"漂移。真机 800×600 搜索页报
/// `focus_tab_right=643.2 > tabs_viewport_w=623`（首页同尺寸却是 574.6 ≤ 587）——
/// 看着像"搜索页标签行少算了一段余量"，其实两页标签行宽度、最大滚动量完全一致
/// （`content-width` = 标签行 `preferred-width`，两者贴合）。改成内容坐标 +
/// 当前滚动偏移后，滚到底的探针位置就是标签真正的位置：右沿贴视口右沿。
#[test]
fn last_tab_scrolls_fully_into_the_tabs_viewport() {
    for (width, height) in [(800.0f32, 600.0f32), (1920.0, 1080.0)] {
        for page in [Page::Home, Page::Search] {
            let (left, right, viewport, scroll_x) = last_tab_geometry(page, width, height);
            let label = format!("{page:?} {width}x{height}");
            assert!(
                left >= -0.5 && right <= viewport + 0.5,
                "{label}: 焦点标签要完整落在标签区里：left={left} right={right} viewport={viewport} scroll_x={scroll_x}"
            );
            if width < 1000.0 {
                assert!(
                    scroll_x < 0.0,
                    "{label}: 800px 下标签行必须已经横向滚动（content-x = {scroll_x}）"
                );
                // 滚到底 = Flickable 的滚动钳制处；标签行末端与内容宽度齐平，
                // 所以最后一个标签的右沿应当**贴住**视口右沿，不是差一截。
                assert!(
                    (viewport - right).abs() <= 1.0,
                    "{label}: 滚到底后最后一个标签的右沿 {right} 应贴住视口右沿 {viewport}（探针偏了 {}px）",
                    viewport - right
                );
            } else {
                assert!(
                    scroll_x.abs() < 0.5,
                    "{label}: 1920px 下标签全都放得下，不该有滚动（content-x = {scroll_x}）"
                );
            }
        }
    }
}

/// 把键盘焦点推到最后一个标签，返回 `(focus_tab_left, focus_tab_right,
/// tabs_viewport_w, tabs_scroll_x)`（都是顶栏探针的原样输出）。
fn last_tab_geometry(page: Page, width: f32, height: f32) -> (f32, f32, f32, f32) {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    app.ui().set_window_w(width);
    app.ui().set_window_h(height);

    let tabs = real_tabs();
    let mut home = home_filled();
    home.library_tabs = tabs.clone();
    app.set_home(&home);
    app.show_page(Page::Home);
    // 搜索页的顶栏标签来自首页视图（`App::set_search` 直接复用 `home.library_tabs`）。
    let search = search_view();
    app.set_search(&search);
    if page == Page::Search {
        app.open_search(&search);
    }
    assert_eq!(app.page(), page);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    // 进顶栏层（首页可能先经过 hero 按钮行）。
    for _ in 0..3 {
        if app.focus_layer() == FocusLayer::Navbar {
            break;
        }
        press(&app, slint::platform::Key::UpArrow);
    }
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Navbar,
        "{page:?} {width}x{height}: 顶栏层要能被方向键到达"
    );
    // 逐步移到最后一个标签：每一步都触发一次 `ensure-visible`（平滑滚动）。
    for _ in 0..(tabs.len() - 1) {
        press(&app, slint::platform::Key::RightArrow);
    }
    assert_eq!(
        app.nav_focus(),
        tabs.len() - 1,
        "{page:?} {width}x{height}: 焦点要能到最后一个标签"
    );
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(800));

    (
        app.ui().get_nav_focus_tab_left(),
        app.ui().get_nav_focus_tab_right(),
        app.ui().get_nav_tabs_viewport_w(),
        app.ui().get_nav_tabs_scroll_x(),
    )
}

/// 发一个键并让动画走一小步（50ms：`ensure-visible` 的平滑滚动还在途中，
/// 焦点刚落到标签上时 `content-x` 还没到钳制位置——这正是修复前探针漂移的来源）。
fn press(app: &App, key: slint::platform::Key) {
    let text: slint::SharedString = key.into();
    app.press_key(text).expect("按键");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(50));
}
