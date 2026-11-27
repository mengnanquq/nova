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
use nova_ui::{
    App, AppWindow, CardView, FocusLayer, HomeView, LibraryRowView, Page, TargetInfo, Theme,
};
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

    // A4：最后一行的滚动落位是**严格**的 —— 卡片下沿正好贴在
    // 「视口底 − 放大余量/2」上（行高与 `library-card-top` 曾经是三份拷贝，
    // 只要有一份偏离这个等式就会露出黑边或被裁掉）。
    let pad = f64::from(app.ui().global::<Theme>().get_zoom_allowance_y()) / 2.0;
    let last_card = app
        .target(&focus_target_id(last))
        .expect("最后一行第一张卡应在窗口里");
    let bottom = f64::from(last_card.y + last_card.height);
    assert!(
        (bottom - (f64::from(H) - pad)).abs() <= 1.0,
        "最后一行卡片下沿 {bottom} 应等于视口底减放大余量 {}（±1）",
        f64::from(H) - pad
    );

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

/// A2：数据变少把焦点钳回 0 时，页面要滚回顶部，并清掉悬停锁存。
///
/// 修复前：`set_home` 只把 `focus_index` 写回 0，页面仍停在底部（按 Enter 打开的
/// 是屏幕外那张），行里的悬停锁存（`hot` / `latch`）也留着——放大层与左右箭头
/// 亮在一个已经不存在的位置。
#[test]
fn shrinking_the_home_data_resets_the_scroll_and_the_hover() {
    let app = app_1920x1080();
    let view = home();
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().set_home_focus(0);
    app.ui().show().expect("显示窗口");
    settle(&app);

    // 先把「最近添加」行滚进窗口，再悬停第 4 张：焦点 → 5，行内 hot/latch = 3。
    // 全程不按方向键，`nav-active` 保持 false，于是放大层只可能来自悬停。
    app.ui().invoke_scroll_home_to(-800.0);
    settle(&app);
    let card = app
        .target("home.row.recent.card.3")
        .expect("最近添加第 4 张应在窗口里");
    let (x, y) = card.center();
    app.ui()
        .window()
        .dispatch_event_with_result(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        })
        .expect("派发 PointerMoved");
    settle(&app);
    assert_eq!(app.ui().get_home_focus(), 5, "悬停应对齐焦点");
    assert!(
        app.target("home.row.recent.left").is_some(),
        "前提：悬停让这一行的左右箭头出现（zoom-index >= 0）"
    );
    assert!(
        f64::from(app.ui().get_home_scroll_y()) < 0.0,
        "前提：页面已经滚离顶部"
    );

    // 数据变少：继续观看 2 + 最近添加 3 = 5 张，旧焦点 5 越界 → 钳到 0。
    let mut shrunk = home();
    shrunk.recently_added.truncate(3);
    shrunk.library_rows.clear();
    app.set_home(&shrunk);
    settle(&app);

    assert_eq!(app.ui().get_home_focus(), 0, "越界焦点必须钳回第 1 张");
    assert_eq!(
        f64::from(app.ui().get_home_scroll_y()),
        0.0,
        "钳位后页面必须滚回顶部（否则 Enter 打开的是屏幕外那张）"
    );
    assert!(
        app.target("home.row.recent.left").is_none(),
        "悬停锁存必须清掉：焦点已经不在那一行，箭头不能留下"
    );
}

/// A3：首页焦点按**条目 id** 重定位，而不是沿用旧下标。
///
/// 修复前 `set_home` 只在越界时才写回焦点：`HomeView` 重新排序（「继续观看」按
/// 最近播放重排）后，同一个下标指向的已经是另一部作品，焦点会悄悄飘走。
#[test]
fn focus_follows_the_item_when_the_home_view_is_reordered() {
    let app = app_1920x1080();
    let mut view = home();
    view.continue_watching = vec![
        CardView::new("a", "A"),
        CardView::new("b", "B"),
        CardView::new("c", "C"),
    ];
    view.recently_added.clear();
    view.library_rows.clear();
    view.hero = None;
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().set_home_focus(1);
    app.ui().show().expect("显示窗口");
    settle(&app);
    assert_eq!(app.ui().get_home_focus(), 1, "前提：焦点停在 b（下标 1）");

    // 重排：b 跑到第一位。
    let mut next = view.clone();
    next.continue_watching = vec![
        CardView::new("b", "B"),
        CardView::new("c", "C"),
        CardView::new("a", "A"),
    ];
    app.set_home(&next);
    settle(&app);

    assert_eq!(
        app.ui().get_home_focus(),
        0,
        "焦点应按 item id 跟着 b 落到新下标 0，而不是留在旧下标 1（c）"
    );
}

