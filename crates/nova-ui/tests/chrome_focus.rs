// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 纯键盘 / 遥控器能否到达**顶栏**（库标签、搜索按钮）与首页 **hero 按钮行**
//! （体检 R2-2b）。
//!
//! 修复前：顶栏与 hero 按钮只响应鼠标（`TouchArea`），方向键在内容层的扁平焦点里
//! 上下夹住——纯键盘用户进不了媒体库页，也按不到「▶ 播放」/「更多信息」。
//!
//! 修复后是三层的焦点模型（见 `pages/app.rs` 的 `FocusLayer`）：
//!
//! ```text
//!   顶栏（navbar）  ← / → 在 标签 0..N-1 → 搜索按钮 之间移动，Enter 激活，↓ 回下一层
//!      ↑↓
//!   hero（hero）    首页 billboard 的两个按钮，← / → 切换，Enter 激活
//!      ↑↓
//!   内容（content） 卡片 / 网格（原来的扁平焦点）
//! ```
//!
//! 本文件的每条断言对应验收里的一条：首页 `↑↑` 到顶栏再切库、媒体库网格首行 `↑`
//! 到顶栏、hero 播放 / 更多信息、顶栏 `Esc` 只退层不返回、搜索槽 Enter 后输入框
//! 拿到焦点、800px 宽下被截断的标签会横向滚入可见区。

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use common::{home_filled, init_backend, library_view};
use nova_ui::{App, AppWindow, FocusLayer, HomeView, LibraryTab, Page, Route, SearchView, Theme};
use slint::ComponentHandle;
use slint::platform::Key;

/// 真实媒体库的标签行（11 个）：只有这么多标签才会在 800px 下溢出。
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

fn setup(width: f32, height: f32) -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    // testing 后端不一定为 `Window.width` 发 `changed`：镜像值也要写（见 `app.slint`）。
    app.ui().set_window_w(width);
    app.ui().set_window_h(height);
    app
}

/// 首页视图：hero 带简介。
///
/// `home.slint` 只在 `hero.overview != ""` 时实例化那两个按钮，所以"hero 按钮行"
/// 存在的前提就是有简介（没有简介时 `↑` 直接跳过 hero 去顶栏，不会进死层）。
fn home_with_hero_overview() -> HomeView {
    let mut home = home_filled();
    home.hero = home
        .hero
        .map(|hero| hero.with_overview("保罗·厄崔迪与弗雷曼人并肩作战，走上复仇之路。"));
    home
}

fn setup_home() -> App {
    let app = setup(1280.0, 800.0);
    let home = home_with_hero_overview();
    app.set_home(&home);
    app.show_home();
    app
}

/// 媒体库页（导航栈：首页 → 该库）。
fn setup_library(tabs: Vec<LibraryTab>) -> App {
    let app = setup(1280.0, 800.0);
    let home = home_filled();
    app.set_home(&home);
    app.show_home();
    let mut view = library_view();
    view.library_tabs = tabs;
    view.current_tab = 0;
    app.set_library(&view);
    app.open_library(&view);
    app
}

fn press(app: &App, key: Key) {
    let text: slint::SharedString = key.into();
    app.press_key(text).expect("按键");
}

fn type_text(app: &App, text: &str) {
    for ch in text.chars() {
        app.press_key(ch.to_string().into()).expect("按键");
    }
}

fn settle(app: &App) {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
    let _ = app;
}

/// 首页 `↑↑` 到顶栏 → `→ →` 到第 3 个标签（下标 2）→ `Enter` 进该库。
#[test]
fn home_keyboard_reaches_the_navbar_and_enters_a_library() {
    let app = setup_home();
    assert_eq!(app.focus_layer(), FocusLayer::Content, "前提：焦点在内容层");
    assert!(!app.ui().get_nav_active(), "前提：还没按过方向键");

    press(&app, Key::UpArrow);
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Hero,
        "内容层第一行再往上 = hero 按钮行"
    );
    assert!(app.ui().get_nav_active(), "键盘导航中（焦点环的门控）");

    press(&app, Key::UpArrow);
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Navbar,
        "hero 再往上 = 顶栏（纯键盘可达）"
    );
    assert_eq!(app.nav_focus(), 0, "顶栏焦点从当前标签开始");

    press(&app, Key::RightArrow);
    press(&app, Key::RightArrow);
    assert_eq!(app.nav_focus(), 2, "两次右移到第 3 个标签");

    let tabs = home_with_hero_overview().library_tabs;
    let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let activated = activated.clone();
        app.on_tab_activated(move |id| activated.borrow_mut().push(id.to_string()));
    }
    press(&app, Key::Return);
    assert_eq!(
        activated.borrow().as_slice(),
        [tabs[2].id.clone()],
        "Enter 激活的是焦点所在的标签（与点击标签同一条回调）"
    );
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Content,
        "选完标签回内容层（焦点环不留在顶栏）"
    );
}

