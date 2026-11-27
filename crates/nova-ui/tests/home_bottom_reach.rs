// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 体检 R7-1：首页**最后一个库行**必须能滚进视口。
//!
//! 修复前的真机现象（1920×1080，滚动钳在 content-y = -3825.07）：第 9 个库行
//! （日番）在视口 y=730，第 10 个库行（日韩电视剧）的表头只在视口底部露出 8px，
//! 整行卡片永远看不到。
//!
//! 根因有两层，都在“内容总高”与“布局真实占位”不一致上：
//!
//! 1. `home_view` 把「首页」导航哨兵（`local::HOME_TAB_ID`）也建成了一条库行。
//!    它下面没有条目 → 空行；而 `.slint` 的纵向布局**不会**剔除 `visible: false`
//!    的行，它照样占一整行高度；内容总高却按“非空行数”算 → 少一整行，最后一行
//!    被挤到 content-height 之外（滚到底也看不到）。
//! 2. 同理，只有一条库行空着（缓存里还没有该库的条目）也能复现。
//!
//! 因此本文件对两种数据形状都断言：滚到最大深度后，最后一行**整行**（表头 + 卡片）
//! 必须落在视口内。800×600 与 1920×1080 两档都跑。

mod common;

use std::time::Duration;

use common::{home_filled, init_backend};
use nova_ui::{App, AppWindow, CardView, HomeView, LibraryRowView, Page, TargetInfo, Theme};
use slint::ComponentHandle;

fn app_at(width: f32, height: f32) -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(width);
    app.ui().set_window_h(height);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    app
}

/// 真实数据的形状：继续观看 2 张 + 最近添加 200 张 + 10 个库行（每行 20 张）。
fn home_like_real() -> HomeView {
    let mut home = home_filled();
    home.continue_watching.truncate(2);
    home.recently_added = (0..200)
        .map(|index| CardView::new(format!("r{index}"), format!("最近 {index}")))
        .collect();
    home.library_rows = library_rows(10);
    home
}

fn library_rows(count: usize) -> Vec<LibraryRowView> {
    (0..count)
        .map(|row| LibraryRowView {
            library_id: format!("lib-{row}"),
            title: format!("库 {row}"),
            items: (0..20)
                .map(|index| {
                    CardView::new(format!("l{row}-{index}"), format!("库 {row} 第 {index}"))
                })
                .collect(),
        })
        .collect()
}

/// 插一条空库行（修复前 `home_view` 就是这么给「首页」哨兵建行的）。
fn home_with_empty_row(at: usize) -> HomeView {
    let mut home = home_like_real();
    home.library_rows.insert(
        at,
        LibraryRowView {
            library_id: "lib-empty".to_owned(),
            title: "空库".to_owned(),
            items: Vec::new(),
        },
    );
    home
}

fn pump() {
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
}

fn find_target(app: &App, id: &str) -> Option<TargetInfo> {
    app.targets().into_iter().find(|target| target.id == id)
}

/// 滚到最大深度，返回 (content_h, viewport_h)。
fn scroll_home_to_bottom(app: &App) -> (f64, f64) {
    let content_h = f64::from(app.ui().get_home_content_h());
    let view_h = f64::from(app.ui().get_home_viewport_h());
    let max_scroll = (content_h - view_h).max(0.0);
    app.ui().invoke_scroll_home_to(-max_scroll as f32);
    pump();
    app.refresh_targets();
    (content_h, view_h)
}

fn assert_row_visible(app: &App, row: usize, view_h: f64, label: &str) {
    let more = find_target(app, &format!("home.row.lib.{row}.more"))
        .unwrap_or_else(|| panic!("{label}: 最后一行（lib.{row}）的「查看全部」不在视口内"));
    assert!(
        f64::from(more.y) >= -0.5 && f64::from(more.y + more.height) <= view_h + 0.5,
        "{label}: 最后一行表头 y={} h={} 超出视口（高 {view_h}）",
        more.y,
        more.height
    );

    // 行内第 1 张卡（横向第一张；行内横向滚动由 R7-2 的跟随逻辑负责）。
    let id = format!("home.row.lib.{row}.card.0");
    let card =
        find_target(app, &id).unwrap_or_else(|| panic!("{label}: 最后一行卡片 {id} 不在视口内"));
    assert!(
        f64::from(card.y) >= -0.5 && f64::from(card.y + card.height) <= view_h + 0.5,
        "{label}: 卡片 {id} y={} h={} 超出视口（高 {view_h}）",
        card.y,
        card.height
    );
}

