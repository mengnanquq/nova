// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 体检 R7-2：首页的焦点行模型必须**一个视觉行一行**。
//!
//! 修复前的真机现象（1920×1080）：
//!
//! - 在「最近添加」行按 ↓，焦点在行内横跳（13 → 19 → 25，每次 +6 张）；
//! - 到行尾后 ↓ 完全没反应，10 个库行**整行不可达**（键盘路径上只有前两行）；
//! - 库行没接 `focused-index` / `focus-hover`，鼠标悬停也不对齐。
//!
//! 根因：`HomeView::focus_rows` 把「最近添加」按 6 列切成网格行、并且完全没把
//! 库行算进去；`.slint` 的库行也没绑焦点。界面上它们其实都是横向 Flickable
//! （`CardRow`），不是网格。
//!
//! 期望（与 `docs/ui-architecture.md` 一致）：↓ / ↑ 在行之间移动并保持最接近的列，
//! ← / → 在行内移动；能到达最后一行，也能回到 hero 与顶栏。
//! 本文件走一遍：hero → 继续观看 → 最近添加 → 4 个库行（最后一行）→ 再 ↑ 回 hero。

mod common;

use std::time::Duration;

use common::{home_filled, init_backend};
use nova_ui::{App, AppWindow, CardView, FocusLayer, HomeView, LibraryRowView, Page, TargetInfo};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

const W: f32 = 1920.0;
const H: f32 = 1080.0;

/// 焦点行（与 `HomeView::focus_rows` 一致）：继续观看 2 + 最近添加 8 + 4 个库行。
const ROWS: [usize; 6] = [2, 8, 6, 6, 6, 6];

fn app_1920x1080() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(W);
    app.ui().set_window_h(H);
    app.ui().window().set_size(slint::LogicalSize::new(W, H));
    app
}

/// 首页：hero 带简介（`hero` 按钮行只在有简介时才实例化），4 个库行。
fn home() -> HomeView {
    let mut home = home_filled();
    home.hero = home
        .hero
        .map(|hero| hero.with_overview("保罗·厄崔迪与弗雷曼人并肩作战，走上复仇之路。"));
    home.continue_watching.truncate(2);
    home.recently_added = (0..8)
        .map(|index| CardView::new(format!("r{index}"), format!("最近 {index}")))
        .collect();
    home.library_rows = (0..4)
        .map(|row| LibraryRowView {
            library_id: format!("lib-{row}"),
            title: format!("库 {row}"),
            items: (0..6)
                .map(|index| {
                    CardView::new(format!("l{row}-{index}"), format!("库 {row} 第 {index}"))
                })
                .collect(),
        })
        .collect();
    home
}

fn settle(app: &App) {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    }
}

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
    settle(app);
}

/// 扁平下标 → (行号, 行内列号)。
fn row_col(index: usize) -> (usize, usize) {
    let mut rest = index;
    for (row, len) in ROWS.iter().enumerate() {
        if rest < *len {
            return (row, rest);
        }
        rest -= *len;
    }
    panic!("扁平下标 {index} 超出首页焦点行 {ROWS:?}");
}

fn row_start(row: usize) -> usize {
    ROWS[..row].iter().sum()
}

fn target(app: &App, id: &str) -> Option<TargetInfo> {
    app.target(id)
}

/// 焦点卡必须完整落在窗口里（首页行是横向 Flickable，两个方向都要跟着滚）。
fn assert_focus_card_visible(label: &str, app: &App, id: &str) {
    let card = target(app, id).unwrap_or_else(|| {
        panic!(
            "{label}: 焦点卡 {id} 不在窗口里；当前首页目标 {:?}",
            app.targets()
                .iter()
                .map(|target| target.id.clone())
                .collect::<Vec<_>>()
        )
    });
    assert!(
        f64::from(card.y) >= -0.5 && f64::from(card.y + card.height) <= f64::from(H) + 0.5,
        "{label}: 焦点卡纵向越界 y={} h={}（窗口高 {H}）",
        card.y,
        card.height
    );
    assert!(
        f64::from(card.x) >= -0.5 && f64::from(card.x + card.width) <= f64::from(W) + 0.5,
        "{label}: 焦点卡横向越界 x={} w={}（窗口宽 {W}）",
        card.x,
        card.width
    );
}

/// 焦点卡的目标 id：第 0 行是继续观看、第 1 行是最近添加，其余是库行。
fn focus_target_id(index: usize) -> String {
    let (row, column) = row_col(index);
    match row {
        0 => format!("home.row.continue.card.{column}"),
        1 => format!("home.row.recent.card.{column}"),
        _ => format!("home.row.lib.{}.card.{column}", row - 2),
    }
}

