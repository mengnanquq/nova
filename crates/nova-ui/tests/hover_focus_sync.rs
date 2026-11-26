// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 悬停某张卡之后，方向键必须**从悬停的那张卡继续**（R2-3）。
//!
//! 回归背景：媒体库 / 搜索页的 `PosterGrid` 悬停时已经把 `focus-index` 设成
//! 悬停卡，但 Rust 侧的 `move_focus_by` 读的是自己的 `state.focus`（上一次键盘
//! 位置），于是 hover `library.card.4` 之后按 Right，焦点从 0 变成 1 而不是 5。
//! 首页行更彻底：悬停根本没同步焦点，方向键总是从旧的键盘位置继续。

mod common;

use std::time::Duration;

use common::{home_filled, init_backend, library_view};
use nova_ui::{App, AppWindow, Page, TargetInfo};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

fn settle(app: &App) {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    }
}

fn move_pointer(app: &App, x: f32, y: f32) {
    app.ui()
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        })
        .expect("派发 PointerMoved");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
}

fn hover_target(app: &App, id: &str) {
    settle(app);
    let target = app.target(id).unwrap_or_else(|| {
        panic!(
            "清单里没有目标 {id}，当前有：{:?}",
            app.targets()
                .iter()
                .map(|target: &TargetInfo| target.id.clone())
                .collect::<Vec<_>>()
        );
    });
    let (x, y) = target.center();
    move_pointer(app, x, y);
}

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn app_1280x800() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(1280.0);
    app.ui().set_window_h(800.0);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app
}

/// 媒体库：悬停第 5 张卡（index 4）后按 Right → 焦点应是 5。
#[test]
fn library_arrow_navigation_continues_from_the_hovered_card() {
    let app = app_1280x800();
    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    app.ui().set_library_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    hover_target(&app, "library.card.4");
    assert_eq!(
        app.ui().get_library_focus(),
        4,
        "悬停应把 UI 侧的焦点索引移到第 5 张卡（`PosterGrid` 已这么做）"
    );

    press(&app, Key::RightArrow);
    assert_eq!(
        app.focus_index(),
        5,
        "Right 应从悬停的卡（4）继续到 5，而不是从上一次键盘位置（0）+1"
    );
}

/// 首页行：悬停「继续观看」第 3 张卡后按 Right → 焦点应是 3。
#[test]
fn home_arrow_navigation_continues_from_the_hovered_card() {
    let app = app_1280x800();
    app.set_home(&home_filled());
    // 压缩 billboard，让行区进入首屏（与 `hover_and_wheel.rs` 同一做法）。
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    hover_target(&app, "home.row.continue.card.2");
    assert_eq!(
        app.ui().get_home_focus(),
        2,
        "首页行悬停也必须把键盘焦点移到悬停卡（否则方向键从旧位置继续）"
    );

    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 3, "Right 应从悬停的卡（2）继续到 3");
}

// ---- 放大覆盖层接管命中区之后的悬停（第三轮体检 R2-3 真机回归）----

/// 放大覆盖层出现需要 400ms 延迟 + 200ms 动画；等 1200ms 保证它已经接管命中区
/// （`CardZoom` 的 `interactive` 在 `zoom-factor` 走到 80% 之后才打开）。
fn wait_for_zoom() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
}

/// 首页：悬停一张卡（放大覆盖层接管）→ 键盘移走 → **再**悬停同一张卡。
///
/// 真机复现路径：`hover home.row.continue.card.1` 之后指针就停在那张卡上，覆盖层
/// 随后接管命中区；再 `hover` 同一张卡时 `has-hover` 没有跳变，底卡的
/// `hover-changed` 不再触发，键盘焦点不会被对齐——于是 `Right` 从"上一次键盘
/// 位置 +1"。本用例的初始焦点（0）与悬停卡（1）不同，并且悬停发生在放大层之上。
#[test]
fn home_rehovering_the_zoomed_card_resyncs_the_keyboard_focus() {
    let app = app_1280x800();
    app.set_home(&home_filled());
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    hover_target(&app, "home.row.continue.card.1");
    wait_for_zoom();
    assert!(
        app.ui().get_row0_zoom_on(),
        "悬停后放大覆盖层必须已经打开（否则本用例没经过覆盖层）"
    );
    assert_eq!(app.ui().get_home_focus(), 1, "首次悬停应对齐焦点");

    // 键盘把焦点移走：覆盖层仍然留在悬停卡上（悬停优先），命中区归覆盖层。
    press(&app, Key::RightArrow);
    press(&app, Key::RightArrow);
    assert_eq!(app.ui().get_home_focus(), 3, "键盘应正常前进到 3");

    // 再悬停同一张卡（坐标重新解算：键盘移动可能把行滚过）。
    hover_target(&app, "home.row.continue.card.1");
    assert_eq!(
        app.ui().get_home_focus(),
        1,
        "悬停放大覆盖层也必须把键盘焦点对齐到这张卡（R2-3：不能留在旧位置 3）"
    );

    press(&app, Key::RightArrow);
    assert_eq!(
        app.focus_index(),
        2,
        "Right 必须从悬停卡（1）继续到 2，而不是从旧键盘位置（3）+1"
    );
}

/// 媒体库网格同一场景（同一套 `CardZoom` 覆盖层）。
#[test]
fn library_rehovering_the_zoomed_card_resyncs_the_keyboard_focus() {
    let app = app_1280x800();
    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    app.ui().set_library_focus(0);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    hover_target(&app, "library.card.4");
    wait_for_zoom();
    assert_eq!(app.ui().get_library_focus(), 4, "首次悬停应对齐焦点");

    // 6 列网格：Right 到行末（5），再 Down 到第二行（11）。
    press(&app, Key::RightArrow);
    press(&app, Key::DownArrow);
    assert_eq!(app.ui().get_library_focus(), 11, "键盘应正常走到第二行末");

    hover_target(&app, "library.card.4");
    assert_eq!(
        app.ui().get_library_focus(),
        4,
        "悬停放大覆盖层也必须把键盘焦点对齐到这张卡（R2-3）"
    );

    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 5, "Right 必须从悬停卡（4）继续到 5");
}
