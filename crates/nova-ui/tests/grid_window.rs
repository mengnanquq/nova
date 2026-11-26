// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 网格窗口化：大库（海外电影 4546 张）只实例化"可见行 ± 缓冲行"。
//!
//! 背景：`PosterGrid` 以前是 `for card in cards`，模型里有几张卡就建几张 `PosterCard`
//! （每张带海报 `Image`、两段 `Text`、`TouchArea`、目标登记绑定）。4546 张的库要
//! 一次性建 4546 个实例：切库慢、常驻内存多、滚动时 UI 线程被绑定求值拖住。
//!
//! 这里钉住的行为：
//! 1. **实例数有上界**：不管库里有几千张，`window-count` 只覆盖视口 ± 缓冲行；
//! 2. **全局下标语义不变**：`window-start` 是全局下标，键盘从第 0 张走到最后一张
//!    焦点与滚动都正确，最后一张卡的 `target-id` / 点击 / 打开详情也对；
//! 3. **滚到底最后一张可点**：目标清单里出现 `library.card.4545`，点它打开最后一条；
//! 4. **窗口跟随视口**：滚轮滚动后窗口区间前移，且始终覆盖视口里的行；
//! 5. 搜索页共用同一个 `PosterGrid`，窗口化同样生效。

mod common;

use std::time::Duration;

use common::{init_backend, library_view, snapshots_dir, write_png};
use nova_ui::{App, AppWindow, CardView, LibraryView, Page, SearchView};
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};

const W: f32 = 1280.0;
const H: f32 = 800.0;

/// 海外电影库的真实规模。
const OVERSEAS: usize = 4546;

/// 实例化张数的上界：6 列 × 16 行 = 96。
///
/// 1280×800 实际窗口 ≈ 3 行可见 + 上下各 2 行缓冲 = 7 行（42 张）；留一倍余量，
/// 让"列数/行高微调"不会误报，但离 4546 有一个数量级以上的差距。
const MAX_INSTANCES: i32 = 96;

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
        .map(|index| {
            CardView::new(format!("m{index:05}"), format!("第 {index} 部")).with_subtitle("2024")
        })
        .collect();
    view
}

fn big_search(count: usize) -> SearchView {
    let cards = (0..count)
        .map(|index| CardView::new(format!("s{index:05}"), format!("结果 {index}")))
        .collect();
    let mut view = SearchView::from_cards("测试", cards);
    view.total = count;
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

fn click(app: &App, x: f32, y: f32) {
    let window = app.ui().window();
    window.dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(x, y),
    });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: slint::LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: slint::LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
}

fn settle(app: &App) -> Vec<nova_ui::TargetInfo> {
    app.refresh_targets();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(60));
    app.targets()
}

/// 4546 张卡：实例化的 `PosterCard` 数量必须有上界（修复前 = 4546）。
#[test]
fn huge_library_instantiates_only_a_bounded_window() {
    let app = app_1280x800();
    let library = big_library(OVERSEAS);
    app.set_library(&library);
    // 数据一落地（还没出首帧）就必须已经窗口化：4546 个实例的代价不能等到
    // 第一次布局才付。
    assert!(
        app.ui().get_library_probe_window_count() <= MAX_INSTANCES,
        "set_library 之后就该是窗口化的（{}）",
        app.ui().get_library_probe_window_count()
    );
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    assert_eq!(
        app.ui().get_library_total_count(),
        OVERSEAS as i32,
        "总张数仍是全量（滚动条 / 内容高度按全部行算）"
    );
    let window_start = app.ui().get_library_probe_window_start();
    let window_count = app.ui().get_library_probe_window_count();
    assert_eq!(window_start, 0, "首屏窗口从第 0 张开始");
    assert!(
        window_count > 0,
        "首屏必须有实例化的卡片（window-count = {window_count}）"
    );
    assert!(
        window_count <= MAX_INSTANCES,
        "4546 张卡只该实例化可见行 ± 缓冲行（上界 {MAX_INSTANCES}），实际 {window_count}"
    );

    // 目标清单只登记**实例化**的卡：数量同样有上界。
    let cards = settle(&app)
        .into_iter()
        .filter(|target| target.kind == "card")
        .count();
    assert!(
        cards > 0 && cards <= MAX_INSTANCES as usize,
        "可见卡的目标登记数应有上界（{cards}）"
    );
}