fn assert_focus(app: &App, expected: usize, label: &str) {
    assert_eq!(
        app.focus_index(),
        expected,
        "{label}: 焦点应是 {expected}（行/列 {:?}），实际 {}{:?}",
        row_col(expected),
        app.focus_index(),
        row_col(app.focus_index())
    );
    assert_eq!(
        app.ui().get_focus_layer(),
        FocusLayer::Content.index(),
        "{label}: 焦点应在内容层"
    );
    let (row, _) = row_col(expected);
    if row >= 2 {
        // 页面级焦点探针只覆盖前两行；焦点在库行时必须报 -1，不能拿库行下标去点亮
        // 前两行的几何（否则调试通道会给出错误的“焦点卡在视口内”结论）。
        for (name, value) in [
            ("focus_top", app.ui().get_home_focus_top()),
            ("focus_bottom", app.ui().get_home_focus_bottom()),
            ("focus_left", app.ui().get_home_focus_left()),
            ("focus_right", app.ui().get_home_focus_right()),
        ] {
            assert_eq!(
                f64::from(value),
                -1.0,
                "{label}: 焦点在库行时 {name} 必须返回 -1（实际 {value}）"
            );
        }
    }
    assert_focus_card_visible(label, app, &focus_target_id(expected));
}

#[test]
fn home_keyboard_walks_every_row_down_and_back_up() {
    let app = app_1920x1080();
    app.set_home(&home());
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    settle(&app);
    // 首屏只有 billboard + 继续观看的表头（88vh 封顶），先把焦点行滚进来。
    app.ui().invoke_scroll_home_to_index(0, 0);
    settle(&app);

    assert_focus(&app, 0, "首页起点（继续观看第 1 张）");

    // ↑：内容第一行 → hero 按钮行。
    press(&app, Key::UpArrow);
    assert_eq!(
        app.ui().get_focus_layer(),
        FocusLayer::Hero.index(),
        "内容第一行再往上先进 hero"
    );
    // ↓：hero → 内容层（焦点留在继续观看）。
    press(&app, Key::DownArrow);
    assert_focus(&app, 0, "hero 回到内容层");

    // ↓ 逐行走到底：最近添加 → 库 0 → 库 1 → 库 2 → 库 3。
    for row in 1..=5 {
        press(&app, Key::DownArrow);
        assert_focus(&app, row_start(row), "向下逐行");
    }

    // 最后一行再 ↓：停在原地（不绕回、不跑空）。
    let last = row_start(5);
    press(&app, Key::DownArrow);
    assert_focus(&app, last, "最后一行的第一张卡");

    // ← / → 只在**行内**移动：到最后一行第 3 列，再 → 到第 6 列。
    press(&app, Key::RightArrow);
    press(&app, Key::RightArrow);
    assert_focus(&app, last + 2, "行内右移");
    for _ in 0..10 {
        press(&app, Key::RightArrow);
    }
    assert_focus(&app, last + ROWS[5] - 1, "行内右移到行尾就停住");
    for _ in 0..10 {
        press(&app, Key::LeftArrow);
    }
    assert_focus(&app, last, "行内左移到行首就停住");

    // ↑ 逐行回到继续观看（保持最接近的列 = 0），再 ↑ 回 hero、再 ↑ 到顶栏。
    for row in (1..5).rev() {
        press(&app, Key::UpArrow);
        assert_focus(&app, row_start(row), "向上逐行");
    }
    press(&app, Key::UpArrow);
    assert_focus(&app, 0, "继续观看第 1 张");
    press(&app, Key::UpArrow);
    assert_eq!(
        app.ui().get_focus_layer(),
        FocusLayer::Hero.index(),
        "内容第一行再往上回 hero"
    );
    press(&app, Key::UpArrow);
    assert_eq!(
        app.ui().get_focus_layer(),
        FocusLayer::Navbar.index(),
        "hero 再往上回顶栏"
    );
}

/// 列保持：在「最近添加」第 5 列按 ↓，落到库行也必须是最接近的第 5 列。
#[test]
fn vertical_moves_keep_the_closest_column() {
    let app = app_1920x1080();
    app.set_home(&home());
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    settle(&app);

    press(&app, Key::DownArrow);
    assert_focus(&app, row_start(1), "最近添加第 1 张");
    for _ in 0..4 {
        press(&app, Key::RightArrow);
    }
    assert_focus(&app, row_start(1) + 4, "最近添加第 5 列");

    for row in 2..=5 {
        press(&app, Key::DownArrow);
        assert_focus(&app, row_start(row) + 4, "库行的同一列");
    }
}

