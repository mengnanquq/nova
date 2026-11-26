// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 顶栏布局：窄窗口下库标签不能与右侧搜索控件重叠。
//!
//! 回归（800×600 实测）：媒体库页「日番」标签的目标矩形是 x733..761，而搜索按钮是
//! x732..768 —— 两个矩形相交，调试通道按标签坐标点击会点到搜索框。原因是标签行
//! （Flickable 的内容）比标签区宽，`absolute-position` 报的是**未裁剪**的布局坐标，
//! 而放不下的标签其实已经被裁在视口外。
//!
//! 断言用 `App::targets()` 的坐标（与 `--debug-socket` 的 `targets` 同一份数据）：
//! 在 800 / 1024 / 1280 三个宽度下，任何 `*.nav.tab.*` 目标都不得与任何
//! `*.nav.search*` 目标相交。

mod common;

use std::time::Duration;

use common::{init_backend, library_view};
use nova_ui::{App, AppWindow, LibraryTab, Page, TargetInfo};
use slint::ComponentHandle;

/// 真实媒体库的标签行（10 个库 + 「首页」）：只有这么多标签才会在 800px 下溢出。
fn real_tabs() -> Vec<LibraryTab> {
    [
        "首页",
        "动画电影",
        "儿童节目",
        "国产电视剧",
        "国产动画片",
        "海外电影",
        "华语电影",
        "纪录片",
        "欧美电视剧",
        "日番",
        "日韩电视剧",
    ]
    .iter()
    .enumerate()
    .map(|(index, title)| LibraryTab::new(format!("tab-{index}"), *title))
    .collect()
}

fn setup() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let mut view = library_view();
    view.library_tabs = real_tabs();
    app.set_library(&view);
    app.show_page(Page::Library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    app
}

/// 推进代次让 `for` 里的标签重新登记，再读清单。
fn settle(app: &App) -> Vec<TargetInfo> {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    app.targets()
}

fn rects_intersect(left: &TargetInfo, right: &TargetInfo) -> bool {
    left.x < right.x + right.width
        && right.x < left.x + left.width
        && left.y < right.y + right.height
        && right.y < left.y + left.height
}

#[test]
fn navbar_tabs_never_overlap_the_search_control() {
    let app = setup();

    for (width, height) in [(800.0, 600.0), (1024.0, 576.0), (1280.0, 720.0)] {
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(width, height));
        // testing 后端不一定为 `Window.width` 发 `changed`：同时写镜像值，
        // 保证 `vw` 派生的 gutter / 间距按新宽度重算（见 `app.slint`）。
        app.ui().set_window_w(width);
        app.ui().set_window_h(height);

        let targets = settle(&app);
        let tabs: Vec<&TargetInfo> = targets
            .iter()
            .filter(|target| target.id.contains(".nav.tab."))
            .collect();
        let search: Vec<&TargetInfo> = targets
            .iter()
            .filter(|target| target.id.contains(".nav.search"))
            .collect();

        assert!(
            !search.is_empty(),
            "{width}x{height}: 顶栏搜索入口必须出现在清单里"
        );
        assert!(
            !tabs.is_empty(),
            "{width}x{height}: 标签行不能整个消失（清单：{:?}）",
            targets.iter().map(|t| &t.id).collect::<Vec<_>>()
        );

        for tab in &tabs {
            for control in &search {
                assert!(
                    !rects_intersect(tab, control),
                    "{width}x{height}: 标签 {} 「{}」({}..{}, {}..{}) 与 {} ({:?}) ({}..{}, {}..{}) 重叠",
                    tab.id,
                    tab.label,
                    tab.x,
                    tab.x + tab.width,
                    tab.y,
                    tab.y + tab.height,
                    control.id,
                    control.kind,
                    control.x,
                    control.x + control.width,
                    control.y,
                    control.y + control.height,
                );
                assert!(
                    tab.x + tab.width <= control.x,
                    "{width}x{height}: 标签 {} 的右缘 {} 越过了 {} 的左缘 {}",
                    tab.id,
                    tab.x + tab.width,
                    control.id,
                    control.x,
                );
            }
        }
    }
}

/// 800px 下确实有一部分标签放不下（否则上面的断言是空的）：
/// 清单里少了标签，且留下的标签都在搜索控件左侧。
#[test]
fn narrow_window_drops_the_tabs_that_do_not_fit() {
    let app = setup();
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(800.0, 600.0));
    app.ui().set_window_w(800.0);
    app.ui().set_window_h(600.0);

    let targets = settle(&app);
    let tabs: Vec<&TargetInfo> = targets
        .iter()
        .filter(|target| target.id.contains(".nav.tab."))
        .collect();
    let search = targets
        .iter()
        .find(|target| target.id.contains(".nav.search"))
        .expect("顶栏搜索入口");

    assert!(
        tabs.len() < real_tabs().len(),
        "800px 下应有标签放不下：清单里却有 {} 个（总共 {} 个）",
        tabs.len(),
        real_tabs().len()
    );
    for tab in tabs {
        assert!(
            tab.x + tab.width <= search.x,
            "{} 的右缘 {} 必须停在搜索控件左缘 {} 之前",
            tab.id,
            tab.x + tab.width,
            search.x
        );
    }
}
