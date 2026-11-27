// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 搜索页视图模型（Netflix 式的独立结果页）。
//!
//! 这一层只认 [`CardView`]：**媒体模型 → 卡片的映射不在这里**，而在
//! `nova-app` 的 `views`（`card_view`，与首页/媒体库共用同一个函数），
//! 因为 `nova-ui` 不依赖 `nova-core`（见 crate 文档的定位）。
//! 「只保留电影 / 剧集」的类型过滤同样在那一侧完成。

use crate::viewmodel::card::CardView;
use crate::viewmodel::focus::chunked_rows;
use crate::viewmodel::image::ImageKey;

/// 搜索页状态。
///
/// `.slint` 侧用整数编号（[`SearchState::index`]）区分分支，所以这个枚举
/// 必须保持"只加不改"的编号顺序。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchState {
    /// 还没有开始搜索（搜索框为空）。
    Idle,
    /// 正在查询（本地已无结果时的占位）。
    Loading,
    /// 有结果。
    Ready,
    /// 查询完成但没有任何命中。
    Empty,
    /// 查询失败，附带一句可读的说明。
    Error(String),
}

impl SearchState {
    /// `.slint` 里的状态编号。
    pub const fn index(&self) -> i32 {
        match self {
            Self::Idle => 0,
            Self::Loading => 1,
            Self::Ready => 2,
            Self::Empty => 3,
            Self::Error(_) => 4,
        }
    }

    /// 错误说明；非错误态为 `None`。
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error(message) => Some(message.as_str()),
            _ => None,
        }
    }

    /// 是否处于"有结果"态。
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// 搜索页。
#[derive(Clone, Debug, PartialEq)]
pub struct SearchView {
    /// 当前查询词（回填到搜索框）。
    pub term: String,
    /// 结果网格里的卡片（只有电影 / 剧集）。
    pub cards: Vec<CardView>,
    /// 查询状态。
    pub state: SearchState,
    /// 命中数：服务端给的总数，否则等于本页卡片数。
    pub total: usize,
}

impl Default for SearchView {
    fn default() -> Self {
        Self {
            term: String::new(),
            cards: Vec::new(),
            state: SearchState::Idle,
            total: 0,
        }
    }
}

impl SearchView {
    /// 一个"搜索中…"的空视图。
    pub fn new(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            state: SearchState::Loading,
            ..Self::default()
        }
    }

    /// 由已经映射好的卡片构造：有卡片 → `Ready`，没有 → `Empty`。
    pub fn from_cards(term: &str, cards: Vec<CardView>) -> Self {
        let total = cards.len();
        let state = if cards.is_empty() {
            SearchState::Empty
        } else {
            SearchState::Ready
        };
        Self {
            term: term.to_owned(),
            cards,
            state,
            total,
        }
    }

    /// 失败态。
    pub fn error(term: &str, message: impl Into<String>) -> Self {
        Self {
            term: term.to_owned(),
            state: SearchState::Error(message.into()),
            ..Self::default()
        }
    }

    /// 覆盖命中数（服务端的 `TotalRecordCount` 可能大于本页卡片数）。
    pub fn with_total(mut self, total: usize) -> Self {
        self.total = total.max(self.cards.len());
        self
    }

    /// 是否没有卡片。
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// 网格需要的海报键。
    pub fn image_keys(&self) -> Vec<ImageKey> {
        self.cards.iter().map(CardView::poster_key).collect()
    }

    /// 可见窗口 `[start, start + count)` 内的海报键（大库按视口请求图片用）。
    pub fn image_window_keys(&self, start: usize, count: usize) -> Vec<ImageKey> {
        let start = start.min(self.cards.len());
        let end = start.saturating_add(count).min(self.cards.len());
        self.cards[start..end]
            .iter()
            .map(CardView::poster_key)
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, title: &str) -> CardView {
        CardView::new(id, title)
    }

    #[test]
    fn new_view_is_loading() {
        let view = SearchView::new("沙丘");
        assert_eq!(view.term, "沙丘");
        assert_eq!(view.state, SearchState::Loading);
        assert!(view.cards.is_empty());
        assert_eq!(view.total, 0);
        assert_eq!(SearchView::default().state, SearchState::Idle);
    }

    #[test]
    fn from_cards_counts_hits_and_marks_ready() {
        let view =
            SearchView::from_cards("沙丘", vec![card("a", "沙丘"), card("b", "沙丘：第二部")]);
        assert_eq!(view.state, SearchState::Ready);
        assert_eq!(view.total, 2);
        assert_eq!(view.cards.len(), 2);
        assert!(!view.is_empty());
        assert_eq!(
            view.image_keys(),
            vec![ImageKey::poster("a"), ImageKey::poster("b")]
        );
    }

    #[test]
    fn empty_hits_become_empty_state() {
        let view = SearchView::from_cards("没有这片", Vec::new());
        assert_eq!(view.state, SearchState::Empty);
        assert_eq!(view.total, 0);
        assert!(view.is_empty());
        assert!(view.image_keys().is_empty());
    }

    #[test]
    fn image_window_keys_cover_only_the_requested_interval() {
        let view = SearchView::from_cards(
            "沙丘",
            (0..10)
                .map(|index| card(&format!("s{index}"), "x"))
                .collect(),
        );
        assert_eq!(
            view.image_window_keys(8, 5),
            vec![ImageKey::poster("s8"), ImageKey::poster("s9")],
            "越界区间要夹住"
        );
        assert!(view.image_window_keys(10, 3).is_empty());
    }

    #[test]
    fn error_state_carries_a_readable_message() {
        let view = SearchView::error("沙丘", "搜索失败，请稍后重试");
        assert_eq!(view.term, "沙丘");
        assert_eq!(
            view.state,
            SearchState::Error("搜索失败，请稍后重试".to_string())
        );
        assert_eq!(view.state.error_message(), Some("搜索失败，请稍后重试"));
        assert!(view.cards.is_empty());
        // 非错误态没有说明。
        assert_eq!(SearchState::Ready.error_message(), None);
    }

    #[test]
    fn state_indices_are_stable_for_slint() {
        assert_eq!(SearchState::Idle.index(), 0);
        assert_eq!(SearchState::Loading.index(), 1);
        assert_eq!(SearchState::Ready.index(), 2);
        assert_eq!(SearchState::Empty.index(), 3);
        assert_eq!(SearchState::Error(String::new()).index(), 4);
        assert!(SearchState::Ready.is_ready());
        assert!(!SearchState::Loading.is_ready());
    }

    #[test]
    fn total_keeps_server_count_but_never_below_page() {
        let view = SearchView::from_cards("dune", vec![card("a", "Dune")]).with_total(37);
        assert_eq!(view.total, 37);
        let view = SearchView::from_cards("dune", vec![card("a", "Dune")]).with_total(0);
        assert_eq!(view.total, 1);
    }

    #[test]
    fn focus_rows_and_card_at_follow_the_grid() {
        let cards = (0..7)
            .map(|index| card(&index.to_string(), "片"))
            .collect::<Vec<_>>();
        let view = SearchView::from_cards("片", cards);
        // 每行的条目数（不是起始下标）：7 张卡 → 6 + 1。
        assert_eq!(view.focus_rows(6), vec![6, 1]);
        assert_eq!(view.card_at(6).map(|card| card.item_id.as_str()), Some("6"));
        assert!(view.card_at(7).is_none());
        assert!(SearchView::default().focus_rows(6).is_empty());
    }
}