/// 库行悬停要对齐焦点（修复前库行没接 `focus-hover`，悬停不改变 `focus_ui`）。
#[test]
fn hovering_a_library_card_moves_the_focus_there() {
    let app = app_1920x1080();
    app.set_home(&home());
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    settle(&app);

    // 先滚到库行，让目标在窗口里。
    for _ in 0..2 {
        press(&app, Key::DownArrow);
    }
    settle(&app);
    let card = app
        .target("home.row.lib.0.card.3")
        .expect("库 0 的第 4 张卡应在清单里");
    let (x, y) = card.center();
    app.ui()
        .window()
        .dispatch_event_with_result(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        })
        .expect("派发 PointerMoved");
    settle(&app);

    assert_eq!(
        app.ui().get_home_focus(),
        (row_start(2) + 3) as i32,
        "悬停库行第 4 张卡应把焦点移到它"
    );
}

/// 悬停不是键盘移动：只同步焦点，不允许纵向滚动（R7-1 回归）。
///
/// 修复前 `CardRow.changed focused-index` 一看到焦点变化就 `scroll-request`，
/// 而悬停（`focus-hover`）也改焦点——悬停一张被窗口底边裁掉的卡会把页面拽走。
/// 重贴同一份视图（每批海报到达时 `set_home` 都会重贴）时，焦点还会被 Rust 里
/// 过期的 `state.focus.home` 覆盖回去。
#[test]
fn hovering_a_partially_visible_card_does_not_scroll_or_reset_the_focus() {
    let app = app_1920x1080();
    let view = home();
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    // 真实场景：用户已经按过方向键（`nav-active` = true）。
    app.ui().set_nav_active(true);
    app.ui().show().expect("显示窗口");
    settle(&app);

    let content_h = f64::from(app.ui().get_home_content_h());
    let view_h = f64::from(app.ui().get_home_viewport_h());
    let max_scroll = (content_h - view_h).max(0.0);
    // 停在「最后一行的卡片被窗口底边裁掉约 100px」的位置。
    app.ui().invoke_scroll_home_to(-(max_scroll - 100.0) as f32);
    settle(&app);
    let scroll_y = f64::from(app.ui().get_home_scroll_y());

    let card = app
        .target("home.row.lib.3.card.0")
        .expect("最后一行第 1 张卡应在目标清单里");
    assert!(
        f64::from(card.y + card.height) > f64::from(H),
        "前提：这张卡必须被窗口底边裁掉一部分（y={} h={}）",
        card.y,
        card.height
    );
    let x = card.x + card.width / 2.0;
    let y = H - 10.0;
    app.ui()
        .window()
        .dispatch_event_with_result(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        })
        .expect("派发 PointerMoved");
    settle(&app);

    let hovered = app.ui().get_home_focus();
    assert_eq!(
        hovered,
        row_start(5) as i32,
        "悬停要把焦点同步到最后一行第 1 张"
    );
    assert_eq!(
        f64::from(app.ui().get_home_scroll_y()),
        scroll_y,
        "悬停不是键盘移动，页面不能被拽走"
    );

    // 海报批次到达：用同一份视图重贴，焦点与滚动都不能变。
    app.set_home(&view);
    settle(&app);
    assert_eq!(
        app.ui().get_home_focus(),
        hovered,
        "重贴同一份视图不能把焦点改回 Rust 里的旧位置"
    );
    assert_eq!(
        f64::from(app.ui().get_home_scroll_y()),
        scroll_y,
        "重贴同一份视图不能把页面弹回键盘焦点位置"
    );
}

/// 第一行是空的库行时，↑ 仍然要能回到 hero（R7-2 的另一半）。
///
/// 修复前 `focus_rows` 保留了长度为 0 的库行，`move_up` 拿 `rows.first()`（0）
/// 判断“是否在第一行”，永远为假，于是 ↑ 变成原地 no-op。
#[test]
fn an_empty_first_focus_row_does_not_block_up_to_hero() {
    let app = app_1920x1080();
    let mut view = home();
    view.continue_watching.clear();
    view.recently_added.clear();
    view.library_rows.insert(
        0,
        LibraryRowView {
            library_id: "lib-empty".to_owned(),
            title: "空库".to_owned(),
            items: Vec::new(),
        },
    );
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    settle(&app);

    press(&app, Key::UpArrow);
    assert_eq!(
        app.ui().get_focus_layer(),
        FocusLayer::Hero.index(),
        "第一行是空库行时，↑ 仍要回到 hero"
    );
}