fn setup(home: &HomeView, width: f32, height: f32) -> App {
    let app = app_at(width, height);
    app.set_home(home);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    pump();
    app.refresh_targets();
    app
}

/// 真实数据形状：滚到底后最后一行（第 10 个库行）整行可见。
#[test]
fn last_library_row_is_reachable_with_real_data_shape() {
    for (width, height) in [(1920.0f32, 1080.0f32), (800.0, 600.0)] {
        let app = setup(&home_like_real(), width, height);
        let (content_h, view_h) = scroll_home_to_bottom(&app);
        // 滚动钳位与内容总高是两份独立的 f32 算术，带容差比较（少算 / 多算都会
        // 让差值远超容差）。
        let max_scroll = content_h - view_h;
        assert!(
            (f64::from(app.ui().get_home_scroll_y()) + max_scroll).abs() <= 0.5,
            "{width}x{height}: 应滚到最大深度 {max_scroll}，实际 {}",
            app.ui().get_home_scroll_y()
        );
        assert_row_visible(&app, 9, view_h, &format!("{width}x{height} 真实形状"));
    }
}

/// 一条空库行（修复前 `home_view` 给「首页」哨兵建的就是这种行）：同样要能滚到底。
#[test]
fn last_library_row_is_reachable_with_an_empty_row() {
    for (width, height) in [(1920.0f32, 1080.0f32), (800.0, 600.0)] {
        for at in [0usize, 4] {
            let app = setup(&home_with_empty_row(at), width, height);
            let (_, view_h) = scroll_home_to_bottom(&app);
            // 最后一行是 lib.10（第 `at` 行是空行，界面上看不到但它占位）。
            assert_row_visible(&app, 10, view_h, &format!("{width}x{height} 空行 at {at}"));
        }
    }
}

/// 媒体库网格与搜索页网格：焦点落到**最后一张**卡时，最后一行必须完整可见
/// （同一份 `PosterGrid` 几何，R7-1 的第二问）。
#[test]
fn library_and_search_last_row_are_reachable() {
    let app = app_at(800.0, 600.0);
    let mut library = common::library_view();
    library.cards = (0..60)
        .map(|index| CardView::new(format!("l{index:02}"), format!("第 {index} 部")))
        .collect();
    app.set_library(&library);
    app.open_library(&library);
    let mut search = common::search_view();
    search.cards = (0..60)
        .map(|index| CardView::new(format!("s{index:02}"), format!("第 {index} 部")))
        .collect();
    search.total = 60;
    app.set_search(&search);
    app.show_page(Page::Library);
    app.ui().show().expect("显示窗口");
    pump();

    // 媒体库：最后一张（下标 59，第 10 行）必须完全可见。
    app.ui().set_library_focus(59);
    app.ui().invoke_scroll_library_to_index(59);
    pump();
    assert_focus_card_visible(
        "媒体库最后一行",
        app.ui().get_library_viewport_h(),
        app.ui().get_library_focus_top(),
        app.ui().get_library_focus_bottom(),
    );

    app.show_page(Page::Search);
    pump();
    app.ui().set_search_focus(59);
    app.ui().invoke_scroll_search_to_index(59);
    pump();
    assert_focus_card_visible(
        "搜索页最后一行",
        app.ui().get_search_viewport_h(),
        app.ui().get_search_focus_top(),
        app.ui().get_search_focus_bottom(),
    );
}

/// 焦点卡必须完整落在视口里（网格探针本来就是视口相对坐标）。
fn assert_focus_card_visible(label: &str, viewport_h: f32, focus_top: f32, focus_bottom: f32) {
    let top = f64::from(focus_top);
    let bottom = f64::from(focus_bottom);
    let view = f64::from(viewport_h);
    assert!(
        top >= -0.5,
        "{label}: 焦点卡上沿 {top} 在视口上沿之上（视口高 {viewport_h}）"
    );
    assert!(
        bottom <= view + 0.5,
        "{label}: 焦点卡下沿 {bottom} 超出视口下沿 {view}"
    );
}

