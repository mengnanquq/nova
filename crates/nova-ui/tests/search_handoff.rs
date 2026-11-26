// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 顶栏搜索框 → 搜索页大输入框的**焦点交接**（输入顺序回归）。
//!
//! 真机上的 bug：在首页顶栏搜索框输入 `12345`，`search.term` 变成 `23451`
//! （每敲一个新字符都插到最后一位之前）。
//!
//! 链路：第一个字符进 `search-changed` → 上层（`nova-app` 的 `Shell::begin_search`）
//! 本地缓存出结果 → `show_search(view, true)` → `open_search` 切到搜索页 +
//! `invoke_focus_search_input()` 把焦点交给页内大输入框。那个输入框的文本是经
//! 两向绑定**程序化**写进来的，`TextInput` 的光标停在 0 → 之后的字符全插到开头。
//! 本测试用与 `Shell` 同步路径相同的三步（`open_search` + `invoke_focus_search_input`）
//! 复现交接；在还没有修 `SearchPage.focus-input` 时本文件必须红。

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::{home_filled, init_backend};
use nova_ui::{App, AppWindow, KeyCommand, Page, SearchView};
use slint::ComponentHandle;
use slint::platform::Key;

fn setup() -> Rc<App> {
    init_backend();
    let app = Rc::new(App::new_with(AppWindow::new().expect("创建窗口")));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.set_home(&home_filled());
    app.show_page(Page::Home);

    // 与 `Shell::begin_search` 的同步路径一致：本地结果 → 打开搜索页 → 焦点交接。
    {
        let weak = Rc::downgrade(&app);
        app.on_search_changed(move |text| {
            if let Some(app) = weak.upgrade() {
                handoff(&app, text);
            }
        });
    }
    // 交接之后字符进的是搜索页大输入框（`search-input` 回调），也要走同一条链路。
    {
        let weak = Rc::downgrade(&app);
        app.on_search_input(move |text| {
            if let Some(app) = weak.upgrade() {
                handoff(&app, text);
            }
        });
    }

    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    app
}

fn handoff(app: &App, text: &str) {
    let view = SearchView::from_cards(text, Vec::new());
    app.open_search(&view);
    app.ui().invoke_focus_search_input();
}

fn type_text(app: &App, text: &str) {
    for ch in text.chars() {
        app.press_key(ch.to_string().into()).expect("按键");
    }
}

fn press(app: &App, key: Key) {
    app.press_key(key.into()).expect("按键");
}

#[test]
fn navbar_typing_keeps_character_order_after_the_focus_handoff() {
    let app = setup();
    // Ctrl+F：焦点交给首页顶栏的搜索框。
    assert!(app.handle_key(KeyCommand::Search));
    assert!(app.ui().get_search_focused());

    type_text(&app, "12345");
    assert_eq!(
        app.page(),
        Page::Search,
        "第一个字符就该切到搜索页（交接路径必须真的跑过）"
    );
    assert_eq!(
        app.search_text(),
        "12345",
        "顶栏输入的字符顺序不能乱：光标要落在交接过来的文本末尾"
    );

    // 交接之后再敲一个字符（这时已经在搜索页的大输入框里）也必须在末尾。
    type_text(&app, "6");
    assert_eq!(app.search_text(), "12345".to_owned() + "6");

    // 退格删的是最后一个字符，不是最开始那个。
    press(&app, Key::Backspace);
    assert_eq!(app.search_text(), "12345");
}

#[test]
fn chinese_text_survives_the_handoff() {
    let app = setup();
    assert!(app.handle_key(KeyCommand::Search));
    type_text(&app, "哆啦A梦");
    assert_eq!(app.search_text(), "哆啦A梦");

    press(&app, Key::Backspace);
    assert_eq!(app.search_text(), "哆啦A");
    type_text(&app, "梦");
    assert_eq!(app.search_text(), "哆啦A梦");
}

#[test]
fn cursor_moved_into_the_middle_is_not_forced_back_to_the_end() {
    let app = setup();
    assert!(app.handle_key(KeyCommand::Search));
    type_text(&app, "12345");

    // 光标移到中间（"12|345"）再输入：交接修好之后这里不能被强行推到末尾。
    press(&app, Key::LeftArrow);
    press(&app, Key::LeftArrow);
    press(&app, Key::LeftArrow);
    type_text(&app, "X");
    assert_eq!(app.search_text(), "12X345");
}
