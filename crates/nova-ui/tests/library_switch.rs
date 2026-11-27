// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 切换媒体库必须回到顶部（R6-2），同一库的重建 / 详情返回必须保留滚动位置（R4-3）。
//!
//! 真机：在「动画电影」滚到 `-1080` 后点「海外电影」，新库直接落在半空中
//! （`library-scroll-y` 仍是 -1080）——网格的 `content-y` 是 `.slint` 局部滚动
//! 状态，换数据时没人归零。修复按 `LibraryView::id` 区分：
//!
//! - **换了库**：滚动归零 + 键盘焦点回到第 0 张；
//! - **同库重建 / 后台刷新 / 详情返回**：滚动与焦点都保留（`rebase_focus` 仍按
//!   条目 id 重新定位）。

mod common;

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use common::{init_backend, library_view};
use nova_ui::{App, AppWindow, CardView, LibraryView, Page};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

const W: f32 = 1280.0;
const H: f32 = 800.0;

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

/// 30 张卡 = 5 行（6 列），足够让焦点移出 1280×800 的首屏。
fn library(id: &str, title: &str, prefix: &str) -> LibraryView {
    let mut view = LibraryView::new(id, title);
    view.library_tabs = library_view().library_tabs;
    view.cards = (1..=30)
        .map(|index| {
            CardView::new(
                format!("{prefix}{index:02}"),
                format!("{title} 第 {index} 部"),
            )
        })
        .collect();
    view
}

fn setup(view: &LibraryView) -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(W);
    app.ui().set_window_h(H);
    app.ui().window().set_size(slint::LogicalSize::new(W, H));
    app.set_library(view);
    app.open_library(view);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));
    app
}

/// 把网格滚离顶部（键盘下移 3 行）。
fn scroll_down(app: &App) {
    for _ in 0..3 {
        press(app, Key::DownArrow);
    }
    assert_eq!(app.focus_index(), 18, "6 列网格：3 次 Down 到第 3 行");
    assert!(
        f64::from(app.ui().get_library_scroll_y()) < 0.0,
        "先决条件：网格真的滚动了（scroll-y = {}）",
        app.ui().get_library_scroll_y()
    );
}

fn assert_at_top(app: &App, label: &str) {
    assert_eq!(
        f64::from(app.ui().get_library_scroll_y()),
        0.0,
        "{label}：换库后必须回到顶部"
    );
    assert_eq!(app.page(), Page::Library);
    assert_eq!(app.focus_index(), 0, "{label}：键盘焦点回到第 0 张");
    assert_eq!(app.ui().get_library_focus(), 0);
}

/// A → B → A 双向都复位。
#[test]
fn switching_libraries_resets_scroll_and_focus() {
    let a = library("lib-a", "动画电影", "a");
    let b = library("lib-b", "海外电影", "b");
    let app = setup(&a);

    scroll_down(&app);
    app.open_library(&b);
    assert_at_top(&app, "动画电影 → 海外电影");

    scroll_down(&app);
    app.open_library(&a);
    assert_at_top(&app, "海外电影 → 动画电影");
}

/// 同一库的后台刷新（id 不变）保留滚动位置与焦点条目。
#[test]
fn refreshing_the_same_library_keeps_the_scroll_position() {
    let view = library("lib-a", "动画电影", "a");
    let app = setup(&view);
    scroll_down(&app);
    let scroll_before = f64::from(app.ui().get_library_scroll_y());

    // R2-8 的路径：后台刷新补条目、重排顺序，但库 id 不变。
    let mut refreshed = view.clone();
    refreshed.cards.reverse();
    refreshed
        .cards
        .insert(0, CardView::new("new-01", "刷新补进来的新片"));
    app.set_library(&refreshed);

    assert_eq!(
        f64::from(app.ui().get_library_scroll_y()),
        scroll_before,
        "同库刷新不能把人弹回顶部"
    );
    // 焦点按条目 id 重新定位（不是回到 0）。
    let focused = app.focus_index();
    assert_ne!(focused, 0, "同库刷新后焦点仍跟着原来的条目");
    assert_eq!(refreshed.cards[focused].item_id, "a19");
}

/// 从详情页返回：页面切回来时不重建视图，滚动位置必须保留（R4-3/a939fe9）。
#[test]
fn returning_from_detail_keeps_the_scroll_position() {
    let view = library("lib-a", "动画电影", "a");
    let app = setup(&view);
    scroll_down(&app);
    let scroll_before = f64::from(app.ui().get_library_scroll_y());
    let focus_before = app.focus_index();

    assert!(app.open_detail("a19"));
    assert_eq!(app.page(), Page::Detail);
    assert!(app.go_back());
    assert_eq!(app.page(), Page::Library);
    assert_eq!(
        f64::from(app.ui().get_library_scroll_y()),
        scroll_before,
        "详情返回必须保留滚动位置"
    );
    assert_eq!(app.focus_index(), focus_before);
}

/// 顶栏标签**点击**（`tab-activated` 回调）也走真实的切库入口（R6-2）。
///
/// 真实窗口里用户点的是 `Navbar` 的标签：`.slint` → `AppWindow.tab-activated` →
/// `nova-app` 的 `Shell::activate_tab` → `App::open_library`。这里用同一个回调把
/// “按 id 找库视图 → open_library”接起来，确认点击路径也会复位滚动与焦点。
#[test]
fn tab_activated_click_resets_scroll_and_focus() {
    let a = library("lib-a", "动画电影", "a");
    let b = library("lib-b", "海外电影", "b");
    let app = Rc::new(setup(&a));
    let views: HashMap<String, LibraryView> =
        [(a.id.clone(), a.clone()), (b.id.clone(), b.clone())].into();
    {
        let weak = Rc::downgrade(&app);
        app.on_tab_activated(move |library_id| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if let Some(view) = views.get(library_id) {
                // 与 `Shell::activate_tab` 同一入口。
                app.open_library(view);
            }
        });
    }

    scroll_down(&app);
    app.ui().invoke_tab_activated("lib-b".into());
    assert_at_top(&app, "tab-activated 动画电影 → 海外电影");

    scroll_down(&app);
    app.ui().invoke_tab_activated("lib-a".into());
    assert_at_top(&app, "tab-activated 海外电影 → 动画电影");
}
