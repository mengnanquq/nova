// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 体检 R5-3 / R5-4：Ctrl+F 在**所有**页面都要能进搜索并聚焦输入框。
//!
//! 两条回归背景：
//! - 播放页的快捷键分支先匹配了 `f` 且没看修饰键，Ctrl+F 变成了全屏切换（R5-3）；
//! - 详情页没有顶栏搜索框，Ctrl+F 只把 `search-focused` 置真、不切页，随后字符被
//!   全局按键处理器复位（R5-4）。
//!
//! 测试用真实键事件（不是直接调 `handle_key`）：R5-3 正是 `.slint` 分派层的顺序
//! 问题，绕过它会假绿。Ctrl 用控制字符 `\u{11}` 按下 / 抬起包住字母键（与
//! `global_shortcuts.rs` 同一注入方式）。
//!
//! 上层（`nova-app` 的 `Shell::begin_search`）在输入时负责"打开搜索页 + 交接焦点"，
//! 这里用同样的两步模拟：`on_search_changed`（顶栏输入框）与 `on_search_input`
//! （搜索页大输入框）都走 `open_search` + `invoke_focus_search_input`。

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{
    detail_view, home_filled, home_image_ids, init_backend, install_posters, library_view,
    player_view,
};
use nova_ui::{App, AppWindow, Page, Route, SearchView};
use slint::ComponentHandle;
use slint::platform::WindowEvent;

const CONTROL: &str = "\u{11}";

fn dispatch(app: &App, text: &str) {
    let window = app.ui().window();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

fn type_text(app: &App, text: &str) {
    for ch in text.chars() {
        dispatch(app, &ch.to_string());
    }
}

/// Ctrl+字符：按下修饰键 → 发目标键 → 抬起修饰键。
fn ctrl(app: &App, key: &str) {
    let window = app.ui().window();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: CONTROL.into(),
    });
    dispatch(app, key);
    window.dispatch_event(WindowEvent::KeyReleased {
        text: CONTROL.into(),
    });
}

/// 与 `Shell::begin_search` 的同步路径一致：打开搜索页 + 焦点交接。
fn handoff(app: &App, text: &str) {
    let view = SearchView::from_cards(text, Vec::new());
    app.open_search(&view);
    app.ui().invoke_focus_search_input();
}

/// 建一个停在指定页面的应用，并接上与 `nova-app` 同形的搜索回调。
fn app_on(page: Page) -> Rc<App> {
    init_backend();
    let app = Rc::new(App::new_with(AppWindow::new().expect("创建窗口")));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);

    {
        let weak = Rc::downgrade(&app);
        app.on_search_changed(move |text| {
            if let Some(app) = weak.upgrade() {
                handoff(&app, text);
            }
        });
    }
    {
        let weak = Rc::downgrade(&app);
        app.on_search_input(move |text| {
            if let Some(app) = weak.upgrade() {
                handoff(&app, text);
            }
        });
    }

    match page {
        Page::Home => app.show_home(),
        Page::Library => {
            app.set_library(&library_view());
            app.show_page(Page::Library);
        }
        Page::Detail => {
            app.set_detail(&detail_view());
            app.open_detail("dune");
        }
        Page::Player => {
            app.set_detail(&detail_view());
            app.open_detail("dune");
            app.play("dune");
            app.set_player(&player_view());
        }
        Page::Search => {
            app.set_search(&SearchView::from_cards("", Vec::new()));
            app.show_page(Page::Search);
        }
    }
    app.ui().show().expect("显示窗口");
    if page == Page::Search {
        // 搜索页自己的大输入框先拿着焦点：Ctrl+F 要在输入框里也生效。
        app.ui().invoke_focus_search_input();
    }
    app
}

fn assert_search_landed(app: &App) {
    assert_eq!(app.page(), Page::Search, "Ctrl+F + 输入后应停在搜索页");
    assert_eq!(
        app.current_route(),
        Some(Route::Search {
            term: "沙丘".to_string()
        }),
        "路由顶端的查询词应是输入的内容"
    );
    assert_eq!(app.search_text(), "沙丘", "输入框里的文本");
}

#[test]
fn ctrl_f_from_home_opens_search_and_types() {
    let app = app_on(Page::Home);
    ctrl(&app, "f");
    assert!(app.ui().get_search_focused(), "首页 Ctrl+F 聚焦搜索输入框");
    type_text(&app, "沙丘");
    assert_search_landed(&app);
}

#[test]
fn ctrl_f_from_library_opens_search_and_types() {
    let app = app_on(Page::Library);
    ctrl(&app, "f");
    assert!(
        app.ui().get_search_focused(),
        "媒体库 Ctrl+F 聚焦搜索输入框"
    );
    type_text(&app, "沙丘");
    assert_search_landed(&app);
}

#[test]
fn ctrl_f_from_search_page_keeps_input_and_types() {
    let app = app_on(Page::Search);
    ctrl(&app, "f");
    assert!(
        app.ui().get_search_focused(),
        "搜索页 Ctrl+F 仍聚焦页内大输入框"
    );
    type_text(&app, "沙丘");
    assert_search_landed(&app);
}

/// R5-4：详情页没有顶栏搜索框，Ctrl+F 必须**直接进搜索页**并聚焦页内大输入框。
#[test]
fn ctrl_f_from_detail_opens_search_page_and_types() {
    let app = app_on(Page::Detail);
    assert_eq!(app.page(), Page::Detail, "前提：停在详情页");

    ctrl(&app, "f");

    assert_eq!(app.page(), Page::Search, "详情页 Ctrl+F 应直接打开搜索页");
    assert!(
        app.current_route()
            == Some(Route::Search {
                term: String::new()
            }),
        "打开的是空查询的搜索页"
    );
    assert!(app.ui().get_search_focused(), "焦点交给页内大输入框");

    type_text(&app, "沙丘");
    assert_search_landed(&app);
}

/// R5-3：播放页 Ctrl+F 不能变全屏。先走正常出口退出会话（提交进度）再进搜索；
/// 裸 `f` 仍然是全屏。
#[test]
fn ctrl_f_from_player_exits_session_and_opens_search() {
    let app = app_on(Page::Player);
    let exits = Rc::new(RefCell::new(Vec::<std::time::Duration>::new()));
    {
        let exits = exits.clone();
        app.on_player_exit(move |position| exits.borrow_mut().push(position));
    }

    // 裸 f：全屏（不能因为加了 Ctrl 判断就把原来的快捷键弄丢）。
    dispatch(&app, "f");
    assert!(app.player_fullscreen(), "裸 f 仍应切换全屏");
    assert_eq!(app.page(), Page::Player, "全屏不离开播放页");
    dispatch(&app, "f");
    assert!(!app.player_fullscreen(), "再按一次 f 退出全屏");

    let position = app.player_view().position;
    ctrl(&app, "f");

    assert_eq!(
        *exits.borrow(),
        vec![position],
        "Ctrl+F 离开播放页前必须先走会话退出回调（提交进度）"
    );
    assert_eq!(app.page(), Page::Search, "Ctrl+F 应退出播放页并打开搜索页");
    assert!(
        app.current_route()
            == Some(Route::Search {
                term: String::new()
            }),
        "打开的是空查询的搜索页"
    );
    assert!(
        app.ui().get_search_focused(),
        "焦点交给搜索页大输入框（不是在播放页上空挂）"
    );

    type_text(&app, "沙丘");
    assert_search_landed(&app);
}