/// 键盘从第 0 张走到最后一张：焦点、滚动、放大指示都对，窗口跟着走。
#[test]
fn keyboard_walks_from_first_to_last_card_inside_the_window() {
    let app = app_1280x800();
    let library = big_library(OVERSEAS);
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    assert_eq!(app.ui().get_library_focus(), 0);

    // 758 行：一路 Down 到底（每次 Down 是一行，与真实键盘同一路径）。
    // 最后一行只有 4 张（4546 = 757×6 + 4），列偏移保持不变，所以再按 3 次 Right
    // 才到第 4545 张（`move_focus` 到边界停在原地，不绕回）。
    for _ in 0..OVERSEAS.div_ceil(6) {
        press(&app, Key::DownArrow);
    }
    assert_eq!(
        app.focus_index(),
        OVERSEAS - 4,
        "一直按 Down 停到最后一行同列的第一张"
    );
    for _ in 0..3 {
        press(&app, Key::RightArrow);
    }
    assert_eq!(
        app.focus_index(),
        OVERSEAS - 1,
        "6 列网格里一直按方向键必须能停在最后一张"
    );
    assert!(
        app.ui().get_library_zoom_on(),
        "最后一张卡必须有可见指示（说明它确实被实例化了）"
    );
    // 焦点卡（含放大余量）完全在视口内。
    let theme = app.ui().global::<nova_ui::Theme>();
    let pad = f64::from(theme.get_zoom_allowance_y()) / 2.0;
    let top = f64::from(app.ui().get_library_focus_top());
    let bottom = f64::from(app.ui().get_library_focus_bottom());
    let viewport = f64::from(app.ui().get_library_viewport_h());
    assert!(
        top - pad >= -0.5 && bottom + pad <= viewport + 0.5,
        "最后一张卡必须完整可见：top={top} bottom={bottom} viewport={viewport}"
    );
    // 窗口覆盖最后一张卡，且仍有上界。
    let start = app.ui().get_library_probe_window_start();
    let count = app.ui().get_library_probe_window_count();
    assert!(
        (start as usize) < OVERSEAS && (start + count) as usize >= OVERSEAS,
        "窗口必须覆盖最后一张卡（start={start} count={count}）"
    );
    assert!(
        count <= MAX_INSTANCES,
        "滚到底也不能把整个库实例化（{count}）"
    );
    assert!(
        app.ui().get_library_scroll_y() < 0.0,
        "焦点卡在首屏之外时必须滚动"
    );

    // 回到顶部再走一遍：窗口回到第 0 张。
    // 只按"行数 - 1"次 Up：再按一次就出了网格顶层（进入顶栏层），
    // 那时方向键就不再移动网格焦点了。
    for _ in 0..(OVERSEAS.div_ceil(6) - 1) {
        press(&app, Key::UpArrow);
    }
    for _ in 0..3 {
        press(&app, Key::LeftArrow);
    }
    assert_eq!(app.focus_index(), 0);
    assert_eq!(app.ui().get_library_probe_window_start(), 0);
}

