// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 输入框抢走焦点之后，全局键盘导航必须能回到"有人处理按键"的状态（R2-1）。
//!
//! 真机 bug：只要搜索输入框获得过焦点，之后离开它的**非 Enter** 路径
//! （点结果卡、点「×」、点库标签、页内 Esc、换页）都没把焦点还给
//! `app.slint` 的全局 `keyboard` FocusScope。`TextInput` 一旦被隐藏，Slint 会把
//! 焦点整个丢掉（`process_key_input` 里"隐藏的元素失去焦点"），于是方向键 /
//! Esc / Enter 全部无响应，直到重启应用。
//!
//! 这些用例走真实事件路径（`dispatch_event` 的指针 / 按键），不直接设焦点属性：
//! 打字必须真的进输入框、点击必须真的点到目标、Esc 必须真的从窗口派发。

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::{detail_view, home_filled, init_backend, library_view, search_view};
use nova_ui::{App, AppWindow, KeyCommand, Page, TargetInfo};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

/// 修饰键字符：Slint 的修饰状态由 `key_codes` 的按下 / 抬起维护
/// （`Control` 就是 `\u{11}`，与 `global_shortcuts.rs` 同一套注入方式）。
const CONTROL: &str = "\u{11}";

fn settle(app: &App) {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
    }
}

fn setup() -> Rc<App> {
    init_backend();
    let app = Rc::new(App::new_with(AppWindow::new().expect("创建窗口")));
    app.ui().set_window_w(1280.0);
    app.ui().set_window_h(800.0);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.set_home(&home_filled());
    app.set_library(&library_view());
    app.set_search(&search_view());
    // `set_search` 会把视图里的查询词回填到搜索框；这些用例要自己敲字，
    // 先清空（程序化清空不触发 `search-changed`）。
    app.set_search_text("");
    app.set_detail(&detail_view());

    // ---- 上层回调的替身（真实实现在 `crates/nova-app/src/ui.rs`）----
    // Esc / 「×」的"清空查询"在真实应用里由上层同步视图并回首页；这里只保留
    // 与本测试相关的那一步（清词 + 展示首页），保证按键链路与真机一致。
    {
        let weak = Rc::downgrade(&app);
        app.on_search_clear(move || {
            if let Some(app) = weak.upgrade() {
                app.set_search_text("");
                app.show_home();
            }
        });
    }
    // 点顶栏库标签：真实应用会重建媒体库视图再切页（`Shell::activate_tab`）。
    {
        let weak = Rc::downgrade(&app);
        app.on_tab_activated(move |_id| {
            if let Some(app) = weak.upgrade() {
                let _ = app.open_library(&library_view());
            }
        });
    }
    // Ctrl+N：真实应用打开"添加服务器"对话框（纯界面操作）。
    {
        let weak = Rc::downgrade(&app);
        app.on_connect_server(move || {
            if let Some(app) = weak.upgrade() {
                app.open_add_server("", "");
            }
        });
    }

    app.show_home();
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    settle(&app);
    app
}

fn targets_hint(app: &App) -> Vec<String> {
    app.targets()
        .iter()
        .map(|target: &TargetInfo| target.id.clone())
        .collect()
}

/// 点一个已登记的目标（按 id，坐标取中心）；目标必须在清单里。
fn click(app: &App, id: &str) {
    settle(app);
    let target = app.target(id).unwrap_or_else(|| {
        panic!("清单里没有目标 {id}，当前有：{:?}", targets_hint(app));
    });
    let (x, y) = target.center();
    app.click_at(x, y).expect("派发指针点击");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
}

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

fn press(app: &App, key: Key) {
    let text: slint::SharedString = key.into();
    dispatch(app, text.as_str());
}

fn press_ctrl(app: &App, key: &str) {
    let window = app.ui().window();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: CONTROL.into(),
    });
    dispatch(app, key);
    window.dispatch_event(WindowEvent::KeyReleased {
        text: CONTROL.into(),
    });
}

/// 把焦点交给搜索页大输入框，并**确认字真的进了它**（否则后面的断言没有意义）。
fn focus_search_page_input(app: &App) {
    app.open_search(&search_view());
    app.set_search_text("");
    click(app, "search.input");
    type_text(app, "沙丘");
    assert_eq!(
        app.search_text(),
        "沙丘",
        "字必须真的进了搜索页输入框（否则这个用例没测到输入框抢焦点）"
    );
}

/// ① 搜索页打字 → 点结果卡 → Esc 必须能返回。
#[test]
fn typing_in_the_search_page_then_opening_a_result_keeps_escape_alive() {
    let app = setup();
    focus_search_page_input(&app);

    click(&app, "search.card.0");
    assert_eq!(app.page(), Page::Detail, "点结果卡应进详情页");

    press(&app, Key::Escape);
    assert_eq!(
        app.page(),
        Page::Search,
        "Esc 必须能返回搜索页：焦点要回到全局 keyboard FocusScope，\
         而不是留在已经隐藏的输入框里"
    );
}