/// R7-焦点：同一部作品同时出现在「继续观看」和某个库行里时，重贴视图不能把焦点
/// 从库行那张静默挪回「继续观看」。
///
/// 修复前 `rebase_home_focus` 用 `find` 取第一个同 id 的位置；每批海报到达时
/// `apply_images` 都会用**同一份**视图重贴，于是只要焦点停在库行那张 dup 上，
/// 就会被挪回「继续观看」（下标 1）。
#[test]
fn refocusing_the_home_keeps_the_focus_on_the_library_duplicate() {
    let app = app_1920x1080();
    let mut view = home();
    view.continue_watching = vec![
        CardView::new("c0", "C0"),
        CardView::new("dup", "DUP"),
        CardView::new("c1", "C1"),
    ];
    view.recently_added.clear();
    view.library_rows = vec![LibraryRowView {
        library_id: "lib-dup".to_owned(),
        title: "库".to_owned(),
        items: vec![CardView::new("dup", "DUP"), CardView::new("x", "X")],
    }];
    app.set_home(&view);
    app.show_page(Page::Home);
    // 扁平下标：c0=0、dup(继续观看)=1、c1=2、dup(库行)=3、x=4。
    app.ui().set_home_focus(3);
    app.ui().set_nav_active(true);
    app.ui().show().expect("显示窗口");
    settle(&app);
    assert_eq!(app.ui().get_home_focus(), 3, "前提：焦点停在库行那张 dup");

    // 海报批次到达：用同一份视图重贴。
    app.set_home(&view);
    settle(&app);
    assert_eq!(
        app.ui().get_home_focus(),
        3,
        "同一份视图重贴不能把焦点从库行那张挪回「继续观看」"
    );

    // 轻微重排：继续观看换序，dup 变成下标 0，库里那张仍是 3。
    let mut reordered = view.clone();
    reordered.continue_watching = vec![
        CardView::new("dup", "DUP"),
        CardView::new("c0", "C0"),
        CardView::new("c1", "C1"),
    ];
    app.set_home(&reordered);
    settle(&app);
    assert_eq!(
        app.ui().get_home_focus(),
        3,
        "重排后焦点仍应留在库行那张（离旧下标最近的同 id 位置）"
    );
}

