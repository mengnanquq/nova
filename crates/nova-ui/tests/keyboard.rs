// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 键盘导航：用 testing backend 发真实键事件，断言焦点索引与导航栈。

mod common;

use common::{home_filled, home_image_ids, init_backend, install_posters, library_view};
use nova_ui::{App, AppWindow, KeyCommand, Page, Route};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

#[test]
fn keyboard_moves_focus_and_navigates() {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.show_home();
    app.ui().set_home_focus(0);

    // 继续观看 5 张 + 最近添加 8 张（6 列 → 6 + 2）。
    press(&app, Key::RightArrow);
    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 2, "两次右移应到第 3 张继续观看");

    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 7, "下移回到最近添加第 1 行第 3 列");

    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 8, "最近添加行内右移");

    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 12, "第二行只有 2 张，列位置夹到最后一格");

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_index(), 6, "上移回到第一行同列");

    // Enter 打开详情：压栈 + 切页。
    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Detail);
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "r2".to_string()
        })
    );
    assert_eq!(app.navigator().len(), 2);

    // Esc 返回首页。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home);
    assert_eq!(app.current_route(), Some(Route::Home));
    assert_eq!(app.focus_index(), 6, "返回后焦点保留");

    // 栈空时 Esc 无动作。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home);
    assert_eq!(app.navigator().len(), 1);

    // 媒体库网格：14 张、6 列。
    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    app.ui().set_library_focus(0);
    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 1);
    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 7);
    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Detail);
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "l08".to_string()
        })
    );

    // Esc 回到媒体库（而不是首页）。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Library);
    assert!(matches!(app.current_route(), Some(Route::Library { .. })));

    // Ctrl+F 的意图走同一个 Rust 入口。
    assert!(app.handle_key(KeyCommand::Search));
    assert!(app.ui().get_search_focused());
}
