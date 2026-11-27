// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 搜索页的交互接线：页内大输入框 → Rust 回调、Esc 清空、焦点入口。
//!
//! 几何由 `search_layout.rs` 断言；这里只验证"按键/回调有没有接上"，
//! 用 testing backend 发真实键事件（与 `search_banner.rs` 同一套做法）。

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{home_filled, init_backend, library_view, search_empty, search_view};
use nova_ui::{App, AppWindow, CardView, FocusLayer, KeyCommand, Page, SearchView};
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

/// 搜索页不属于任何库：三个入口进去后顶栏都不应有选中标签（R6-4）。
///
/// 修复前 `nav_current_tab()` 的 Search 分支返回“上一次进的库”，从「动画电影」
/// Ctrl+F 进搜索后顶栏仍亮着「动画电影」，看着像搜索结果属于那个库。
#[test]
fn search_page_has_no_highlighted_navbar_tab() {
    let app = std::rc::Rc::new(setup());

    // 上层替身：输入即打开搜索页（与 `Shell::begin_search` 同一条链路）。
    {
        let weak = std::rc::Rc::downgrade(&app);
        app.on_search_changed(move |text| {
            if let Some(app) = weak.upgrade() {
                let view = SearchView::from_cards(text, vec![CardView::new("s1", "沙丘")]);
                app.open_search(&view);
            }
        });
    }

    // 入口 1：首页 Ctrl+F → 输入。
    app.set_home(&home_filled());
    app.show_home();
    assert!(app.handle_key(KeyCommand::Search));
    type_text(&app, "沙");
    assert_eq!(app.page(), Page::Search);
    assert_eq!(
        app.ui().get_search_data().current_tab,
        -1,
        "首页进搜索：顶栏不能高亮任何库标签"
    );

    // 入口 2：媒体库（当前是第 2 个标签）Ctrl+F → 输入。
    let mut library = library_view();
    library.current_tab = 2;
    app.set_library(&library);
    app.open_library(&library);
    assert!(app.handle_key(KeyCommand::Search));
    type_text(&app, "丘");
    assert_eq!(app.page(), Page::Search);
    assert_eq!(
        app.ui().get_search_data().current_tab,
        -1,
        "库页进搜索：顶栏不能保留未离开前的高亮"
    );

    // 离开搜索回到库页：库标签高亮恢复成它自己的当前标签。
    assert!(app.go_back());
    assert_eq!(app.page(), Page::Library);
    assert_eq!(
        app.ui().get_library().current_tab,
        2,
        "回到库页后高亮要恢复"
    );

    // 入口 3：顶栏搜索按钮 → 输入。
    app.show_home();
    app.ui().invoke_search();
    type_text(&app, "沙");
    assert_eq!(app.page(), Page::Search);
    assert_eq!(
        app.ui().get_search_data().current_tab,
        -1,
        "顶栏搜索按钮进搜索：顶栏不能高亮任何库标签"
    );

    // 高亮为空，但键盘仍有合理起点：焦点链是 结果网格 → 页内搜索框 → 顶栏
    // （R7-D1），所以从内容层按两下 ↑ 到顶栏，焦点落在第 0 个标签
    // （不是“无高亮”就等于“顶栏不可聚焦”）。
    press(&app, Key::UpArrow);
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Content,
        "第一下 ↑ 先回页内搜索框（不是直接跳到顶栏）"
    );
    assert!(app.ui().get_search_focused(), "焦点应在页内搜索框里");
    press(&app, Key::UpArrow);
    assert_eq!(app.focus_layer(), FocusLayer::Navbar);
    assert_eq!(app.nav_focus(), 0, "无高亮时顶栏焦点从第一个标签开始");
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

/// R7-D1：页内搜索框不能把方向键吞掉。
///
/// 修复前：到搜索页时大输入框持有焦点，`TextInput` 把 ↓ / ↑ 当编辑键吃掉（事件冒泡
/// 到不了兄弟节点 `keyboard`），结果网格完全无法用键盘进入——必须先按 Tab / Esc
/// 手动释放焦点。
///
/// 期望（与 `docs/ui-architecture.md` 的搜索页键盘约定一致）：
/// - 输入框里 ↓ → 焦点交给结果网格的第一张卡；
/// - 网格第一行 ↑ → 回到搜索框；
/// - 搜索框里 ↑ → 进顶栏（标签仍可达）。
#[test]
fn page_input_hands_arrow_keys_to_the_result_grid() {
    let app = setup();
    // 搜索页的顶栏沿用首页的标签：进顶栏那一步需要它们（`nav_slots`）。
    app.set_home(&home_filled());
    let view = search_view();
    app.set_search(&view);
    app.open_search(&view);
    app.set_search_text("沙丘");
    assert!(app.handle_key(nova_ui::KeyCommand::Search));
    assert!(app.ui().get_search_focused(), "前提：搜索框持有焦点");
    assert_eq!(app.ui().get_search_focus(), 0);

    // ↓：焦点必须是「网格第一张」，并且真的交给了全局键盘（`nav_active` 为真说明
    // 这个键走到了 `keyboard` 的 `key-pressed`，而不是被输入框吃掉）。
    press(&app, Key::DownArrow);
    app.refresh_targets();
    assert!(
        !app.ui().get_search_focused(),
        "↓ 之后搜索框必须让出焦点（否则方向键仍然被它吞掉）"
    );
    assert_eq!(app.ui().get_search_focus(), 0, "焦点应停在结果网格第一张卡");
    assert!(
        app.ui().get_nav_active(),
        "↓ 必须真的到达全局键盘（nav_active 由键盘处理器置真）"
    );
    assert_eq!(app.focus_layer(), FocusLayer::Content);

    // 再从第一行 ↑：回到搜索框，而不是"没反应"或被顶栏截走。
    press(&app, Key::UpArrow);
    assert!(
        app.ui().get_search_focused(),
        "网格第一行 ↑ 应把焦点交回页内搜索框"
    );
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Content,
        "输入框不是 chrome 层"
    );

    // 搜索框里 ↑：进顶栏（标签仍可达）。
    press(&app, Key::UpArrow);
    assert_eq!(
        app.focus_layer(),
        FocusLayer::Navbar,
        "搜索框 ↑ 应进顶栏（否则搜索页的标签键盘不可达）"
    );
    assert!(!app.ui().get_search_focused(), "进顶栏后输入框不再持有焦点");
}