/// R7-悬停（本轮复审第 2 项调整了语义）：数据变少、焦点被钳回 0 时，放大效果不能
/// 留在**指针已经不在的那一格**上；但它也不能被"永久熄灭"——指针一动就要重新点亮。
///
/// 历史：R7 当时的判据是"指针停在一张还存在的卡上，放大必须留着"（只按条数判断
/// `hover-reset`）。实测前提不成立：清空「继续观看」后「最近添加」整行上移 274px
/// （1920×1080），指针那一格下面**一张卡都没有**了，放大留在 `hot` 上就是复审说的
/// "停在指针并没有停留的卡上"。所以现在改成清掉、再由仍有 `has-hover` 的卡重新点亮
/// （`CardRow.hover-reset`）：没有卡带悬停 ⇒ 熄灭；指针一动 ⇒ 立即可重新点亮。
#[test]
fn shrinking_the_data_does_not_leave_the_zoom_on_a_stale_card() {
    let app = app_1920x1080();
    let mut view = home();
    view.hero = None;
    view.continue_watching = vec![CardView::new("c0", "C0"), CardView::new("c1", "C1")];
    view.recently_added = (0..6)
        .map(|index| CardView::new(format!("r{index}"), format!("最近 {index}")))
        .collect();
    view.library_rows.clear();
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    settle(&app);

    let card = app
        .target("home.row.recent.card.5")
        .expect("「最近添加」第 6 张应在窗口里");
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
        2 + 5,
        "悬停应把焦点对齐到第 6 张"
    );
    assert!(
        app.target("home.row.recent.left").is_some(),
        "前提：悬停让这一行出现左右箭头（放大已点亮）"
    );

    // 数据变少：清空「继续观看」→ 焦点 7 越界，页面要求重算悬停（`hover-reset`）。
    let mut shrunk = view.clone();
    shrunk.continue_watching.clear();
    app.set_home(&shrunk);
    settle(&app);
    assert_eq!(app.ui().get_home_focus(), 0, "越界焦点必须钳回第 1 张");
    assert!(
        !any_card_under(&app, x, y),
        "前提：行上移后指针 ({x}, {y}) 下面已经没有卡"
    );
    assert!(
        app.target("home.row.recent.left").is_none(),
        "指针下面没有卡，放大与左右箭头必须熄掉（不能留在旧下标上）"
    );

    // 复位不是"永久熄灭"：指针动一下，同一格立刻重新点亮（R7 的原关注点）。
    let card = app
        .target("home.row.recent.card.5")
        .expect("「最近添加」第 6 张仍应在窗口里");
    let (x, y) = card.center();
    app.hover_at(x + 1.0, y).expect("再悬停第 6 张");
    settle(&app);
    assert!(
        app.target("home.row.recent.left").is_some(),
        "指针一动就必须能重新点亮（`has-hover` 不该被永久熄灭）"
    );
    assert_eq!(
        app.ui().get_home_focus(),
        5,
        "重新点亮后焦点回到「最近添加」第 6 张（继续观看已空）"
    );
}

/// R7-悬停（本轮复审第 1 项）：数据条数**不变**、但本行的卡片被整体替换后，指针
/// 没动也不能把放大效果熄灭。
///
/// 修复前 `convert::model` 每次 `set_home` 都新建一个 `VecModel`：Slint 的重复元素
/// 按 model 指针判断有没换数据源，于是整行 `PosterCard` 实例全被重建，卡片上的
/// `has-hover` 随之清空（指针不动就不会有新事件），上一轮新增的 `hover-gen`
/// （让仍有 `has-hover` 的卡重新点亮）在线上永远没机会生效。现在模型原地写
/// （`reuse_model`）+ `set_home` 内容变了就推 `hover-reset`：实例、悬停状态都保留，
/// 序号一推，指针下那张卡自己把放大重新点亮（同一格，数据已换新）。
#[test]
fn replacing_the_cards_under_a_parked_pointer_keeps_the_zoom_on_that_card() {
    let app = app_1920x1080();
    let mut view = home();
    view.hero = None;
    view.continue_watching = (0..5)
        .map(|index| CardView::new(format!("c{index}"), format!("C{index}")))
        .collect();
    view.recently_added.clear();
    view.library_rows.clear();
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    settle(&app);

    let card = app
        .target("home.row.continue.card.2")
        .expect("「继续观看」第 3 张应在窗口里");
    let (x, y) = card.center();
    app.hover_at(x, y).expect("悬停第 3 张");
    settle(&app);
    assert!(app.ui().get_row0_zoom_on(), "前提：悬停点亮放大");
    assert_eq!(
        app.ui().get_home_focus(),
        2,
        "前提：悬停把焦点对齐到第 3 张"
    );
    let zoom_x = app.ui().get_zoom_rect_x();

    // 条数不变（5 → 5）、内容整体替换，指针没动：**只走生产路径**（只调 `set_home`，
    // 不手动推任何复位序号）。模型是原地改的，卡片实例没重建。
    let mut replaced = view.clone();
    replaced.continue_watching = (10..15)
        .map(|index| CardView::new(format!("d{index}"), format!("D{index}")))
        .collect();
    app.set_home(&replaced);
    settle(&app);
    assert!(
        app.ui().get_row0_zoom_on(),
        "卡片实例被复用时指针仍停在同一格（{zoom_x}），放大不能熄灭"
    );
    assert_eq!(
        app.ui().get_home_focus(),
        2,
        "焦点仍在指针下那一格（不是掉回 0，也没熄掉）"
    );
    assert_eq!(
        app.ui().get_zoom_rect_x(),
        zoom_x,
        "放大层还在同一格上（下标 2 的几何不变）"
    );
    // 同一格里已经是新数据：模型原地更新，实例没重建。
    let card = app
        .target("home.row.continue.card.2")
        .expect("替换后第 3 张仍应在窗口里");
    assert_eq!(card.label, "D12", "下标 2 的卡片数据已换成新视图的");

    // 指针再动一下：同一格仍然是亮的（不能因为复位而“永久熄灭”）。
    let (x, y) = card.center();
    app.hover_at(x + 1.0, y).expect("再悬停第 3 张");
    settle(&app);
    assert!(app.ui().get_row0_zoom_on(), "指针一动仍然点亮同一张");
    assert_eq!(app.ui().get_home_focus(), 2, "焦点还是第 3 张");
}

