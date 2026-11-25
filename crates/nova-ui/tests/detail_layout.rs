// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 详情页内容块几何断言：内容块必须**叠在 hero 下半部**（底端锚定），
//! 且标题 / 简介 / 按钮三块**互不重叠**。
//!
//! 上一轮踩的坑：标题块与简介块各自绑 `y`（标题块 `alignment: end` 贴 backdrop 下缘、
//! 简介块 `y = backdrop-height + space-5`），只把简介块上移 → 标题压在简介上。
//! 现在两块合并成 `detail.slint` 里的单一 `body := VerticalLayout`，本文件锁住：
//!
//! 1. `body.y + body.height + Theme.detail-body-bottom-gap ≈ backdrop-height`（±2px）；
//! 2. `body.y > 0`（内容块顶端确实在 hero 内部，不是被兜底顶到 0）；
//! 3. `title.bottom <= overview.top <= overview.bottom <= buttons.top`（回归断言）。
//!
//! 几何全部来自 `.slint` 自己算的探针（`AppWindow.detail-*`），测试不重写布局公式。

mod common;

use std::time::Duration;

use common::{detail_view, init_backend, install_backdrops};
use nova_ui::{App, AppWindow, Page};
use slint::ComponentHandle;

/// `Theme.detail-body-bottom-gap`（Netflix 官方实测值）。
/// 测试里硬编码一份是故意的：token 被误改时断言要能失败。
const BOTTOM_GAP: f64 = 112.0;

/// 打开详情页并结算入场动画（`reveal` 结算到 1，上浮量归 0）。
fn detail_page(width: u32, height: u32) -> App {
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    // 无头 testing 后端不会为创建后变更重排布局：尺寸必须在 show() 之前定好。
    app.ui().set_window_w(width as f32);
    app.ui().set_window_h(height as f32);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width as f32, height as f32));

    install_backdrops(&app, &["dune"]);
    app.set_detail(&detail_view());
    app.show_page(Page::Detail);
    app.ui().set_detail_focus(0);
    app.ui().show().expect("显示窗口");

    // 无头后端不会自己走时间：把入场动画推到终态再读几何。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));
    app
}

#[derive(Debug)]
struct Geometry {
    backdrop_h: f64,
    body_y: f64,
    body_h: f64,
    title_top: f64,
    title_bottom: f64,
    overview_top: f64,
    overview_bottom: f64,
    buttons_top: f64,
    buttons_bottom: f64,
}

fn geometry(app: &App) -> Geometry {
    let ui = app.ui();
    Geometry {
        backdrop_h: f64::from(ui.get_detail_backdrop_height()),
        body_y: f64::from(ui.get_detail_body_y()),
        body_h: f64::from(ui.get_detail_body_height()),
        title_top: f64::from(ui.get_detail_title_top()),
        title_bottom: f64::from(ui.get_detail_title_bottom()),
        overview_top: f64::from(ui.get_detail_overview_top()),
        overview_bottom: f64::from(ui.get_detail_overview_bottom()),
        buttons_top: f64::from(ui.get_detail_buttons_top()),
        buttons_bottom: f64::from(ui.get_detail_buttons_bottom()),
    }
}

/// 两种验收视口：1280×800（真实启动）与 1920×1080（官方实测视口）。
const VIEWPORTS: [(u32, u32); 2] = [(1280, 800), (1920, 1080)];

#[test]
fn content_block_bottom_is_anchored_112px_above_the_hero_edge() {
    init_backend();
    for (w, h) in VIEWPORTS {
        let app = detail_page(w, h);
        let g = geometry(&app);
        println!("{w}x{h}: {g:?}");

        // 1) hero 高度 = 78vh（官方 845/1080 = 0.782）。
        let expected_backdrop = f64::from(h) * 0.78;
        assert!(
            (g.backdrop_h - expected_backdrop).abs() <= 1.0,
            "{w}x{h}: backdrop 高 {} 应为窗口高的 0.78（{expected_backdrop}）",
            g.backdrop_h
        );

        // 2) 内容块底端 + 112px = hero 下缘（容差 ±2px）。
        let bottom = g.body_y + g.body_h;
        assert!(
            (bottom + BOTTOM_GAP - g.backdrop_h).abs() <= 2.0,
            "{w}x{h}: 内容块底端 {bottom} + {BOTTOM_GAP} 应落在 hero 下缘 {}（差 {}）",
            g.backdrop_h,
            (bottom + BOTTOM_GAP - g.backdrop_h).abs()
        );

        // 3) 内容块顶端要在 hero 内部（不是被兜底到 0）。
        assert!(
            g.body_y > 0.0,
            "{w}x{h}: 内容块顶端 y = {} 应 > 0（内容块被顶到页面顶了）",
            g.body_y
        );
        assert!(
            g.body_y < g.backdrop_h,
            "{w}x{h}: 内容块顶端 y = {} 应 < backdrop 高 {}（官方 35% 位置）",
            g.body_y,
            g.backdrop_h
        );

        // 4) 三个区块**全部**落在 hero 内，且纵向严格递增（互不重叠）。
        assert!(
            g.title_top > 0.0 && g.title_top < g.backdrop_h,
            "{w}x{h}: 标题顶部 {} 应落在 hero 内部（0..{}）",
            g.title_top,
            g.backdrop_h
        );
        assert!(
            g.title_bottom <= g.overview_top,
            "{w}x{h}: 标题底部 {} 压住了简介顶部 {}（上一轮的 bug）",
            g.title_bottom,
            g.overview_top
        );
        assert!(
            g.overview_bottom <= g.buttons_top,
            "{w}x{h}: 简介底部 {} 压住了按钮顶部 {}",
            g.overview_bottom,
            g.buttons_top
        );
        assert!(
            g.buttons_bottom <= g.backdrop_h,
            "{w}x{h}: 按钮底部 {} 超出了 hero 下缘 {}",
            g.buttons_bottom,
            g.backdrop_h
        );
        assert!(
            g.title_top >= g.body_y && g.buttons_bottom <= bottom + 0.5,
            "{w}x{h}: 子项 {}..{} 应落在内容块 {}..{} 之内",
            g.title_top,
            g.buttons_bottom,
            g.body_y,
            bottom
        );
    }
}

#[test]
fn tall_content_falls_back_to_zero_instead_of_going_negative() {
    init_backend();
    // 800×340 的窗口：hero 只有 265px，内容块（≈301px）比 hero 还高，
    // 底端锚定公式会算出负数 → 必须兜底到 0。
    let app = detail_page(800, 340);
    let g = geometry(&app);
    println!("800x340: {g:?}");

    assert_eq!(
        g.body_y, 0.0,
        "内容比 hero 高时 body.y 应兜底到 0（不允许负坐标）"
    );
    assert!(
        g.body_y + g.body_h + BOTTOM_GAP > g.backdrop_h,
        "构造的用例应让底端锚定算出负坐标：{}",
        g.body_y + g.body_h + BOTTOM_GAP - g.backdrop_h
    );
    // 兜底时也不能让视觉顺序乱掉。
    assert!(g.title_bottom <= g.overview_top);
    assert!(g.overview_bottom <= g.buttons_top);
}
