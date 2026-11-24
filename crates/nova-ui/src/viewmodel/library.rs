// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体库标签与媒体库页面视图模型。

use crate::viewmodel::card::CardView;
use crate::viewmodel::focus::chunked_rows;
use crate::viewmodel::image::ImageKey;

/// 顶部导航里的一个媒体库标签。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct LibraryTab {
    /// 媒体库标识。
    pub id: String,
    /// 显示名。
    pub title: String,
}

impl LibraryTab {
    /// 构造标签。
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
        }
    }
}

/// 媒体库页面。
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryView {
    /// 媒体库标识。
    pub id: String,
    /// 页面标题。
    pub title: String,
    /// 排序条文案，例如 `"排序：最近添加"`。
    pub sort_label: String,
    /// 网格里的卡片。
    pub cards: Vec<CardView>,
    /// 顶部导航标签。
    pub library_tabs: Vec<LibraryTab>,
    /// 当前标签下标。
    pub current_tab: usize,
}

impl Default for LibraryView {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            sort_label: "排序：最近添加".to_string(),
            cards: Vec::new(),
            library_tabs: Vec::new(),
            current_tab: 0,
        }
    }
}

impl LibraryView {
    /// 构造一个空媒体库页面。
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    /// 页面是否没有卡片。
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// 网格需要的海报键。
    pub fn image_keys(&self) -> Vec<ImageKey> {
        self.cards.iter().map(CardView::poster_key).collect()
    }

    /// 按列数切出的焦点行。
    pub fn focus_rows(&self, columns: usize) -> Vec<usize> {
        chunked_rows(self.cards.len(), columns)
    }

    /// 按扁平索引取卡片。
    pub fn card_at(&self, index: usize) -> Option<&CardView> {
        self.cards.get(index)
    }
}
