// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 搜索页绑定层：视图模型 → Slint 属性。
//!
//! 与媒体库页同构：这里只做"塞数据"和"焦点行 / 卡片"的薄封装，
//! 回调注册在 [`crate::pages::App`]（`set_search` / `on_search_input` / …）。
//! 查询本身（本地缓存 + 服务端）在上层 `nova-app`，界面不联网。

use crate::AppWindow;
use crate::pages::assets::ImageStore;
use crate::pages::convert;
use crate::viewmodel::{CardView, SearchView};

/// 把搜索页视图模型写进 Slint。
///
/// `current_tab` 是顶栏高亮下标（`-1` = 不选中任何标签）：搜索页不属于任何库。
pub fn apply(
    ui: &AppWindow,
    view: &SearchView,
    tabs: &[crate::viewmodel::LibraryTab],
    current_tab: i32,
    images: &ImageStore,
) {
    ui.set_search_data(convert::search_data(view, tabs, current_tab, images));
}

/// 搜索结果的焦点行。
pub fn focus_rows(view: &SearchView, columns: usize) -> Vec<usize> {
    view.focus_rows(columns)
}

/// 按扁平焦点索引取卡片。
pub fn card_at(view: &SearchView, index: usize) -> Option<&CardView> {
    view.card_at(index)
}