/// 空结果时 ↓ 在输入框里是**无操作**（Netflix 同款）：焦点留在输入框，
/// 不给出额外反馈。`docs/ui-architecture.md` 的搜索页键位表里写明了这条约定。
#[test]
fn down_with_no_results_keeps_the_focus_in_the_input() {
    let app = setup();
    app.set_home(&home_filled());
    let view = search_empty();
    app.set_search(&view);
    app.open_search(&view);
    assert!(app.handle_key(KeyCommand::Search));
    assert!(app.ui().get_search_focused(), "前提：搜索框持有焦点");

    press(&app, Key::DownArrow);
    assert!(
        app.ui().get_search_focused(),
        "空结果时 ↓ 不能把焦点交给空网格（应留在输入框）"
    );
    assert_eq!(app.focus_layer(), FocusLayer::Content);
}

/// 网格第 N 张 ↑ 回输入框，再 ↓ 必须回到第 N 张（不是第 0 张）。
///
/// 与首页「上下移行保持列」同口径：离开网格时记住位置，回来时恢复；
/// 查询词变了（结果已经换了一批）则回到第 0 张。
#[test]
fn grid_up_then_down_returns_to_the_same_card() {
    let app = setup();
    app.set_home(&home_filled());
    let view = search_view();
    app.set_search(&view);
    app.open_search(&view);
    assert!(app.handle_key(KeyCommand::Search));

    // ↓ 进网格，再行内右移 3 格。
    press(&app, Key::DownArrow);
    for _ in 0..3 {
        press(&app, Key::RightArrow);
    }
    assert_eq!(app.ui().get_search_focus(), 3, "前提：焦点在第 4 张卡");

    // 网格首行 ↑ 回输入框。
    press(&app, Key::UpArrow);
    assert!(app.ui().get_search_focused(), "前提：焦点回到输入框");

    // 再 ↓：回到第 3 张，而不是第 0 张。
    press(&app, Key::DownArrow);
    assert!(!app.ui().get_search_focused(), "↓ 应把焦点交给结果网格");
    assert_eq!(
        app.ui().get_search_focus(),
        3,
        "回到网格时应恢复离开时的位置（不是第 0 张）"
    );

    // 查询词变了：旧下标作废，回到第 0 张。
    press(&app, Key::UpArrow);
    assert!(app.ui().get_search_focused(), "前提：焦点回到输入框");
    let mut other = search_view();
    other.term = "沙丘：第二部".to_owned();
    app.set_search(&other);
    press(&app, Key::DownArrow);
    assert!(!app.ui().get_search_focused(), "↓ 应把焦点交给结果网格");
    assert_eq!(
        app.ui().get_search_focus(),
        0,
        "查询词变了就不能再回旧位置（新结果里那个下标没有意义）"
    );
}

/// 输入法组字期间 ↑ / ↓ 交给输入法，不交接焦点。
///
/// 用 Slint 内部的 `UpdateComposition` 事件模拟一次组字（`TextInput.preedit-text`
/// 变为非空），然后发方向键：`search.slint` 的 `key-pressed` 读 `preedit-text`，
/// 非空就 `reject`，焦点必须留在输入框。
#[test]
fn down_during_ime_composition_stays_in_the_input() {
    let app = setup();
    app.set_home(&home_filled());
    let view = search_view();
    app.set_search(&view);
    app.open_search(&view);
    assert!(app.handle_key(KeyCommand::Search));
    assert!(app.ui().get_search_focused(), "前提：搜索框持有焦点");

    // 组字中："沙" 的拼音在输入法里还没提交。
    let composition = i_slint_core::input::InternalKeyEvent {
        event_type: i_slint_core::input::KeyEventType::UpdateComposition,
        preedit_text: "sha".into(),
        ..Default::default()
    };
    app.ui()
        .window()
        .dispatch_event(i_slint_core::platform::WindowEvent::internal(composition));

    // 组字期间 ↓（选下一候选）不能把焦点交给结果网格。
    press(&app, Key::DownArrow);
    assert!(
        app.ui().get_search_focused(),
        "组字期间 ↓ 属于输入法，不能交接焦点"
    );
    assert_eq!(app.focus_layer(), FocusLayer::Content);

    press(&app, Key::UpArrow);
    assert!(
        app.ui().get_search_focused(),
        "组字期间 ↑ 属于输入法，不能进顶栏"
    );
    assert_eq!(app.focus_layer(), FocusLayer::Content);
}
