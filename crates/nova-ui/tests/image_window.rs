// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 图片按**可见窗口**请求（体检 R6-5）。
//!
//! 背景：`crates/nova-app/src/images.rs` 以前在视图落地时 `keys.take(120)` 截断，
//! 视口外的键永远轮不到；滚动也不补发。结果是海外电影（4546 部）滚到中部全骨架，
//! 首页「最近添加」（200 张）第 ~108 张之后与后续所有库行一张图都不载。
//!
//! 这里钉住界面侧的两个上报口径（下载侧的去重/取消见 nova-app 的单测）：
//! 1. 媒体库网格上报的图片窗口 == 网格实例化窗口（可见行 ± 缓冲行），滚动后前移；
//! 2. 首页每一行只上报自己的可见区间：最近添加第 150 张、最后一个库行进入视口时
//!    都会出现在上报里（旧行为里它们永远到不了 120 张的队列）。

mod common;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use common::{home_filled, init_backend, library_view};
use nova_ui::{App, AppWindow, CardView, HomeView, ImageKey, LibraryRowView, LibraryView};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// 海外电影库的真实规模。
const OVERSEAS: usize = 4546;

fn app_1280x800() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(W);
    app.ui().set_window_h(H);
    app.ui().window().set_size(slint::LogicalSize::new(W, H));
    app
}

fn big_library(count: usize) -> LibraryView {
    let mut view = library_view();
    view.id = "overseas".to_string();
    view.title = "海外电影".to_string();
    view.cards = (0..count)
        .map(|index| CardView::new(format!("m{index:05}"), format!("第 {index} 部")))
        .collect();
    view
}

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn wheel(app: &App, x: f32, y: f32, dy: f32) {
    app.ui().window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(x, y),
    });
    app.ui()
        .window()
        .dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(x, y),
            delta_x: 0.0,
            delta_y: dy,
        });
}

/// 4546 张的库：滚到中部后上报的图片窗口必须跟着视口走。
#[test]
fn scrolling_a_huge_library_requests_the_visible_window() {
    let app = app_1280x800();
    let library = big_library(OVERSEAS);
    let seen: Rc<RefCell<Vec<Vec<ImageKey>>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let seen = Rc::clone(&seen);
        app.on_library_images(move |keys| seen.borrow_mut().push(keys.to_vec()));
    }
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let head = seen
        .borrow()
        .last()
        .cloned()
        .expect("视图落地后网格必须上报可见图片窗口");
    assert_eq!(
        head.first(),
        Some(&ImageKey::poster("m00000")),
        "首屏窗口从第 0 张开始"
    );
    // 上限按几何推导，不是写死的 96：可见行 × 列数 + 上下缓冲行（`grid_window` 的
    // 同一套口径），再加一行容差（首行可能只露出一部分）。
    let columns = app.ui().get_grid_columns().max(1) as usize;
    let viewport_h = f64::from(app.ui().get_library_viewport_h());
    let grid_top = f64::from(app.ui().get_library_grid_top());
    let row_h = f64::from(app.ui().get_library_grid_row_h());
    assert!(row_h > 0.0 && viewport_h > grid_top, "网格几何还没算出来");
    let visible_rows = (((viewport_h - grid_top) / row_h).ceil() as usize).max(1);
    let max_keys = (visible_rows + 1 + 2 * nova_ui::viewmodel::BUFFER_ROWS) * columns;
    assert!(
        head.len() <= max_keys,
        "只该请求可见行 ± 缓冲行（{visible_rows} 可见行 × {columns} 列 + 2×{} 缓冲行\
         ⇒ ≤ {max_keys}，实际 {} 张）",
        nova_ui::viewmodel::BUFFER_ROWS,
        head.len()
    );

    // 滚到中部（与网格窗口化测试同一路径：真实滚轮）。
    for _ in 0..380 {
        wheel(&app, W / 2.0, H / 2.0, -360.0);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));

    let start = app.ui().get_library_probe_window_start() as usize;
    let count = app.ui().get_library_probe_window_count() as usize;
    assert!(start > 100, "必须真的滚到中部（window-start={start}）");
    let tail = seen.borrow().last().cloned().expect("滚动后必须重新上报");
    assert_eq!(
        tail,
        library.image_window_keys(start, count),
        "上报的图片窗口必须与网格实例化窗口一致"
    );
    assert!(
        tail.iter()
            .any(|key| key == &ImageKey::poster(format!("m{start:05}"))),
        "视口里的第一张（m{start:05}）必须在请求集合里"
    );
    assert!(
        !tail.contains(&ImageKey::poster("m00000")),
        "早就滚出视口的第一张不该还在请求集合里"
    );

    // 旧行为：整页 `take(120)`，所有上报键加起来最多 120 个。
    let distinct: HashSet<ImageKey> = seen.borrow().iter().flatten().cloned().collect();
    assert!(
        distinct.len() > 300,
        "滚动过程中请求过的图应远超旧的 120 张上限（实际 {}）",
        distinct.len()
    );
}

