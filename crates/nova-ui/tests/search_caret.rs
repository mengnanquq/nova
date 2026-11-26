// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 顶栏搜索框回填了旧查询词时，点开后**立即**打字的首字符位置（体检 R3-1）。
//!
//! 真机 bug：顶栏输入框的文本是常驻的（`search-text` 与 AppWindow 双向同步，
//! 回首页也保留旧词）。点放大镜展开输入框时，`TextInput` 的光标停在程序化回填
//! 时的旧位置（通常是 0），于是"点开就打字"的首字符会插到旧词前面：
//! 旧词 `沙丘` + 输入 `1234567890` → `1沙丘234567890`；之后第一个字符触发
//! 顶栏 → 搜索页的焦点交接（`SearchPage.focus-input` 把光标推到末尾），
//! 余下字符才追加到末尾。等待 0.7s 也一样（光标不会自己跑到末尾）。
//!
//! 修法在 `navbar.slint` 的 `focus-input()`：与搜索页大输入框同一口径，交接时
//! 把光标与锚点一起推到末尾（已经在本框里打字时不碰光标，中间改字仍不被推走，
//! 见 `search_handoff.rs`）。
//!
//! 四个象限（空词 / 旧词 × 立即 / 延迟）= 旧词两格是本修复的回归断言，
//! 空词两格是对照组（修复前也过）。

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::{home_filled, init_backend};
use nova_ui::{App, AppWindow, Page, SearchView, TargetInfo};
use slint::ComponentHandle;

/// 目标值与键入文本。
const OLD_TERM: &str = "沙丘";
const TYPED: &str = "1234567890";

fn settle(app: &App) {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
    }
}

/// 首页 + 旧查询词（可空），回调链与 `nova-app` 的 `Shell` 同步路径一致：
/// 第一次编辑走 `begin_search` 的本地结果 → `show_search(view, true)`。
fn setup(term: &str) -> Rc<App> {
    init_backend();
    let app = Rc::new(App::new_with(AppWindow::new().expect("创建窗口")));
    app.ui().set_window_w(1280.0);
    app.ui().set_window_h(800.0);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.set_home(&home_filled());
    app.show_page(Page::Home);
    if !term.is_empty() {
        // 程序化回填：搜索框显示旧词（真实应用在上一轮搜索结束后就是这样）。
        app.set_search(&SearchView::from_cards(term, Vec::new()));
    }
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
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    settle(&app);
    app
}

/// 与 `Shell::begin_search` 的同步路径同构：本地结果 → 打开搜索页 → 焦点交接。
fn handoff(app: &App, text: &str) {
    let view = SearchView::from_cards(text, Vec::new());
    app.open_search(&view);
    app.ui().invoke_focus_search_input();
}

fn click(app: &App, id: &str) {
    let target: TargetInfo = app.target(id).unwrap_or_else(|| {
        panic!(
            "清单里没有目标 {id}，当前有：{:?}",
            app.targets()
                .iter()
                .map(|target| target.id.clone())
                .collect::<Vec<_>>()
        );
    });
    let (x, y) = target.center();
    app.click_at(x, y).expect("派发指针点击");
}

fn type_chars(app: &App, text: &str) {
    for ch in text.chars() {
        app.press_key(ch.to_string().into()).expect("按键");
    }
}

/// 点开放大镜 →（可选等待）→ 逐字输入 → 断言完整查询词与页面。
fn open_and_type(term: &str, delay: Duration) -> String {
    let app = setup(term);
    click(&app, "home.nav.search");
    assert!(
        app.ui().get_search_focused(),
        "点放大镜应展开顶栏输入框并让它拿到焦点"
    );
    if !delay.is_zero() {
        i_slint_backend_testing::mock_elapsed_time(delay);
    }
    type_chars(&app, TYPED);
    // 第一个字符就切到搜索页（交接路径真的跑过），余下字符进页内大输入框。
    assert_eq!(
        app.page(),
        Page::Search,
        "第一个字符就该切到搜索页（交接路径必须真的跑过）"
    );
    app.search_text()
}

#[test]
fn old_term_then_immediate_typing_appends_at_the_end() {
    let text = open_and_type(OLD_TERM, Duration::ZERO);
    assert_eq!(
        text,
        format!("{OLD_TERM}{TYPED}"),
        "旧词回填后立即打字：首字符必须追加在末尾，而不是插到旧词前面（R3-1）"
    );
}

#[test]
fn old_term_then_delayed_typing_appends_at_the_end() {
    let text = open_and_type(OLD_TERM, Duration::from_millis(700));
    assert_eq!(
        text,
        format!("{OLD_TERM}{TYPED}"),
        "旧词回填 + 等 0.7s 再打字，也必须追加在末尾（R3-1）"
    );
}

#[test]
fn empty_term_then_immediate_typing_keeps_the_order() {
    let text = open_and_type("", Duration::ZERO);
    assert_eq!(text, TYPED, "空词的对照组：顺序本来就正确");
}

#[test]
fn empty_term_then_delayed_typing_keeps_the_order() {
    let text = open_and_type("", Duration::from_millis(700));
    assert_eq!(text, TYPED, "空词 + 延迟的对照组：顺序本来就正确");
}
