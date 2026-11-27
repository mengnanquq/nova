// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `sweep`（`--debug-socket` 的交互扫描）的恢复器回归测试。
//!
//! 体检 R8 的两条遗留：
//! 1. 每个目标扫完都无条件把媒体库 / 搜索视图写回去：`set_library` 会重建网格、
//!    清图片窗口缓存、换库时还会复位滚动。快照与当前值一致时（首页 / 详情页 /
//!    播放页上的绝大多数目标）这些都是白做的。
//! 2. 首页各行的行内横向滚动（`CardRow.flick.content-x`）是 Slint 的局部状态，
//!    Rust 侧没有通道：点过 `home.row.*.right` / `*.left` 之后这一行就停在滚开的
//!    位置，同一轮里后面按**扫描开始时的坐标**点这一行的卡片会点到别的卡。

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use common::{home_filled, home_image_ids, init_backend, install_posters, library_view};
use nova_ui::{App, AppWindow, CardView, HomeView, Page};
use slint::ComponentHandle;

const WINDOW_W: f32 = 1280.0;
const WINDOW_H: f32 = 800.0;

fn window() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(WINDOW_W);
    app.ui().set_window_h(WINDOW_H);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(WINDOW_W, WINDOW_H));
    app
}

fn settle(app: &App, millis: u64) {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(millis));
}

/// 首页（继续观看 9 张：一行装不下、滚一屏之后右边还有余量）+ 媒体库数据。
fn home_with_library() -> (App, HomeView) {
    let app = window();
    let mut home = home_filled();
    for index in 6..=9 {
        home.continue_watching.push(
            CardView::new(format!("c{index}"), format!("续看测试 {index}"))
                .with_subtitle("第 1 季 · 第 1 集")
                .with_progress(0.1),
        );
    }
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.set_library(&library_view());
    app.show_home();
    app.ui().set_billboard_ratio(0.2);
    app.ui().show().expect("显示窗口");
    settle(&app, 200);
    (app, home)
}

/// 把指针移到某张卡上（行的左右箭头只在"行被点亮"时出现）。
fn hover_card(app: &App, id: &str) {
    let card = app.target(id).expect("卡片目标");
    let (x, y) = card.center();
    app.hover_at(x, y).expect("悬停卡片");
    settle(app, 200);
}

fn card_x(app: &App, id: &str) -> f32 {
    app.target(id)
        .unwrap_or_else(|| {
            panic!(
                "清单里没有 {id}：{:?}",
                app.targets().iter().map(|t| &t.id).collect::<Vec<_>>()
            )
        })
        .x
}

/// 快照与当前值一致时，恢复器不该重写媒体库 / 搜索视图。
///
/// 断言用图片窗口上报次数：`set_library` 会清掉 `last_library_images`，下一次
/// `sync_grid_window` 必然重新上报一次（即使可见的键完全相同）。修复前每个目标
/// 都会触发一次，首页上任何点击都等于"重建网格 + 重发图片请求"。
#[test]
fn sweep_does_not_rewrite_unchanged_views() {
    let (app, _home) = home_with_library();
    let reports = Rc::new(Cell::new(0usize));
    {
        let reports = reports.clone();
        app.on_library_images(move |_keys| reports.set(reports.get() + 1));
    }

    assert!(app.targets().len() > 3, "前提：首页有一批可点目标");
    let report = app.sweep();
    assert!(!report.aborted, "首页扫描不该中途收尾");

    assert_eq!(
        reports.get(),
        0,
        "快照与当前值一致时不该重写媒体库视图（会重建网格 / 清图片窗口 / 复位滚动）"
    );
}