/// 悬停锁存不能留在越界下标上：指针停在行的左右箭头上时，`hover-reset` 会跳过
/// “清掉 + 重新点亮”那一段（放大层本来就是指针底下的对象），但**越界**的
/// `hot` / `latch` 必须清掉——否则 `zoom-index` 还照着一个不存在的卡算，
/// 整行左右箭头会一直亮着（`arrow.visible: zoom-index >= 0`）再也下不去。
///
/// 修前：条数 5 → 4 后 `latch` 还是 4（`>= cards.length`），另一个箭头在指针不在它
/// 上面时也还登记在目标清单里。
#[test]
fn shrinking_a_row_while_the_pointer_is_on_its_arrow_drops_the_stale_zoom() {
    let app = app_1920x1080();
    let mut view = home();
    view.hero = None;
    view.continue_watching = (0..5)
        .map(|index| CardView::new(format!("c{index}"), format!("C{index}")))
        .collect();
    // 「最近添加」留着：缩行后总卡数仍大于焦点下标，`set_home` 就不会走“越界钳回 0”
    // 的硬清那条路，正好把行内锁存自己的越界夹住逻辑暴露出来。
    view.recently_added = (0..3)
        .map(|index| CardView::new(format!("r{index}"), format!("R{index}")))
        .collect();
    view.library_rows.clear();
    app.set_home(&view);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    settle(&app);

    // 悬停最后一张：锁存落在下标 4 上。
    let card = app
        .target("home.row.continue.card.4")
        .expect("「继续观看」第 5 张应在窗口里");
    let (x, y) = card.center();
    app.hover_at(x, y).expect("悬停第 5 张");
    settle(&app);
    assert!(app.ui().get_row0_zoom_on(), "前提：悬停点亮放大");

    // 指针移到本行右箭头上：`hot` 清掉，锁存留在 4（`zoom-index` 靠它维持箭头）。
    let arrow = app
        .target("home.row.continue.right")
        .expect("放大时有右箭头");
    let (ax, ay) = arrow.center();
    app.hover_at(ax, ay).expect("悬停右箭头");
    settle(&app);

    // 数据变少（5 → 4），但焦点 4 仍在总卡数（4 + 3）以内：只推 `hover-reset`。
    let mut shrunk = view.clone();
    shrunk.continue_watching.truncate(4);
    let total_before = app.ui().get_home_focus();
    app.set_home(&shrunk);
    settle(&app);
    assert!(
        total_before < 7,
        "前提：焦点 {total_before} 在缩小后仍在总卡数内（不走越界硬清）"
    );

    // 锁存里的 4 已经越界：不能再把箭头点亮。
    app.refresh_targets();
    assert!(
        app.target("home.row.continue.left").is_none(),
        "下标越界后左箭头不能还亮着（指针只停在右箭头上）"
    );
    assert!(!app.ui().get_row0_zoom_on(), "越界下标上不能还画着放大层");
}

/// 指针 (x, y) 下面是否压着某张首页卡片（`home.row.*.card.*` 目标矩形）。
fn any_card_under(app: &App, x: f32, y: f32) -> bool {
    app.targets().into_iter().any(|target| {
        target.id.contains(".card.")
            && x >= target.x
            && x <= target.x + target.width
            && y >= target.y
            && y <= target.y + target.height
    })
}
