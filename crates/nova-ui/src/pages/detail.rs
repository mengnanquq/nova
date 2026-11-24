// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 详情页绑定层：视图模型 → Slint 属性。

use crate::AppWindow;
use crate::pages::assets::ImageStore;
use crate::pages::convert;
use crate::viewmodel::DetailView;

/// 把详情页视图模型写进 Slint。
pub fn apply(ui: &AppWindow, view: &DetailView, images: &ImageStore) {
    ui.set_detail(convert::detail_data(view, images));
}

/// 详情页焦点行：播放按钮 + 每个版本一格。
pub fn focus_rows(view: &DetailView) -> Vec<usize> {
    vec![1 + view.sources.len()]
}