/// 行箭头不能被 `sweep` 误记成 dead。
///
/// 箭头走的是平滑滚动：点击那一刻 `flick.content-x` 还没变（动画要等 tick），
/// 只看状态摘要（不包含行内横向滚动）就会把箭头记成“点了没反应”。修复后
/// `sweep` 在恢复前再读一次各行的**箭头点击计数**：点完比点前多就算有反应。
#[test]
fn sweep_does_not_mark_row_arrows_dead() {
    let (app, _home) = home_with_library();
    app.ui().set_nav_active(true);
    settle(&app, 120);
    let right: slint::SharedString = slint::platform::Key::RightArrow.into();
    let left: slint::SharedString = slint::platform::Key::LeftArrow.into();
    // 把焦点推到行内第 4 张：行被点亮且横向滚开，左 / 右箭头才会同时出现在
    // 目标清单里（箭头只在“本行还能往那个方向滚”时登记）。
    for _ in 0..3 {
        app.press_key(right.clone()).expect("按右键");
        settle(&app, 120);
    }
    app.press_key(left).expect("按左键");
    settle(&app, 200);
    assert!(
        app.target("home.row.continue.left").is_some()
            && app.target("home.row.continue.right").is_some(),
        "前提：继续观看行的左右箭头都在清单里"
    );

    let report = app.sweep();
    assert!(!report.aborted, "首页扫描不该中途收尾");
    let arrows: Vec<&str> = report
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .filter(|id| id.ends_with(".left") || id.ends_with(".right"))
        .collect();
    assert!(!arrows.is_empty(), "前提：扫描清单里有行箭头");
    let dead: Vec<&str> = report
        .dead()
        .iter()
        .map(|entry| entry.id.as_str())
        .filter(|id| id.ends_with(".left") || id.ends_with(".right"))
        .collect();
    assert!(
        dead.is_empty(),
        "行箭头点击会启动平滑滚动，不能在 sweep 里被记成 dead：{dead:?}"
    );
}

/// 扫描前后各行的行内横向滚动位置必须一致。
///
/// 扫描会真的点 `home.row.*.right` / `*.left`：这一行被滚走之后不写回，页面就
/// 停在另一个位置——同一轮里后面按旧坐标点的卡片会落到邻卡上（R8 审查）。
///
/// 起点故意弄成**非整屏**的偏移（键盘把焦点移出可视区），这样左 / 右箭头各
/// 按一次不能碰巧回到原位：只有恢复器真写回了快照值，前后坐标才会相等。
/// 同时把「最近添加」行也预先滚开：写回是**整份数组**一次写的，没被扫描点过的
/// 行不能被默认值 0 拖回行首。
#[test]
fn sweep_restores_row_scroll_to_the_snapshot() {
    let (app, _home) = home_with_library();
    let origin = card_x(&app, "home.row.continue.card.2");
    let recent_origin = card_x(&app, "home.row.recent.card.2");

    app.ui().set_nav_active(true);
    settle(&app, 120);
    let down: slint::SharedString = slint::platform::Key::DownArrow.into();
    let up: slint::SharedString = slint::platform::Key::UpArrow.into();
    let right: slint::SharedString = slint::platform::Key::RightArrow.into();
    // 先滚开「最近添加」（下移进这一行，再行内右移到 1280 宽装不下的地方），
    // 留一份非零快照。
    app.press_key(down).expect("按↓键");
    settle(&app, 120);
    for _ in 0..6 {
        app.press_key(right.clone()).expect("按右键");
        settle(&app, 120);
    }
    let recent_before = card_x(&app, "home.row.recent.card.2");
    assert!(
        recent_before < recent_origin - 1.0,
        "前提：最近添加行已经横向滚开（{recent_origin} → {recent_before}）"
    );
    // 回到「继续观看」并沿行内退到局部 2：扫描开始时焦点在这一行，左右箭头才在
    // 目标清单里（箭头只在行被点亮时登记），扫描才会真的把它再滚走；退到局部 2
    // 既能保持非零偏移，又让第 3 张卡留在窗口里，恢复后才能量它的坐标。
    app.press_key(up).expect("按↑键");
    let left: slint::SharedString = slint::platform::Key::LeftArrow.into();
    for _ in 0..4 {
        app.press_key(left.clone()).expect("按左键");
    }
    settle(&app, 120);

    let before = card_x(&app, "home.row.continue.card.2");
    assert!(
        before < origin - 1.0,
        "前提：这一行已经横向滚开（{origin} → {before}）"
    );
    assert!(
        app.target("home.row.continue.left").is_some(),
        "前提：滚开之后左箭头出现"
    );
    assert!(
        app.target("home.row.continue.right").is_some(),
        "前提：右边还有余量（扫描会再点一次 right）"
    );

    let report = app.sweep();
    assert!(!report.aborted, "首页扫描不该中途收尾");
    // 箭头的平滑滚动要几帧才停：不推进时间的话量到的还是动画开始前的位置。
    settle(&app, 1200);

    let after = app
        .target("home.row.continue.card.2")
        .expect("写回后第 3 张卡仍应在视口内（被推出窗口 = 行内滚动没写回）")
        .x;
    assert!(
        (after - before).abs() < 1.0,
        "扫描结束必须把行内横向滚动写回扫描开始的位置（{before} → {after}）"
    );
    let recent_after = app
        .target("home.row.recent.card.2")
        .expect("写回后最近添加第 3 张卡仍应存在")
        .x;
    assert!(
        (recent_after - recent_before).abs() < 1.0,
        "没被扫描点过的最近添加行也必须保持快照位置（{recent_before} → {recent_after}）"
    );
    assert_eq!(app.page(), Page::Home, "扫描结束仍停在首页");
}

