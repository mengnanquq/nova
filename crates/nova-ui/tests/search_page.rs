// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 搜索页的交互接线：页内大输入框 → Rust 回调、Esc 清空、焦点入口。
//!
//! 几何由 `search_layout.rs` 断言；这里只验证"按键/回调有没有接上"，
//! 用 testing backend 发真实键事件（与 `search_banner.rs` 同一套做法）。

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{home_filled, init_backend, library_view, search_view};
use nova_ui::{App, AppWindow, Page};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

fn setup() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.set_search(&search_view());
    app.show_page(Page::Search);
    app.ui().show().expect("显示窗口");
    app
}

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn type_text(app: &App, text: &str) {
    let window = app.ui().window();
    for ch in text.chars() {
        let text: slint::SharedString = ch.to_string().into();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }
}

#[test]
fn page_input_edits_and_escape_clear_are_wired() {
    let app = setup();

    // Ctrl+F 在搜索页把焦点交给页内大输入框（`focus-search-input` 的新分支）。
    assert!(app.handle_key(nova_ui::KeyCommand::Search));
    assert!(app.ui().get_search_focused());

    let edits = Rc::new(RefCell::new(Vec::new()));
    {
        let edits = edits.clone();
        app.on_search_input(move |text| edits.borrow_mut().push(text.to_string()));
    }

    // 清空已有文本，再敲两个字：每次编辑都把当前全文报给 Rust。
    app.set_search_text("");
    type_text(&app, "沙丘");
    assert_eq!(
        edits.borrow().as_slice(),
        ["沙", "沙丘"],
        "页内输入框的每次编辑都应触发搜索回调"
    );
    assert_eq!(app.search_text(), "沙丘");

    // Esc：清空并回首页（回调交给上层；纯界面侧至少要把清除意图发出来）。
    let clears = Rc::new(RefCell::new(0));
    {
        let clears = clears.clone();
        app.on_search_clear(move || *clears.borrow_mut() += 1);
    }
    press(&app, Key::Escape);
    assert_eq!(*clears.borrow(), 1, "Esc 应触发一次清空");
    // nova-ui 的清空链路本身会把搜索框文本置空。
    assert_eq!(app.search_text(), "");
}

#[test]
fn escape_after_enter_still_clears_from_the_page() {
    let app = setup();

    // Enter 之后焦点回到全局键盘（页内 `search-blur`），此时 Esc 走 `KeyCommand::Back`；
    // 搜索页上它必须等价于"清空 + 回首页"，而不是只弹一层栈。
    let clears = Rc::new(RefCell::new(0));
    {
        let clears = clears.clone();
        app.on_search_clear(move || *clears.borrow_mut() += 1);
    }
    assert!(app.handle_key(nova_ui::KeyCommand::Search));
    press(&app, Key::Return);
    assert!(app.handle_key(nova_ui::KeyCommand::Back));
    assert_eq!(*clears.borrow(), 1, "搜索页的 Esc 必须是清空语义");
}

#[test]
fn search_page_keeps_the_current_navbar_tab() {
    let app = setup();

    // 用户在媒体库切到第 2 个标签后进入搜索页：顶栏高亮必须沿用这个下标。
    app.set_home(&home_filled());
    let mut library = library_view();
    library.current_tab = 2;
    app.set_library(&library);
    app.set_search(&search_view());

    assert_eq!(app.ui().get_search_data().current_tab, 2);
}

#[test]
fn set_search_does_not_overwrite_text_while_input_is_focused() {
    let app = setup();

    app.set_search_text("正在输入的词");
    app.ui().set_search_focused(true);
    let mut late = search_view();
    late.term = "旧查询词".to_string();
    app.set_search(&late);

    assert_eq!(
        app.search_text(),
        "正在输入的词",
        "输入框聚焦时晚到的视图不能把文本回填成旧查询词"
    );
}

#[test]
fn set_search_backfills_text_when_input_is_not_focused() {
    let app = setup();

    app.set_search_text("旧文本");
    app.ui().set_search_focused(false);
    let mut late = search_view();
    late.term = "沙丘".to_string();
    app.set_search(&late);

    assert_eq!(
        app.search_text(),
        "沙丘",
        "未聚焦时视图里的查询词仍应回填搜索框"
    );
}

/// 点搜索页的「×」：清空关键词、**留在搜索页**、焦点回输入框。
///
/// 回归：以前「×」与 Esc 共用 `search-clear`，点了会直接把路由换成 Home
/// （`search` 也置空）——用户只是想改个词，却被踢回首页。
#[test]
fn clear_button_clears_the_term_but_stays_on_the_search_page() {
    let app = setup();
    // 真实入口都是 `open_search`：路由栈顶必须是搜索页，才谈得上"留在本页"。
    app.open_search(&search_view());
    assert_eq!(app.search_text(), "沙丘");

    // Esc 链路（清空 + 回首页）的替身：真实实现在 `Shell::clear_search` 里
    // 调 `show_home`。点「×」时它一次都不该被触发。
    let escapes = Rc::new(RefCell::new(0));
    {
        let escapes = escapes.clone();
        let weak = app.ui().as_weak();
        app.on_search_clear(move || {
            *escapes.borrow_mut() += 1;
            if let Some(ui) = weak.upgrade() {
                ui.set_page(Page::Home.index());
            }
        });
    }

    // 先聚焦输入框（真实点「×」时输入框本来就是焦点）。
    assert!(app.handle_key(nova_ui::KeyCommand::Search));
    assert!(app.target("search.clear").is_some(), "结果页应有「×」");
    app.click_target("search.clear").expect("点「×」");
    assert_eq!(*escapes.borrow(), 0, "「×」不能走 Esc 的清空+回首页链路");

    assert_eq!(app.page(), Page::Search, "清空关键词不该离开搜索页");
    assert_eq!(app.search_text(), "");
    assert!(
        app.ui().get_search_focused(),
        "清空后焦点应回到输入框（接着输入新词）"
    );
    // 结果也跟着清掉：空关键词不该继续显示上一轮的卡片。
    let view = app.search_view();
    assert!(view.cards.is_empty(), "结果卡应清空");
    assert_eq!(view.state, nova_ui::SearchState::Idle, "应回到 Idle 态");
    assert_eq!(view.term, "");
    // 「×」随之隐藏，就不能再出现在可点目标清单里（否则扫描会把它记成死按钮）。
    app.refresh_targets();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    assert!(
        app.target("search.clear").is_none(),
        "空关键词时「×」不可见，不应登记为目标"
    );
    // 路由仍然是搜索页（不是 Home），栈顶查询词也跟着清空。
    assert_eq!(
        app.current_route(),
        Some(nova_ui::Route::Search {
            term: String::new()
        })
    );
}

/// Esc 的行为不变：走 `search-clear`（Shell 侧清空 + 回首页），与「×」区分开。
#[test]
fn escape_still_takes_the_clear_and_home_path() {
    let app = setup();
    let homes = Rc::new(RefCell::new(0));
    {
        let homes = homes.clone();
        app.on_search_clear(move || *homes.borrow_mut() += 1);
    }
    assert!(app.handle_key(nova_ui::KeyCommand::Search));
    press(&app, Key::Escape);
    assert_eq!(
        *homes.borrow(),
        1,
        "Esc 必须走 search-clear（清空 + 回首页）"
    );
    assert_eq!(app.search_text(), "");
}
