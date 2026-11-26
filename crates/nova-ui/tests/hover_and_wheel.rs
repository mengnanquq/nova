// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 调试通道 `hover` / `scroll` 走的是**真实事件路径**（`WindowEvent::PointerMoved` /
//! `PointerScrolled`），不是直接改属性。这两个测试把"派发的事件真的到得了界面"钉住：
//!
//! 1. `PointerMoved` 命中卡片 → 卡片的 `TouchArea.has-hover` 生效 → 首页行的放大覆盖层
//!    `visible`（`row0-zoom-on`）打开；
//! 2. `PointerMoved` 命中播放页音量图标 → 控件展开 → 滑杆目标 `player.volume`
//!    有了正面积（这条以前被误判成"hover 不生效"，见下）；
//! 3. `PointerScrolled` 在网格上 → Flickable 的 `content-y` 真的变了（负 = 向下滚），
//!    反向滚回顶端能钳回 0。
//!
//! 背景：体检报告里"`hover` 派发后音量控件没进入悬停态"不是派发链路坏了——真机复测
//! 里 `PointerMoved` 同样能改焦点 / 打开放大层。当时的观察点是**目标清单**：滑杆在
//! 收起时宽度为 0、根本不登记，而 `hover` 以前又不会先刷新登记（清单可能是上一页 /
//! 上一屏的旧坐标）。所以这里断言的是"悬停真的改变了界面状态"，不是"清单里有条目"。

mod common;

use std::time::Duration;

use common::{detail_view, home_filled, init_backend, library_view, player_view};
use nova_ui::{App, AppWindow, Page, TargetInfo};
use slint::ComponentHandle;

const W: f32 = 800.0;
const H: f32 = 600.0;

fn app_of(width: f32, height: f32) -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(width);
    app.ui().set_window_h(height);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    app
}

/// 刷新目标登记并让出一拍（与调试通道 `needs_settle` 同一条路径）。
fn settled_targets(app: &App) -> Vec<TargetInfo> {
    app.refresh_targets();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
    app.targets()
}

fn target<'a>(targets: &'a [TargetInfo], id: &str) -> &'a TargetInfo {
    targets
        .iter()
        .find(|target| target.id == id)
        .unwrap_or_else(|| {
            panic!(
                "清单里没有 {id}：{:?}",
                targets.iter().map(|t| &t.id).collect::<Vec<_>>()
            )
        })
}

fn move_pointer(app: &App, x: f32, y: f32) {
    app.ui()
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        })
        .expect("派发 PointerMoved");
}

fn wheel(app: &App, x: f32, y: f32, dy: f32) {
    app.ui()
        .window()
        .dispatch_event_with_result(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(x, y),
            delta_x: 0.0,
            delta_y: dy,
        })
        .expect("派发 PointerScrolled");
}

/// 首页：把指针移到「继续观看」第 2 张卡上，行的放大覆盖层必须打开。
#[test]
fn pointer_move_opens_the_zoom_overlay_on_a_home_row_card() {
    let app = app_of(1280.0, 800.0);
    app.set_home(&home_filled());
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
    assert!(!app.ui().get_row0_zoom_on(), "没悬停时不该有放大层");

    let targets = settled_targets(&app);
    let (x, y) = target(&targets, "home.row.continue.card.1").center();
    move_pointer(&app, x, y);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));

    assert!(
        app.ui().get_row0_zoom_on(),
        "PointerMoved 命中卡片后，继续观看行应进入放大态（hover → hot → zoom-index）"
    );

    // 移开指针：放大层收起（离开也是真实事件路径的一部分）。
    move_pointer(&app, 4.0, 4.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
    assert!(!app.ui().get_row0_zoom_on(), "指针移开后放大层应收起");
}

/// 播放页：悬停音量图标 → 控件展开 → 滑杆目标有正面积。
///
/// 收起时滑杆宽 0（`player.volume` 被"正面积"过滤掉），所以"清单里没有它"不是
/// 故障；展开后必须出现——这条证明 `PointerMoved` 确实触发了 `has-hover`。
#[test]
fn pointer_move_expands_the_player_volume_slider() {
    let app = app_of(1280.0, 800.0);
    app.set_detail(&detail_view());
    app.open_detail("dune");
    app.play("dune");
    assert_eq!(app.page(), Page::Player);
    app.set_player(&player_view());
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    let targets = settled_targets(&app);
    assert!(
        !targets.iter().any(|t| t.id == "player.volume"),
        "收起时滑杆宽 0，不该进清单"
    );
    let (x, y) = target(&targets, "player.mute").center();
    move_pointer(&app, x, y);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    let targets = settled_targets(&app);
    let volume = target(&targets, "player.volume");
    assert!(
        volume.width > 1.0,
        "悬停音量图标后滑杆应展开（宽度 {}）",
        volume.width
    );
}

/// 媒体库：滚轮真的滚动网格（`content-y` 向下滚为负），反向滚回顶端钳回 0。
#[test]
fn wheel_scrolls_the_library_grid() {
    let app = app_of(W, H);
    let mut library = library_view();
    library.cards = (1..=30)
        .map(|index| nova_ui::CardView::new(format!("l{index:02}"), format!("第 {index} 部")))
        .collect();
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
    assert_eq!(f64::from(app.ui().get_library_scroll_y()), 0.0);

    // 负 dy = 向下滚（与 winit / `content-y` 的符号一致）。
    wheel(&app, W / 2.0, H / 2.0, -360.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
    let scrolled = f64::from(app.ui().get_library_scroll_y());
    assert!(
        scrolled < -100.0,
        "滚轮应把网格向下滚（scroll-y = {scrolled}）"
    );

    // 反向滚：回到顶端并钳在 0（不会滚成正值）。
    wheel(&app, W / 2.0, H / 2.0, 4000.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
    assert_eq!(f64::from(app.ui().get_library_scroll_y()), 0.0);
}

/// 媒体库网格：鼠标悬停卡片必须放大（与首页行同一规则），移开后收起。
///
/// 回归：以前网格的放大只看 `nav-active`（按过方向键才为真），鼠标悬停只移动焦点、
/// 不放大——首页行悬停会放大、媒体库 / 搜索结果不会，同一个应用两套行为。
#[test]
fn pointer_move_zooms_a_library_grid_card_without_keyboard_navigation() {
    let app = app_of(1280.0, 800.0);
    let mut library = library_view();
    library.cards = (1..=12)
        .map(|index| nova_ui::CardView::new(format!("l{index:02}"), format!("第 {index} 部")))
        .collect();
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
    assert!(!app.ui().get_nav_active(), "前提：没按过方向键");
    assert!(!app.ui().get_library_zoom_on(), "没悬停时不该放大");

    let targets = settled_targets(&app);
    let (x, y) = target(&targets, "library.card.2").center();
    move_pointer(&app, x, y);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
    assert!(
        app.ui().get_library_zoom_on(),
        "鼠标悬停媒体库卡片应放大（不依赖按过方向键）"
    );

    move_pointer(&app, 4.0, 4.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
    assert!(!app.ui().get_library_zoom_on(), "指针移开后放大层应收起");
}