/// 内容总高的各种组合（R7-1 的第三问：少算 / 多算都要抓得到）。
///
/// 判据是**最后一行第 1 张卡的底边**：滚到底后它必须正好落在
/// `viewport_h - Theme.space-8`（内容底部的 `padding-bottom`）。用真实布局出来的
/// 坐标比对，所以 `content-total` 少算一整行（卡底被挤到视口下方）或多算一整行
/// （视口底部多出空白）都会失败；容差只吸收 f32 误差。
#[test]
fn content_total_handles_every_combinations_of_empty_blocks() {
    const TOLERANCE: f64 = 1.0;

    let mut no_hero = home_filled();
    no_hero.hero = None;

    let mut no_continue = home_filled();
    no_continue.continue_watching.clear();

    let mut no_recent = home_filled();
    no_recent.recently_added.clear();

    let mut no_library_rows = home_filled();
    no_library_rows.library_rows.clear();

    let mut one_library_row = home_filled();
    one_library_row.library_rows.truncate(1);

    // R7-C2：继续观看与最近添加**都**空（真机首页的常见形状——没有续播点）。
    let mut no_continue_no_recent = home_filled();
    no_continue_no_recent.continue_watching.clear();
    no_continue_no_recent.recently_added.clear();

    // 搜索过滤后的形状：每个块都只剩 1 张卡（行数不变）。
    let mut filtered = home_filled();
    filtered.continue_watching.truncate(1);
    filtered.recently_added.truncate(1);
    for row in &mut filtered.library_rows {
        row.items.truncate(1);
    }

    let cases: [(&str, HomeView); 7] = [
        ("无 hero", no_hero),
        ("继续观看为空", no_continue),
        ("最近添加为空", no_recent),
        ("继续观看与最近添加为空", no_continue_no_recent),
        ("0 个库行", no_library_rows),
        ("1 个库行", one_library_row),
        ("搜索过滤后", filtered),
    ];

    for (width, height) in [(1920.0f32, 1080.0f32), (800.0, 600.0)] {
        for (label, view) in &cases {
            let app = setup(view, width, height);
            let (content_h, view_h) = scroll_home_to_bottom(&app);
            let max_scroll = (content_h - view_h).max(0.0);
            assert!(
                max_scroll > 0.0,
                "{width}x{height} {label}: 内容必须高于视口（content_h={content_h} view_h={view_h}）"
            );
            assert!(
                (f64::from(app.ui().get_home_scroll_y()) + max_scroll).abs() <= 0.5,
                "{width}x{height} {label}: 滚动范围应是 content_h - view_h = {max_scroll}，实际 {}",
                app.ui().get_home_scroll_y()
            );

            let id = if view.library_rows.is_empty() {
                // 库行全没了：最后一个块是「最近添加」（本组用例里它非空）。
                "home.row.recent.card.0".to_owned()
            } else {
                format!("home.row.lib.{}.card.0", view.library_rows.len() - 1)
            };
            let card = find_target(&app, &id)
                .unwrap_or_else(|| panic!("{width}x{height} {label}: 最后一张卡 {id} 不在视口内"));
            let space = f64::from(app.ui().global::<Theme>().get_space_8());
            let expected_bottom = view_h - space;
            let bottom = f64::from(card.y + card.height);
            assert!(
                (bottom - expected_bottom).abs() <= TOLERANCE,
                "{width}x{height} {label}: 最后一行卡底应贴在视口底 {expected_bottom}\
                 （viewport_h - space-8）上，实际 {bottom}（差 {}）",
                bottom - expected_bottom
            );
        }
    }
}