/// 扫描开始时的坐标 + 写回之后点下去，必须还是同一张卡。
///
/// 这是"同一轮扫描里后续点击不会点错卡"的最小复现：先按扫描的规矩取坐标，再点
/// 右箭头把行滚走（`flick.content-x` 变化 ⇒ 所有卡片的窗口坐标整体左移），
/// 写回之后用**旧坐标**点第 2 张，打开的必须是第 2 张对应的条目。
#[test]
fn stale_coordinates_still_hit_the_same_card_after_restore() {
    let (app, _home) = home_with_library();
    hover_card(&app, "home.row.continue.card.1");

    let snapshot = app
        .target("home.row.continue.card.1")
        .expect("继续观看第 2 张卡");
    let (x, y) = snapshot.center();

    app.click_target("home.row.continue.right")
        .expect("右箭头应在清单里");
    settle(&app, 800);
    let scrolled = app
        .target("home.row.continue.card.1")
        .expect("继续观看第 2 张卡");
    assert!(
        scrolled.x < snapshot.x - 1.0,
        "前提：点右箭头把这一行滚走了（{} → {}）",
        snapshot.x,
        scrolled.x
    );

    // 恢复器做的事：让各行报一次位置，再整份数组写回原处（`sweep` 每轮都走一遍）。
    // 这里直接用同一条 Slint 公开通道，坐标用的是扫描开始时的那一份。
    app.ui()
        .set_home_row_scroll_targets(slint::ModelRc::new(slint::VecModel::from(vec![0.0f32])));
    app.ui()
        .set_home_row_scroll_gen(app.ui().get_home_row_scroll_gen() + 1);
    app.refresh_targets();
    assert!(
        (card_x(&app, "home.row.continue.card.1") - snapshot.x).abs() < 1.0,
        "写回之后卡片必须回到扫描开始时的位置"
    );

    let opened = Rc::new(RefCell::new(Vec::<String>::new()));
    {
        let opened = opened.clone();
        app.on_open_detail(move |id| opened.borrow_mut().push(id.to_owned()));
    }
    // 用旧坐标点（不清坐标！
    app.click_at(x, y).expect("按旧坐标点第 2 张卡");
    assert_eq!(
        opened.borrow().as_slice(),
        ["c2"],
        "旧坐标点下去必须还是第 2 张卡（滚不回去就会点到邻卡）"
    );
}