/// 媒体库网格首行 `↑` 到顶栏，`→` 换一个标签，`Enter` 切库。
///
/// 同时钉住两件事：
/// - 焦点跑到顶栏后，内容网格**不再放大**焦点卡（不能同时亮两处焦点指示）；
/// - 鼠标点标签不会进入顶栏层（焦点环不会因为点击而留下）。
#[test]
fn library_grid_up_reaches_the_navbar_and_enter_switches_library() {
    let app = setup_library(real_tabs());
    let tabs = real_tabs();
    assert_eq!(app.page(), Page::Library);

    // 内容层：右移一格让 nav-active 生效，网格应该放大焦点卡。
    press(&app, Key::RightArrow);
    assert!(app.ui().get_library_zoom_on(), "内容层：焦点卡放大");
    press(&app, Key::UpArrow);
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Navbar,
        "网格首行再往上 = 顶栏"
    );
    assert_eq!(app.nav_focus(), 0, "顶栏焦点从当前标签开始");
    assert!(
        !app.ui().get_library_zoom_on(),
        "焦点在顶栏时内容网格不再放大（只有一处焦点指示）"
    );

    press(&app, Key::RightArrow);
    assert_eq!(app.nav_focus(), 1);

    let activated: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let activated = activated.clone();
        app.on_tab_activated(move |id| activated.borrow_mut().push(id.to_string()));
    }
    press(&app, Key::Return);
    assert_eq!(activated.borrow().as_slice(), [tabs[1].id.clone()]);

    // 鼠标点标签：不进顶栏层（焦点环只在键盘导航时出现）。
    app.refresh_targets();
    assert!(
        app.click_target("library.nav.tab.2").is_some(),
        "顶栏标签应该在可点目标清单里"
    );
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Content,
        "鼠标点击不会把焦点层切到顶栏"
    );
}

/// hero 按钮行：`Enter` 播放，`→ Enter` 更多信息（打开详情）。
#[test]
fn hero_buttons_are_keyboard_reachable() {
    let app = setup_home();

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Hero);
    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Player, "Enter = 「▶ 播放」");
    assert_eq!(
        app.current_route(),
        Some(Route::Player {
            item_id: "dune".to_string()
        })
    );

    // 播放页 Esc = 退出播放（回首页），焦点层要跟着复位。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home);
    assert_eq!(app.focus_layer(), FocusLayer::Content);

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Hero, "回到首页还能再进 hero");
    press(&app, Key::RightArrow);
    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Detail, "→ Enter = 「更多信息」");
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "dune".to_string()
        })
    );
}

/// 主推是剧集（不能直接播）时 hero 只剩「更多信息」：进 hero 焦点就落在它上面，
/// `←` / `→` 不会移到不存在的播放按钮，`Enter` 打开详情（不压播放路由）。
#[test]
fn series_hero_has_only_more_info_and_enter_opens_detail() {
    let app = setup(1280.0, 800.0);
    let mut home = home_with_hero_overview();
    home.hero = home.hero.map(|hero| hero.with_playable(false));
    app.set_home(&home);
    app.show_home();

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Hero);
    assert_eq!(app.ui().get_hero_focus(), 0, "唯一槽位是「更多信息」");
    press(&app, Key::RightArrow);
    press(&app, Key::LeftArrow);
    assert_eq!(
        app.ui().get_hero_focus(),
        0,
        "方向键不该移到不存在的「▶ 播放」"
    );

    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Detail, "Enter = 「更多信息」");
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "dune".to_string()
        }),
        "Enter 不该压入播放路由"
    );
}

/// hero 从可播变成不能播（数据刷新）时，停在「播放」槽位上的焦点要退回 0，
/// 不能留下一个看不见焦点环、`←` / `→` 也救不回来的悬空下标。
#[test]
fn hero_focus_falls_back_to_more_info_when_the_hero_stops_being_playable() {
    let app = setup_home();
    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Hero);
    press(&app, Key::RightArrow);
    assert_eq!(app.ui().get_hero_focus(), 1, "先停在「更多信息」上");

    let mut home = home_with_hero_overview();
    home.hero = home.hero.map(|hero| hero.with_playable(false));
    app.set_home(&home);
    assert_eq!(
        app.ui().get_hero_focus(),
        0,
        "不能播了，「更多信息」就是槽位 0"
    );

    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Detail, "Enter 仍然是打开详情");
}

/// 顶栏层 `Esc` 只回内容层，**不**返回上一页；再按一次才返回。
#[test]
fn escape_in_the_navbar_returns_to_content_without_going_back() {
    let app = setup_library(real_tabs());
    assert_eq!(app.navigator().len(), 2, "前提：首页 → 媒体库");

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Navbar);

    press(&app, Key::Escape);
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Content,
        "第一下 Esc 只退出顶栏层"
    );
    assert_eq!(app.page(), Page::Library, "没有返回上一页");
    assert_eq!(app.navigator().len(), 2, "导航栈没动");

    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home, "第二下 Esc 才是返回上一页");
}

