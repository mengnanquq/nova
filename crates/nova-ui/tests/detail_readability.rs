// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 详情页文字可读性：标题 / 原片名 / 元信息行叠在亮背景上时必须有压暗保护（R6-7）。
//!
//! 真机 800×600 的「一念关山」详情页：亮背景（灯笼、白墙、人物）上，原片名与
//! `2023 · 48 分 · 剧情 / 动作冒险` 几乎不可读——底部渐变从 backdrop 40% 才开始，
//! 小窗口下内容块被顶到导航栏下沿，整块文字落在渐变还很淡的区域。
//!
//! 修复给内容块后面的左侧渐变（与首页 hero 同款 `Theme.scrim` → 透明）。这里用
//! 像素断言锁住它：背景足够亮时，标题行左侧的背景必须被压暗到可读范围。

mod common;

use std::time::Duration;

use common::{detail_view, init_backend, install_backdrops};
use nova_ui::{App, AppWindow, Page};
use slint::ComponentHandle;

/// 800×600：真机上最难读的那一档。
const W: f32 = 800.0;
const H: f32 = 600.0;

fn detail_page() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(W);
    app.ui().set_window_h(H);
    app.ui().window().set_size(slint::LogicalSize::new(W, H));
    install_backdrops(&app, &["dune"]);
    app.set_detail(&detail_view());
    app.show_page(Page::Detail);
    app.ui().show().expect("显示窗口");
    // 入场动画推到终态（`reveal = 1`，上浮量归 0）。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));
    app
}

fn luminance(pixel: i_slint_core::graphics::Rgba8Pixel) -> f64 {
    0.2126 * f64::from(pixel.r) + 0.7152 * f64::from(pixel.g) + 0.0722 * f64::from(pixel.b)
}

/// 标题行左侧（文字起点之前的背景）必须被压暗：那里没有任何文字，
/// 读到的是背景图的亮度，修复前是亮背景、修复后被左侧渐变盖住。
#[test]
fn title_area_gets_a_readability_scrim() {
    let app = detail_page();
    let title_top = f64::from(app.ui().get_detail_title_top()).max(0.0) as u32;
    let title_bottom = f64::from(app.ui().get_detail_title_bottom()).max(0.0) as u32;
    assert!(
        title_bottom > title_top + 20,
        "标题块几何异常：{title_top}..{title_bottom}"
    );

    let snapshot = app.ui().window().take_snapshot().expect("详情页快照");
    let width = snapshot.width() as usize;
    let buffer = snapshot.as_slice();
    let mut sum = 0.0;
    let mut count = 0usize;
    // x 0..32 在 `Theme.content-gutter`（48px）左侧，是纯背景。
    for y in title_top..title_bottom {
        for x in 0..32usize {
            let index = (y as usize) * width + x;
            sum += luminance(buffer[index]);
            count += 1;
        }
    }
    let mean = sum / count as f64;
    println!("title area mean luminance after scrim: {mean:.1}");
    assert!(
        mean < 96.0,
        "标题行左侧背景平均亮度 {mean:.1}，亮背景上文字不可读（阈值 96；\
         修复前的亮背景实测 136.3）"
    );
}
