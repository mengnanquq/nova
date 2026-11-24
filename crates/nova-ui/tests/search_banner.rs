// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 搜索框与横幅的交互测试：testing backend 发真实键 / 鼠标事件。
//!
//! 覆盖：Ctrl+F 聚焦、输入触发 `search-changed`、Esc 退出聚焦、Enter 提交、
//! 横幅动作按钮触发回调。

mod common;

use common::{home_filled, home_image_ids, init_backend, install_posters};
use nova_ui::{App, AppWindow, BannerData, BannerLevel, KeyCommand};
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use std::cell::RefCell;
use std::rc::Rc;

fn setup() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.show_home();
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
fn ctrl_f_focuses_and_typing_emits_changed() {
    let app = setup();

    // Ctrl+F 聚焦（走 Rust 的 KeyCommand 入口，与真实键事件同一路径）。
    assert!(!app.ui().get_search_focused());
    assert!(app.handle_key(KeyCommand::Search));
    assert!(app.ui().get_search_focused());

    // 输入逐字符触发 search-changed，参数就是当前文本。
    let changes = Rc::new(RefCell::new(Vec::new()));
    {
        let changes = changes.clone();
        app.on_search_changed(move |text| {
            changes.borrow_mut().push(text.to_string());
        });
    }
    type_text(&app, "沙丘");
    assert_eq!(
        changes.borrow().as_slice(),
        ["沙", "沙丘"],
        "每次编辑都应发一次当前文本"
    );
    assert_eq!(app.search_text(), "沙丘");
}

#[test]
fn escape_blurs_search_input() {
    let app = setup();

    assert!(app.handle_key(KeyCommand::Search));
    assert!(app.ui().get_search_focused());

    // Esc：退出聚焦（焦点还给全局键盘处理，文本保留）。
    press(&app, Key::Escape);
    assert!(!app.ui().get_search_focused());
    assert_eq!(app.search_text(), "");
}

#[test]
fn enter_submits_search() {
    let app = setup();

    assert!(app.handle_key(KeyCommand::Search));
    let submits = Rc::new(RefCell::new(0));
    {
        let submits = submits.clone();
        app.on_search_submit(move || {
            *submits.borrow_mut() += 1;
        });
    }
    type_text(&app, "dune");
    press(&app, Key::Return);
    assert_eq!(*submits.borrow(), 1, "Enter 应触发一次提交");
}

#[test]
fn banner_action_button_fires_callback() {
    let app = setup();

    let banner = BannerData::new("登录已过期：重新运行 nova login", BannerLevel::Warning)
        .with_action("重新登录");
    app.set_banner(Some(banner));
    assert!(app.ui().get_banner_visible());
    assert_eq!(app.ui().get_banner_level(), 1);
    assert_eq!(app.ui().get_banner_action_label(), "重新登录");

    let clicks = Rc::new(RefCell::new(0));
    {
        let clicks = clicks.clone();
        app.on_banner_action(move || {
            *clicks.borrow_mut() += 1;
        });
    }

    // 横幅贴顶（0..48px），动作按钮贴右侧（gutter 48px 内缩）。
    let window = app.ui().window();
    let position = slint::LogicalPosition::new(1180.0, 24.0);
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    assert_eq!(*clicks.borrow(), 1, "点击横幅动作按钮应触发回调");

    // 清掉横幅后不可见。
    app.set_banner(None);
    assert!(!app.ui().get_banner_visible());
}