/// R7-C2：空的「继续观看」/「最近添加」不能占一整块高度。
///
/// 真机（800×600）滚到中段时出现约半屏的纯黑带：`.slint` 的纵向布局不会剔除
/// `visible: false` 的行，空行照样占一整块（卡片高 + 表头），而画面上什么都没有。
/// 判据：把一行清空后内容总高必须**减少这一块**（而不是 0），两行都空时减量等于
/// 两次单独减量之和；同时「内容总高 = 布局真实占位」不能因此被破坏（那条由
/// `content_total_handles_every_combinations_of_empty_blocks` 守着）。
#[test]
fn empty_continue_and_recent_rows_do_not_reserve_a_full_block() {
    let content_h = |view: &HomeView, width: f32, height: f32| -> f64 {
        let app = setup(view, width, height);
        f64::from(app.ui().get_home_content_h())
    };

    let full = home_filled();
    let mut no_continue = home_filled();
    no_continue.continue_watching.clear();
    let mut no_recent = home_filled();
    no_recent.recently_added.clear();
    let mut neither = home_filled();
    neither.continue_watching.clear();
    neither.recently_added.clear();

    for (width, height) in [(1920.0f32, 1080.0f32), (800.0, 600.0)] {
        let a = content_h(&full, width, height);
        let b = content_h(&no_continue, width, height);
        let c = content_h(&no_recent, width, height);
        let d = content_h(&neither, width, height);

        assert!(
            a - b > 1.0,
            "{width}x{height}: 清空「继续观看」后内容总高必须减少一整块（实际 {a} → {b}）"
        );
        assert!(
            a - c > 1.0,
            "{width}x{height}: 清空「最近添加」后内容总高必须减少一整块（实际 {a} → {c}）"
        );
        assert!(
            ((a - b) - (c - d)).abs() <= 1.0,
            "{width}x{height}: 两行都空的减量应等于两次单独减量之和（{} vs {}）",
            a - b,
            c - d
        );
    }
}

/// R7-空白：空的继续观看 / 最近添加行**不占间距**。
///
/// Slint 的 `VerticalLayout` 对 0 高的子项照样算 `spacing`，于是空行高 0 却仍然
/// 吃掉一个 `section-gap`：两行都空时 hero 底到首个可见行标题顶是 3 个 gap，
/// 单行空时是 2 个。修法是把间距挪进每行自己的上边距（空行高 0 ⇒ 间距一起消失）。
///
/// 判据：hero 底（`billboard-slot`，hero 块就是这个高度）到首个可见行标题顶的
/// 距离恒等于**一个** `section-gap`——三种形状（都不空 / 单行空 / 两行都空）都断言。
/// 行标题顶取自真实布局：继续观看 / 最近添加用行自己的 `*-title-bottom` 探针减标题高，
/// 库行用「查看全部 ›」目标的窗口 y（`more` 就压在标题行上）。
#[test]
fn an_empty_top_row_does_not_add_a_section_gap() {
    let mut neither = home_filled();
    neither.continue_watching.clear();
    neither.recently_added.clear();

    let mut no_continue = home_filled();
    no_continue.continue_watching.clear();

    let mut no_recent = home_filled();
    no_recent.recently_added.clear();

    let cases: [(&str, HomeView); 4] = [
        ("都不空", home_filled()),
        ("继续观看为空", no_continue),
        ("最近添加为空", no_recent),
        ("两行都空", neither),
    ];

    for (width, height) in [(1920.0f32, 1080.0f32), (800.0, 600.0)] {
        for (label, view) in &cases {
            let app = setup(view, width, height);
            // 行入场动画（`CardRow.rise`，`Theme.motion-row` = 540ms）跑完再量：
            // `row-title-bottom` 含 `rise`，动画未完会少报几像素。
            for _ in 0..4 {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
            }
            app.refresh_targets();
            assert_eq!(
                app.ui().get_home_scroll_y(),
                0.0,
                "{width}x{height} {label}: 前提是页面停在顶部"
            );
            let theme = app.ui().global::<Theme>();
            let gap = f64::from(theme.get_section_gap());
            let title_h = f64::from(theme.get_section_title_height());
            drop(theme);

            let hero_bottom = f64::from(app.ui().get_hero_bottom());
            let first_title_top = if !view.continue_watching.is_empty() {
                f64::from(app.ui().get_row0_title_bottom()) - title_h
            } else if !view.recently_added.is_empty() {
                f64::from(app.ui().get_row1_title_bottom()) - title_h
            } else {
                let more = find_target(&app, "home.row.lib.0.more").unwrap_or_else(|| {
                    panic!("{width}x{height} {label}: 首个库行的「查看全部」缺失")
                });
                f64::from(more.y)
            };

            assert!(
                ((first_title_top - hero_bottom) - gap).abs() <= 0.5,
                "{width}x{height} {label}: hero 底 {hero_bottom} 到首个可见行标题顶 \
                 {first_title_top} 应恰为一个 section-gap {gap}（实际 {}）",
                first_title_top - hero_bottom
            );
        }
    }
}

