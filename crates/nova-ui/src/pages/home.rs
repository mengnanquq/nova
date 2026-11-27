// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 首页绑定层：视图模型 → Slint 属性。

use crate::AppWindow;
use crate::pages::assets::ImageStore;
use crate::pages::convert;
use crate::viewmodel::{CardView, HomeView};

/// 把首页视图模型写进 Slint。
///
/// 把 Slint 侧当前挂着的 `HomeData` 一起读出来交给 [`convert::home_data`] 复用里面的
/// model：每次新建 `VecModel` 会让整行 `PosterCard` 实例重建（`has-hover` 清零），
/// 指针底下的放大效果在每批海报到达时都会熄灭。
pub fn apply(ui: &AppWindow, view: &HomeView, images: &ImageStore) {
    let previous = ui.get_home();
    ui.set_home(convert::home_data(view, images, Some(&previous)));
}

/// 首页的焦点行（继续观看 / 最近添加 / 各库行，每个视觉行一项）。
pub fn focus_rows(view: &HomeView) -> Vec<usize> {
    view.focus_rows()
}

/// 按扁平焦点索引取卡片。
pub fn card_at(view: &HomeView, index: usize) -> Option<&CardView> {
    view.card_at(index)
}
