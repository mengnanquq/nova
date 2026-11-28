// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 键盘导航：用 testing backend 发真实键事件，断言焦点索引与导航栈。

mod common;

use common::{
    detail_view, home_filled, home_image_ids, init_backend, install_posters, library_view,
};
use nova_ui::{App, AppWindow, KeyCommand, Page, Route};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

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

    // 焦点行 = 视觉行：继续观看 5 张 + 最近添加 8 张 + 库行（电影 4 / 剧集 3），
    // 扁平下标 0..4 是继续观看、5..12 是最近添加、13..16 是「电影」、17.. 是「剧集」。
    press(&app, Key::RightArrow);
    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 2, "两次右移应到第 3 张继续观看");

    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 7, "下移进最近添加的同一列（第 3 张）");

    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 8, "最近添加行内右移");

    // Down 是换行（不再在最近添加这一行里横跳）：进「电影」行，列号夹到该行卡片数。
    press(&app, Key::DownArrow);
    assert_eq!(app.focus_index(), 16, "换行到「电影」行，列位置夹到第 4 张");

    press(&app, Key::UpArrow);
    assert_eq!(app.focus_index(), 8, "上移回最近添加同列");

    // Enter 打开详情：压栈 + 切页。
    press(&app, Key::Return);
    assert_eq!(app.page(), Page::Detail);
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "r4".to_string()
        })
    );
    assert_eq!(app.navigator().len(), 2);

    // Esc 返回首页。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home);
    assert_eq!(app.current_route(), Some(Route::Home));
    assert_eq!(app.focus_index(), 8, "返回后焦点保留");

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

/// 详情页的版本下拉展开时，Esc 先收下拉，再按一次才返回上一页。
///
/// 回归：下拉展开（光标在版本列表里）按 Esc 直接退回首页，下拉状态留在原地，
/// 用户还得回头再关一次——两件事挤在一个按键上。
#[test]
fn escape_closes_the_source_dropdown_before_leaving_detail() {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    app.set_detail(&detail_view());
    app.open_detail("dune");
    assert_eq!(app.page(), Page::Detail);
    app.ui().show().expect("显示窗口");

    // 展开版本下拉（点击入口，与用户操作同一条路径）。
    // 入口在条件子树里（`sources.length > 0`，R9-A2），靠 `TargetBus` 代次登记：
    // 先让 item tree 实例化、再推代次才能看到它（与 `interaction_targets` 的
    // `settle` 同一口径）。
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    let drop = app.target("detail.drop").expect("版本下拉入口没登记");
    app.click_at(drop.center().0, drop.center().1)
        .expect("点击版本下拉");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
    assert!(app.ui().get_detail_drop_open(), "下拉应当已展开");

    // 第一次 Esc：只关下拉，页面不动。
    press(&app, Key::Escape);
    assert!(!app.ui().get_detail_drop_open(), "Esc 应当先收起版本下拉");
    assert_eq!(app.page(), Page::Detail, "第一次 Esc 不该离开详情页");

    // 第二次 Esc：正常返回。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Library, "第二次 Esc 才返回上一页");
}

/// R9-A2（真机实测）：剧集详情没有播放按钮（选集未做），键盘焦点从「我的列表」
/// 开始（不能落在不存在的播放按钮上），Enter 激活「我的列表」而不是起播。
#[test]
fn series_detail_focus_starts_on_my_list_and_enter_does_not_play() {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    let mut detail = detail_view();
    detail.playable = false;
    detail.sources.clear();
    app.set_detail(&detail);
    app.open_detail("dune");
    assert_eq!(app.page(), Page::Detail);
    app.ui().show().expect("显示窗口");

    // 进详情默认焦点 0：这里就是「我的列表」。
    assert_eq!(app.ui().get_detail_focus(), 0, "焦点应从第一个可用按钮开始");
    // 单项行：方向键不动，焦点不会落到不存在的按钮上。
    press(&app, Key::RightArrow);
    press(&app, Key::LeftArrow);
    press(&app, Key::DownArrow);
    press(&app, Key::UpArrow);
    assert_eq!(
        app.ui().get_detail_focus(),
        0,
        "唯一可用控件上方向键不该移动焦点"
    );
    assert_eq!(app.page(), Page::Detail, "方向键不该离开详情页");

    // Enter 激活「我的列表」，不去起播：页面留在详情页、路由不进播放页。
    let toggled = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = toggled.clone();
    app.on_detail_my_list_toggle(move || flag.set(true));
    press(&app, Key::Return);
    assert!(toggled.get(), "Enter 应激活「我的列表」");
    assert_eq!(app.page(), Page::Detail, "Enter 不该离开详情页");
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "dune".to_string()
        }),
        "Enter 不该压入播放路由"
    );
}

/// R9-D1（真机 2/2 复现）：剧集详情按 `Enter` 切「我的列表」时，handler 会**同步**
/// 回到 `nova-app`（`Shell::toggle_detail_mylist` → 写本地库 → `App::set_detail`），
/// 那里要 `state.borrow_mut()`。修复前 `play_current` 把 `state.borrow()` 写在
/// if-let 的 scrutinee 里（`if let Some(Route::Detail { .. }) = state.borrow()...`），
/// 临时 `Ref` 活到整块结束，块内回调再 `borrow_mut()` 就是
/// `RefCell already borrowed`（整个进程退出）。
///
/// 这里让 handler **真的**调 `set_detail`：上一条用例的 handler 只置一个标志，
/// 盖不到这条路径。
#[test]
fn series_detail_enter_my_list_handler_reentering_state_does_not_panic() {
    init_backend();
    let app = Rc::new(App::new_with(AppWindow::new().unwrap()));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    let mut detail = detail_view();
    detail.playable = false;
    detail.sources.clear();
    app.set_detail(&detail);
    app.open_detail("dune");
    assert_eq!(app.page(), Page::Detail);
    app.ui().show().expect("显示窗口");
    assert!(
        !app.ui().get_detail_in_my_list(),
        "起始不该在「我的列表」里"
    );

    let calls = Rc::new(Cell::new(0));
    let current = Rc::new(RefCell::new(detail.clone()));
    {
        let weak = Rc::downgrade(&app);
        let calls = Rc::clone(&calls);
        let current = Rc::clone(&current);
        app.on_detail_my_list_toggle(move || {
            calls.set(calls.get() + 1);
            let Some(app) = weak.upgrade() else {
                return;
            };
            // 与 `Shell::toggle_detail_mylist` 同构：改视图模型，再重贴详情。
            // 重贴这一步在修复前正好撞上 if-let scrutinee 还活着的临时 `Ref`。
            let mut view = current.borrow().clone();
            view.in_my_list = !view.in_my_list;
            *current.borrow_mut() = view.clone();
            app.set_detail(&view);
        });
    }

    // 第一下：切进「我的列表」，不崩、不走播放页。
    press(&app, Key::Return);
    assert_eq!(calls.get(), 1, "Enter 应激活「我的列表」一次");
    assert!(
        app.ui().get_detail_in_my_list(),
        "handler 里的 set_detail 应当落地"
    );
    assert_eq!(app.page(), Page::Detail, "Enter 不该离开详情页");
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "dune".to_string()
        }),
        "Enter 不该压入播放路由"
    );

    // 第二下：切回原状（真机验收要求的两下）。
    press(&app, Key::Return);
    assert_eq!(calls.get(), 2, "再按一次应再激活一次");
    assert!(!app.ui().get_detail_in_my_list(), "再按一次应恢复原状");
}
