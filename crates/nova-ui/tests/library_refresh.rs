// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 后台刷新替换媒体库视图模型时，用户的排序选择与焦点条目必须留下（R2-8）。
//!
//! 真机现象：停在媒体库页浏览时，启动期后台刷新把库视图整包换掉——条目数
//! 200 → 820 → 863，排序条跳回「最近添加」，键盘焦点落到的已经是另一条内容。
//! 修复后：排序由上层（Shell 的 `LibrarySort`）带进新视图，焦点按**条目 id**
//! 重新定位（`nova_ui::rebase_focus`），找不到才钳到范围内。

mod common;

use common::{init_backend, library_view};
use nova_ui::{App, AppWindow, CardView, KeyCommand};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn setup() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app
}

/// 焦点卡片的 item id（界面侧从 `library` 数据模型读，与用户看到的一致）。
fn focused_item(app: &App) -> String {
    let cards = app.ui().get_library().cards;
    let index = app.focus_index();
    cards
        .row_data(index)
        .map(|card| card.id.to_string())
        .unwrap_or_default()
}

#[test]
fn replacing_the_model_keeps_the_focus_item() {
    let app = setup();

    // 用户已经把排序切到「标题 A→Z」，并移动焦点到第二张卡（l02）。
    let mut old = library_view();
    old.sort_label = "排序：标题 A→Z".to_owned();
    app.open_library(&old);
    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 1, "一次右移应到第二张卡");
    assert_eq!(focused_item(&app), "l02");

    // 后台刷新补全数据：条目变多、顺序完全不同（「最近添加」按加入时间重排）。
    let mut next = old.clone();
    next.cards.reverse();
    next.cards
        .insert(0, CardView::new("n01", "刷新补进来的新片"));
    next.cards.push(CardView::new("n02", "又一条"));
    app.set_library(&next);

    // 排序文案由传入的视图决定（上层按 Shell 的 `LibrarySort` 重建）。
    assert_eq!(
        app.ui().get_library().sort_label.as_str(),
        "排序：标题 A→Z",
        "替换模型不能把排序选择重置成默认值"
    );
    // 焦点跟着**同一个条目**，而不是停在旧下标上。
    let expected = next
        .cards
        .iter()
        .position(|card| card.item_id == "l02")
        .expect("l02 还在新视图里");
    assert_ne!(expected, 1, "刷新后 l02 的位置变了，测试才有意义");
    assert_eq!(app.focus_index(), expected, "焦点应按 item id 重新定位");
    assert_eq!(app.ui().get_library_focus(), expected as i32);
    assert_eq!(focused_item(&app), "l02");

    // 键盘从新位置继续（不是从上一次键盘位置）：Left 落在视觉上的前一张。
    press(&app, Key::LeftArrow);
    assert_eq!(app.focus_index(), expected - 1);
    assert_eq!(focused_item(&app), next.cards[expected - 1].item_id);
}

#[test]
fn focus_falls_back_to_range_when_the_item_disappears() {
    let app = setup();

    let old = library_view();
    app.open_library(&old);
    for _ in 0..5 {
        press(&app, Key::RightArrow);
    }
    assert_eq!(app.focus_index(), 5);
    assert_eq!(focused_item(&app), "l06");

    // 刷新后这个条目没了，剩下的也更少：钳到最后一格，不能越界。
    let mut next = old.clone();
    next.cards.retain(|card| card.item_id != "l06");
    next.cards.truncate(4);
    app.set_library(&next);

    assert_eq!(app.focus_index(), 3, "条目消失 → 退回旧下标并钳到末位");
    assert_eq!(focused_item(&app), next.cards[3].item_id);
    // 键盘仍然可用（不会因为越界下标卡死）。
    press(&app, Key::RightArrow);
    assert_eq!(app.focus_index(), 3, "已在末位，右移不动");
    assert!(app.handle_key(KeyCommand::Back));
}