/// R7-空白（无 hero 分支）：**没有 hero** 时首个可见行紧贴页面顶端。
///
/// 上一个测试只覆盖了有 hero 的三种形状；无 hero（缓存还没回来 / 直接进了首页的
/// 空 billboard）时 hero 槽位是 0，首个可见行的标题顶也必须正好是 0——不能多出
/// 一个 `section-gap`。覆盖「继续观看非空」（首行是 row0）与「继续观看为空」
/// （首行是 row1 最近添加）两种形状，并用行自己的 `cards-offset` 交叉校验：
/// 标题顶到卡片上沿只差 `title-off`，说明行块本身也从页面顶端开始。
#[test]
fn without_a_hero_the_first_row_starts_at_the_page_top() {
    let mut with_continue = home_filled();
    with_continue.hero = None;
    let mut no_continue = with_continue.clone();
    no_continue.continue_watching.clear();

    for (width, height) in [(1920.0f32, 1080.0f32), (800.0, 600.0)] {
        for (label, view) in [
            ("无 hero + 继续观看非空", with_continue.clone()),
            ("无 hero + 继续观看为空", no_continue.clone()),
        ] {
            let app = setup(&view, width, height);
            // 行入场动画（`CardRow.rise`）跑完再量，否则标题顶会少报几像素。
            for _ in 0..4 {
                i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
            }
            app.refresh_targets();

            let theme = app.ui().global::<Theme>();
            let gap = f64::from(theme.get_section_gap());
            let title_h = f64::from(theme.get_section_title_height());
            let title_off = f64::from(theme.get_row_title_gap()) + title_h;
            drop(theme);

            assert_eq!(
                app.ui().get_home_scroll_y(),
                0.0,
                "{width}x{height} {label}: 前提是页面停在顶部"
            );
            let hero_bottom = f64::from(app.ui().get_hero_bottom());
            assert!(
                hero_bottom.abs() <= 0.5,
                "{width}x{height} {label}: 没有 hero 时 hero 槽位必须是 0（实际 {hero_bottom}）"
            );

            // 首个可见行：标题下沿探针（`row-title-bottom`）+ 该行第 1 张卡的真实窗口
            // y（`card.0` 目标，交叉校验行块本身也从页面顶端开始）。
            let (title_bottom, card_target) = if !view.continue_watching.is_empty() {
                (
                    f64::from(app.ui().get_row0_title_bottom()),
                    "home.row.continue.card.0",
                )
            } else {
                (
                    f64::from(app.ui().get_row1_title_bottom()),
                    "home.row.recent.card.0",
                )
            };
            let first_title_top = title_bottom - title_h;
            assert!(
                first_title_top.abs() <= 0.5,
                "{width}x{height} {label}: 首个可见行标题顶应正好是 0（实际 {first_title_top}，\
                 多出了 {}）",
                first_title_top.abs()
            );
            assert!(
                (first_title_top - hero_bottom).abs() < gap,
                "{width}x{height} {label}: hero 底 {hero_bottom} 到首个可见行标题顶 \
                 {first_title_top} 之间不该有 section-gap {gap}（实际 {}）",
                first_title_top - hero_bottom
            );
            let card = find_target(&app, card_target)
                .unwrap_or_else(|| panic!("{width}x{height} {label}: {card_target} 缺失"));
            let cards_top = f64::from(card.y);
            assert!(
                ((cards_top - first_title_top) - title_off).abs() <= 0.5,
                "{width}x{height} {label}: 卡片上沿 {cards_top} 与标题顶 {first_title_top} \
                 的差应恰为标题高 + 标题下距 {title_off}"
            );
        }
    }
}