/// 首页：最近添加第 150 张、最后一个库行进入视口时都要被请求。
#[test]
fn home_rows_request_their_visible_interval() {
    let app = app_1280x800();
    let mut home = home_with_long_rows();
    home.hero = Some(CardView::new("dune", "沙丘：第二部"));
    let events: Rc<RefCell<Vec<(i32, i32, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let events = Rc::clone(&events);
        app.on_home_images(move |row, first, count| {
            events.borrow_mut().push((row, first, count));
        });
    }
    app.set_home(&home);
    app.show_home();
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    // 首屏是 billboard：往下滚到「最近添加」这一行进入视口（真实滚轮）。
    for _ in 0..4 {
        wheel(&app, W / 2.0, H / 2.0, -360.0);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));

    // 最近添加行（row = 1）从头部开始上报。
    let recent_head = events
        .borrow()
        .iter()
        .rev()
        .find(|(row, _, count)| *row == 1 && *count > 0)
        .copied()
        .expect("首屏必须上报最近添加行的可见窗口");
    assert_eq!(recent_head.1, 0, "最近添加行从第 0 张开始");

    // 键盘 →：在「最近添加」这一行里横向走到第 150 张之后（体检 R7-2 之后行内
    // 前进只归 ← / →；↓ / ↑ 是换行）。行内滚动跟随由 `CardRow` 自己负责。
    //
    // 先把指针移开（滚轮停在某张库卡上，悬停会把焦点同步过去——R7-2 的预期行为），
    // 再用 ↑ 夹到内容第一行、↓ 进「最近添加」。
    app.ui().window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(2.0, 2.0),
    });
    for _ in 0..20 {
        press(&app, Key::UpArrow);
    }
    press(&app, Key::DownArrow);
    press(&app, Key::DownArrow);
    for _ in 0..160 {
        press(&app, Key::RightArrow);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    assert!(
        app.focus_index() >= 2 + 150,
        "焦点应走进最近添加第 150 张之后（实际 {}）",
        app.focus_index()
    );
    let recent_late = events
        .borrow()
        .iter()
        .rev()
        .find(|(row, _, count)| *row == 1 && *count > 0)
        .copied()
        .expect("横向滚动后最近添加行仍要上报");
    // 焦点在最近添加行里的局部下标（首页焦点 = 继续观看张数 + 局部下标）。
    let focused = app.focus_index() as i32 - 2;
    assert!(focused >= 150, "焦点必须已经走过最近添加第 150 张");
    assert!(
        recent_late.1 <= focused && recent_late.1 + recent_late.2 > focused,
        "焦点卡（局部 {focused}）必须落在图片请求窗口 [first={}, first+count={}) 里",
        recent_late.1,
        recent_late.1 + recent_late.2
    );

    // 纵向滚到底：最后一个库行（row = 2 + 2 = 4）进入视口并被请求。
    for _ in 0..12 {
        wheel(&app, W / 2.0, H / 2.0, -360.0);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    let last_row = events
        .borrow()
        .iter()
        .rev()
        .find(|(row, _, count)| *row == 4 && *count > 0)
        .copied();
    assert!(
        last_row.is_some(),
        "最后一个库行进入视口时必须上报图片窗口；事件：{:?}",
        events.borrow()
    );
    assert_eq!(last_row.unwrap().1, 0, "库行从第 0 张开始");
}

/// 离开首页：每一次行（含 hero）都要把空窗口报给 Rust（R6 必修）。
///
/// 修复前 `report-images()` 有 `if (root.page-active && ...)` 守卫，页面离屏时
/// 一次都不上报；Rust 侧又在非首页路由直接 `return`。两边叠起来，`HomeRow(n)`
/// 的 visible 集合永远停在离开时的值。
#[test]
fn leaving_home_reports_empty_windows_for_every_row() {
    let app = app_1280x800();
    let mut home = home_with_long_rows();
    home.hero = Some(CardView::new("dune", "沙丘：第二部"));
    let events: Rc<RefCell<Vec<(i32, i32, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let events = Rc::clone(&events);
        app.on_home_images(move |row, first, count| {
            events.borrow_mut().push((row, first, count));
        });
    }
    app.set_home(&home);
    app.show_home();
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    // 切到媒体库（真实切页入口）：首页每行的 `page-active` 变假，必须上报空区间。
    let library = library_view();
    app.set_library(&library);
    app.open_library(&library);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));

    let events = events.borrow();
    // hero = -1，0 = 继续观看，1 = 最近添加，2..=4 = 三个库行。
    for row in -1..=4 {
        let last = events
            .iter()
            .rev()
            .find(|(event_row, _, _)| *event_row == row)
            .copied()
            .unwrap_or_else(|| panic!("第 {row} 行从未上报过：{events:?}"));
        // hero 的上报固定 `first = 0`（`home.slint` 的 `image-window(-1, 0, count)`），
        // 各行则报 `(-1, 0)`；两者都必须 `count = 0`。
        assert_eq!(last.0, row);
        assert_eq!(
            last.2, 0,
            "第 {row} 行离开首页后必须上报空窗口（实际 {last:?}）"
        );
        if row >= 0 {
            assert_eq!(
                last.1, -1,
                "第 {row} 行离屏时 first 必须是 -1（实际 {last:?}）"
            );
        }
    }
}

/// hero + 2 张继续观看 + 200 张最近添加 + 3 个库行（每行 20 张）。
fn home_with_long_rows() -> HomeView {
    let mut home = home_filled();
    home.continue_watching.truncate(2);
    home.recently_added = (0..200)
        .map(|index| CardView::new(format!("r{index:03}"), format!("最近 {index}")))
        .collect();
    home.library_rows = (0..3)
        .map(|row| LibraryRowView {
            library_id: format!("lib-{row}"),
            title: format!("库 {row}"),
            items: (0..20)
                .map(|index| {
                    CardView::new(format!("l{row}-{index:02}"), format!("库{row} {index}"))
                })
                .collect(),
        })
        .collect();
    home
}
