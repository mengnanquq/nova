// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体库绑定层：视图模型 → Slint 属性。

use crate::AppWindow;
use crate::pages::assets::ImageStore;
use crate::pages::convert;
use crate::viewmodel::{CardView, LibraryView};

/// 把媒体库视图模型写进 Slint。
pub fn apply(ui: &AppWindow, view: &LibraryView, images: &ImageStore) {
    ui.set_library(convert::library_data(view, images));
}

/// 媒体库的焦点行。
pub fn focus_rows(view: &LibraryView, columns: usize) -> Vec<usize> {
    view.focus_rows(columns)
}

/// 按扁平焦点索引取卡片。
pub fn card_at(view: &LibraryView, index: usize) -> Option<&CardView> {
    view.card_at(index)
}