/// 顶栏搜索槽：`Enter` 后输入框真的拿到焦点，打字进搜索页。
#[test]
fn navbar_search_slot_hands_focus_to_the_input() {
    init_backend();
    // 回调里要用 `App` 本体，所以从一开始就放进 `Rc`
    // （与 `search_handoff.rs` 里模拟 `Shell::begin_search` 的写法一致）。
    let app = Rc::new(App::new_with(AppWindow::new().expect("创建窗口")));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.ui().set_window_w(1280.0);
    app.ui().set_window_h(800.0);
    let home = home_with_hero_overview();
    app.set_home(&home);
    app.show_home();

    // 与 `nova-app` 的 `Shell::begin_search` 同一条链路：输入即打开搜索页并交接焦点。
    {
        let weak = Rc::downgrade(&app);
        app.on_search_changed(move |text| {
            if let Some(app) = weak.upgrade() {
                let view = SearchView::from_cards(text, Vec::new());
                app.open_search(&view);
                app.ui().invoke_focus_search_input();
            }
        });
    }

    press(&app, Key::UpArrow);
    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Navbar);

    // 槽位数 = 标签数 + 1（搜索按钮）；一路右移到搜索槽。
    let slots = home_with_hero_overview().library_tabs.len();
    for _ in 0..slots {
        press(&app, Key::RightArrow);
    }
    assert_eq!(app.nav_focus(), slots, "最后一个槽位是搜索按钮");

    press(&app, Key::Return);
    assert!(
        app.ui().get_search_focused(),
        "Enter 之后真焦点交给顶栏输入框"
    );
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Content,
        "焦点进输入框后不再占用顶栏层"
    );

    // 之前的 bug：字符进不了输入框。这里打字 → 顶栏输入框 → search-changed → 搜索页。
    type_text(&app, "沙丘");
    assert_eq!(app.search_text(), "沙丘");
    assert_eq!(app.page(), Page::Search, "打字后进搜索页");
    assert_eq!(
        app.current_route(),
        Some(Route::Search {
            term: "沙丘".to_string()
        })
    );
}

/// 顶栏焦点从**当前高亮的标签**开始：首页永远是 0（即使刚从某个库返回首页），
/// 媒体库是它自己的标签。
#[test]
fn navbar_starts_at_the_currently_highlighted_tab() {
    let app = setup_library(real_tabs());
    // 与"从第 2 个标签进的库"一致：`nav_current_tab` 会停在 1
    // （真机就是这么复现的：从「全部」返回首页后，焦点落在了第 2 个标签上）。
    let mut view = library_view();
    view.library_tabs = real_tabs();
    view.current_tab = 1;
    app.set_library(&view);
    assert_eq!(app.page(), Page::Library);
    press(&app, Key::UpArrow);
    assert_eq!(app.nav_focus(), 1, "库页：焦点从它自己的标签开始");
    press(&app, Key::Escape);

    // 从媒体库返回首页：首页顶栏常驻高亮「首页」，就算上次进的是另一个库，
    // 焦点也不能落在那个旧标签上（真机实测的 bug）。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home);
    press(&app, Key::UpArrow);
    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Navbar);
    assert_eq!(app.nav_focus(), 0, "首页顶栏焦点从「首页」标签开始");
}

/// 800px 宽：标签行被截断，焦点移到末尾的标签时要横向滚入可见区。
#[test]
fn narrow_navbar_scrolls_the_focused_tab_into_view() {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(800.0, 600.0));
    app.ui().set_window_w(800.0);
    app.ui().set_window_h(600.0);
    let home = home_filled();
    app.set_home(&home);
    app.show_home();
    let tabs = real_tabs();
    let mut view = library_view();
    view.library_tabs = tabs.clone();
    app.set_library(&view);
    app.open_library(&view);
    app.ui().show().expect("显示窗口");
    settle(&app);

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Navbar);
    assert_eq!(app.ui().get_nav_tabs_scroll_x(), 0.0, "起点没滚动");

    // 最后一个标签（下标 = 标签数 - 1）在 800px 下被裁在标签区外。
    for _ in 0..(tabs.len() - 1) {
        press(&app, Key::RightArrow);
    }
    assert_eq!(app.nav_focus(), tabs.len() - 1);
    settle(&app);

    let left = app.ui().get_nav_focus_tab_left();
    let right = app.ui().get_nav_focus_tab_right();
    let viewport = app.ui().get_nav_tabs_viewport_w();
    // 焦点环画在标签本体外 `Theme.focus-offset`：判定要连环一起算，
    // 只看标签本体的话末位标签的右侧描边被裁掉也“通过”。
    let theme = app.ui().global::<Theme>();
    let offset = theme.get_focus_offset();
    let focus_width = theme.get_focus_width();
    assert!(
        offset >= focus_width,
        "焦点环余量 {offset} 不能小于描边宽度 {focus_width}"
    );
    assert!(
        app.ui().get_nav_tabs_scroll_x() < 0.0,
        "标签区应该已经横向滚动（content-x = {}）",
        app.ui().get_nav_tabs_scroll_x()
    );
    assert!(
        left - offset >= -0.5 && right + offset <= viewport + 0.5,
        "焦点环要完整落在标签区里：ring_left={} ring_right={} offset={offset} viewport={viewport} scroll_x={}",
        left - offset,
        right + offset,
        app.ui().get_nav_tabs_scroll_x()
    );
}