/// ② 搜索页打字 → 点「×」→ Esc 回首页 → 方向键可用。
#[test]
fn clear_button_then_escape_returns_home_and_arrows_work_again() {
    let app = setup();
    focus_search_page_input(&app);

    click(&app, "search.clear");
    assert_eq!(app.search_text(), "", "「×」应清空关键词");
    assert_eq!(app.page(), Page::Search, "「×」只清词，不离开搜索页");

    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Home, "Esc = 清空并回首页（上层替身）");

    let before = app.focus_index();
    press(&app, Key::RightArrow);
    assert_ne!(
        app.focus_index(),
        before,
        "回首页后方向键必须可用（焦点归还给全局 FocusScope）"
    );
}

/// ③ 页内搜索框按 Esc 之后方向键可用。
#[test]
fn escape_in_the_page_search_input_hands_focus_back_to_the_global_scope() {
    let app = setup();
    focus_search_page_input(&app);

    press(&app, Key::Escape);
    assert_eq!(
        app.page(),
        Page::Home,
        "页内 Esc = 清空并回首页（上层替身）"
    );

    let before = app.focus_index();
    press(&app, Key::DownArrow);
    assert_ne!(
        app.focus_index(),
        before,
        "页内 Esc 之后方向键必须可用（不能只剩一个已失焦的输入框）"
    );
}

/// ④ 点顶栏库标签离开搜索页之后方向键可用。
#[test]
fn clicking_a_navbar_tab_while_typing_restores_global_keys() {
    let app = setup();
    focus_search_page_input(&app);

    click(&app, "search.nav.tab.0");
    assert_eq!(app.page(), Page::Library, "点库标签应切到媒体库");

    let before = app.focus_index();
    press(&app, Key::RightArrow);
    assert_ne!(
        app.focus_index(),
        before,
        "换标签之后方向键必须可用（输入框随页面隐藏时不能带走键盘）"
    );
}

/// ⑤ Ctrl+N 打开对话框 → Esc 关闭 → 方向键可用。
#[test]
fn closing_the_add_server_dialog_restores_global_keys() {
    let app = setup();

    // 先在顶栏搜索框里制造"输入框抢过焦点"的状态（真实点击 + 真实按键）。
    click(&app, "home.nav.search");
    type_text(&app, "沙丘");
    assert_eq!(app.search_text(), "沙丘", "字必须真的进了顶栏输入框");

    // 输入框里的 Ctrl+N 由输入框自己上报（`global-shortcut(8)`）。
    press_ctrl(&app, "n");
    assert!(app.add_server_open(), "Ctrl+N 应打开添加服务器对话框");
    assert_eq!(
        app.ui().get_add_server_focus(),
        1,
        "打开对话框时焦点应在地址输入框"
    );

    press(&app, Key::Escape);
    assert!(!app.add_server_open(), "地址框里的 Esc 应关闭对话框");

    let before = app.focus_index();
    press(&app, Key::RightArrow);
    assert_ne!(
        app.focus_index(),
        before,
        "关框之后方向键必须可用（焦点还给全局 FocusScope）"
    );
}

/// ⑥ 同页点击非输入控件（排序条）也要还焦。
///
/// 这是 ①–⑤ 之外最容易漏的形状：页面没变，输入框也还在，只是用户用鼠标
/// 干了别的事。方向键必须从"点过的那一下"之后继续可用。
#[test]
fn clicking_a_non_input_control_while_typing_restores_global_keys() {
    let app = setup();
    app.open_library(&library_view());
    click(&app, "library.nav.search");
    type_text(&app, "沙丘");
    assert_eq!(app.search_text(), "沙丘", "字必须真的进了顶栏输入框");

    settle(&app);
    click(&app, "library.sort");
    assert_eq!(app.page(), Page::Library, "排序是同一页上的动作");

    let before = app.focus_index();
    press(&app, Key::DownArrow);
    assert_ne!(
        app.focus_index(),
        before,
        "点了非输入控件之后方向键必须可用（页面没变也不能留在输入框里）"
    );
}

/// ⑦ 页面没变的重建（异步搜索结果到达）不能把焦点从正在打字的输入框抢走。
///
/// 真实应用里每次异步结果到达都会 `open_search(&view)`（页面还是搜索页）。
/// "换页 → 还焦"只在页面**真的**变了时执行；否则每次结果到达都会把焦点
/// 抢回全局键盘，后续字符全部丢失（实测调试通道 `search page:true` 之后输入无效）。
#[test]
fn late_search_view_update_does_not_steal_focus_from_the_page_input() {
    let app = setup();
    focus_search_page_input(&app);

    // 异步结果到达：上层只重建视图（`open_search`），不会再次聚焦输入框。
    app.open_search(&search_view());
    assert_eq!(app.page(), Page::Search, "页面没变：仍是搜索页");

    type_text(&app, "子");
    assert_eq!(
        app.search_text(),
        "沙丘子",
        "页面没变的视图重建之后，字符必须仍然进输入框（不能把焦点抢回全局键盘）"
    );
}

/// 兜底：Ctrl+F 聚焦之后立刻用键盘意图返回，方向键仍可用（对照组，修复前也过）。
#[test]
fn ctrl_f_then_keyboard_navigation_still_works() {
    let app = setup();
    assert!(app.handle_key(KeyCommand::Search));
    press(&app, Key::Escape);
    let before = app.focus_index();
    press(&app, Key::RightArrow);
    assert_ne!(app.focus_index(), before);
}