/// 滚到底：最后一张卡在目标清单里、坐标可点、点开的是最后一条。
#[test]
fn last_card_is_clickable_after_scrolling_to_the_bottom() {
    let app = app_1280x800();
    let library = big_library(OVERSEAS);
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let opened = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    {
        let opened = std::rc::Rc::clone(&opened);
        app.on_open_detail(move |id| {
            *opened.borrow_mut() = id.to_string();
        });
    }

    // 真实滚轮一直滚到钳底（内容约 178000px，一格约 335px）。
    let mut last = f32::NAN;
    for _ in 0..700 {
        wheel(&app, W / 2.0, H / 2.0, -360.0);
        let now = app.ui().get_library_scroll_y();
        if (now - last).abs() < 0.01 {
            break;
        }
        last = now;
    }
    assert!(last < -100_000.0, "必须真的滚到接近底部（scroll_y={last}）");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));

    let targets = settle(&app);
    let last = targets
        .iter()
        .find(|target| target.id == format!("library.card.{}", OVERSEAS - 1))
        .unwrap_or_else(|| {
            panic!(
                "滚到底后最后一张卡必须登记成目标；当前卡片目标：{:?}",
                targets
                    .iter()
                    .filter(|t| t.kind == "card")
                    .map(|t| t.id.clone())
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        last.is_clickable(),
        "最后一张卡的矩形必须有正面积（{:?}）",
        (last.width, last.height)
    );
    let (x, y) = last.center();
    click(&app, x, y);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
    assert_eq!(
        *opened.borrow(),
        format!("m{:05}", OVERSEAS - 1),
        "点最后一张卡要打开最后一条"
    );
}

/// 滚轮滚动后窗口区间前移，并且覆盖新的视口（滚动不留空洞）。
#[test]
fn window_follows_wheel_scrolling() {
    let app = app_1280x800();
    let library = big_library(OVERSEAS);
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let start_before = app.ui().get_library_probe_window_start();
    for _ in 0..6 {
        wheel(&app, W / 2.0, H / 2.0, -360.0);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));

    let scroll_y = -f64::from(app.ui().get_library_scroll_y());
    assert!(scroll_y > 0.0, "滚轮必须真的滚下去（scroll_y={scroll_y}）");
    let start = app.ui().get_library_probe_window_start();
    let count = app.ui().get_library_probe_window_count();
    assert!(
        start > start_before,
        "窗口起点必须跟着滚动前移（{start_before} → {start}）"
    );
    assert!(count > 0 && count <= MAX_INSTANCES);

    // 视口里的第一行与最后一行都落在窗口里（否则会看到空洞）。
    let row_h = f64::from(app.ui().get_library_grid_row_h());
    let grid_top = f64::from(app.ui().get_library_grid_top());
    let viewport = f64::from(app.ui().get_library_viewport_h());
    let first_visible = (((scroll_y - grid_top) / row_h).floor().max(0.0)) as i32;
    let last_visible = (((scroll_y + viewport - grid_top) / row_h).ceil().max(0.0)) as i32;
    assert!(
        start / 6 <= first_visible && (start + count) as f32 / 6.0 >= last_visible as f32,
        "视口行 {first_visible}..{last_visible} 必须落在窗口 {start}..{} 里",
        start + count
    );

    // 快速滚动之后的画面：人工复核（确认没有空洞 / 错位）。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));
    let snapshot = app.ui().window().take_snapshot().expect("快照");
    write_png(&snapshots_dir().join("grid-window-scrolled.png"), &snapshot);
}

/// 搜索页共用 `PosterGrid`：结果同样只实例化可见行。
#[test]
fn search_results_are_windowed_too() {
    let app = app_1280x800();
    let search = big_search(OVERSEAS);
    app.set_search(&search);
    app.open_search(&search);
    assert_eq!(app.page(), Page::Search);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    assert_eq!(app.ui().get_search_total_count(), OVERSEAS as i32);
    let count = app.ui().get_search_probe_window_count();
    assert!(
        count > 0 && count <= MAX_INSTANCES,
        "搜索结果也只该实例化可见行（{count}）"
    );
    let cards = settle(&app)
        .into_iter()
        .filter(|target| target.kind == "card")
        .count();
    assert!(cards > 0 && cards <= MAX_INSTANCES as usize);
}
