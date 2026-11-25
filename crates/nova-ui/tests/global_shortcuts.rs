// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 全局快捷键（Ctrl+N / Ctrl+F）在输入框里也必须生效。
//!
//! 回归背景：TextInput 会先把 Ctrl+字母当编辑快捷键吞掉，`key-pressed` 里
//! `return reject` 也不会把事件冒泡给 `AppWindow` 的全局 `keyboard` FocusScope，
//! 于是光标停在搜索框里时 Ctrl+N / Ctrl+F 完全没反应（`opened=0`）。
//! 修法是输入框主动识别这两个组合并上报 `global-shortcut(int)`（见
//! `ui/navbar.slint` / `ui/search.slint`），同时把 Ctrl+A/C/V/X/Z 留给输入框自己。

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{home_filled, home_image_ids, init_backend, install_posters, library_view};
use nova_ui::{App, AppWindow, Page, SearchView};
use slint::ComponentHandle;
use slint::platform::WindowEvent;

/// 修饰键字符：Slint 后端的修饰状态由 `key_codes` 的按下 / 抬起维护
/// （`Control` 就是 `\u{11}`，与 `--debug-socket` 的 `key` 命令同一套注入方式）。
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

fn window() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app
}

/// 搜索页：焦点在页内大输入框里。
fn search_page() -> App {
    let app = window();
    app.set_search(&SearchView::new("沙丘"));
    app.show_page(Page::Search);
    app.ui().show().unwrap();
    app.ui().invoke_focus_search_input();
    app
}

/// 媒体库页：焦点在顶栏搜索框里。
fn library_page_with_focused_nav_search() -> App {
    let app = window();
    app.set_library(&library_view());
    app.show_page(Page::Library);
    app.ui().show().unwrap();
    app.ui().set_search_focused(true);
    app.ui().invoke_focus_search_input();
    app
}

fn count_connect(app: &App) -> Rc<RefCell<usize>> {
    let opened = Rc::new(RefCell::new(0));
    let counter = opened.clone();
    app.on_connect_server(move || *counter.borrow_mut() += 1);
    opened
}

fn count_search(app: &App) -> Rc<RefCell<usize>> {
    let focused = Rc::new(RefCell::new(0));
    let counter = focused.clone();
    app.on_search(move || *counter.borrow_mut() += 1);
    focused
}

#[test]
fn ctrl_n_from_search_page_input_opens_dialog() {
    let app = search_page();
    let opened = count_connect(&app);
    let before = app.search_text();

    ctrl(&app, "n");

    assert_eq!(
        *opened.borrow(),
        1,
        "搜索页大输入框里的 Ctrl+N 应触发 connect-server（打开对话框）"
    );
    assert_eq!(
        app.search_text(),
        before,
        "Ctrl+N 不应被当成普通字符写进输入框"
    );
}

#[test]
fn ctrl_n_from_navbar_input_opens_dialog() {
    let app = library_page_with_focused_nav_search();
    let opened = count_connect(&app);

    ctrl(&app, "n");

    assert_eq!(
        *opened.borrow(),
        1,
        "顶栏搜索框里的 Ctrl+N 应触发 connect-server（打开对话框）"
    );
    assert_eq!(app.search_text(), "", "Ctrl+N 没有写进搜索框");
}

#[test]
fn ctrl_f_from_navbar_input_focuses_search() {
    let app = library_page_with_focused_nav_search();
    let focused = count_search(&app);
    let opened = count_connect(&app);

    ctrl(&app, "f");

    assert_eq!(*focused.borrow(), 1, "顶栏搜索框里的 Ctrl+F 应聚焦搜索");
    assert!(app.ui().get_search_focused());
    assert_eq!(*opened.borrow(), 0, "Ctrl+F 不应打开对话框");
}

#[test]
fn ctrl_f_from_search_page_input_keeps_search_focused() {
    let app = search_page();
    let focused = count_search(&app);
    let opened = count_connect(&app);

    ctrl(&app, "f");

    assert_eq!(
        *focused.borrow(),
        1,
        "搜索页输入框里的 Ctrl+F 也应记账并聚焦搜索"
    );
    assert!(app.ui().get_search_focused());
    assert_eq!(*opened.borrow(), 0, "Ctrl+F 不应打开对话框");
}

#[test]
fn ctrl_a_stays_inside_the_navbar_input() {
    let app = library_page_with_focused_nav_search();
    let focused = count_search(&app);
    let opened = count_connect(&app);

    type_text(&app, "abc");
    assert_eq!(app.search_text(), "abc");

    // Ctrl+A 必须被输入框自己处理（全选），而不是被转发成全局快捷键：
    // 随后输入 "x" 会替换掉选中的 "abc"。若 Ctrl+A 被转发/吞错，这里会是 "abcx"。
    ctrl(&app, "a");
    type_text(&app, "x");

    assert_eq!(
        app.search_text(),
        "x",
        "Ctrl+A 应在输入框内全选，而不是被转发"
    );
    assert_eq!(*focused.borrow(), 0, "Ctrl+A 不应触发 Ctrl+F 的聚焦搜索");
    assert_eq!(*opened.borrow(), 0, "Ctrl+A 不应打开对话框");
}
