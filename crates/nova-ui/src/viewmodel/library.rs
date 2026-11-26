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

/// 视图被整体替换（后台刷新 / 重新查询）后，焦点下标该落在哪里。
///
/// 焦点按**条目 id** 重新定位，而不是沿用旧下标：刷新会补齐条目、顺序也可能变
/// （「最近添加」按 `updated_at` 排），同一个下标指向的已经是另一条内容。
///
/// - 旧焦点卡片的 `item_id` 在新卡片里还在 → 用它的新下标；
/// - 找不到（条目被移除 / 换了媒体库）→ 退回旧下标并夹到新卡片范围内；
/// - 新卡片为空 → `0`。
pub fn rebase_focus(old: &LibraryView, old_focus: usize, next: &LibraryView) -> usize {
    let fallback = old_focus.min(next.cards.len().saturating_sub(1));
    let Some(id) = old.cards.get(old_focus).map(|card| card.item_id.as_str()) else {
        return fallback;
    };
    next.cards
        .iter()
        .position(|card| card.item_id == id)
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewmodel::card::CardView;

    /// 按 id 造一个媒体库视图（标题随手派生，测试只关心 id 顺序）。
    fn view(ids: &[&str]) -> LibraryView {
        let mut view = LibraryView::new("lib-1", "电影");
        view.cards = ids
            .iter()
            .map(|id| CardView::new(*id, format!("标题 {id}")))
            .collect();
        view
    }

    const TWELVE: [&str; 12] = [
        "l01", "l02", "l03", "l04", "l05", "l06", "l07", "l08", "l09", "l10", "l11", "l12",
    ];

    #[test]
    fn focus_follows_the_item_through_a_rebuild() {
        // 刷新前：12 张，用户停在 l05（下标 4）。
        let old = view(&TWELVE);
        assert_eq!(old.cards[4].item_id, "l05");
        // 刷新后：条目补齐到 15 张，顺序也变了（「最近添加」按加入时间重排）。
        let next = view(&[
            "l09", "l03", "l05", "l01", "l12", "l02", "l11", "l07", "l04", "l10", "l06", "l08",
            "n01", "n02", "n03",
        ]);
        assert_eq!(
            rebase_focus(&old, 4, &next),
            2,
            "焦点应跟着 l05 落到新下标，而不是留在旧下标 4"
        );
        assert_eq!(next.cards[2].item_id, "l05");
    }

    #[test]
    fn removed_item_falls_back_to_the_same_slot_clamped() {
        let old = view(&TWELVE);
        // 条目被移除：l05 不在了。旧下标 4 还有内容，就停在 4。
        let shrunk = view(&["l01", "l02", "l03", "l04", "l06", "l07"]);
        assert_eq!(rebase_focus(&old, 4, &shrunk), 4);
        // 新列表更短：夹到末位。
        let tiny = view(&["l01", "l02"]);
        assert_eq!(rebase_focus(&old, 4, &tiny), 1);
        // 新列表为空：回到 0。
        let empty = view(&[]);
        assert_eq!(rebase_focus(&old, 4, &empty), 0);
    }

    #[test]
    fn out_of_range_focus_is_clamped() {
        // 旧焦点越界（视图刚被清空过）时不该 panic，也不该给出越界下标。
        let old = view(&[]);
        let next = view(&["l01", "l02", "l03"]);
        assert_eq!(rebase_focus(&old, 9, &next), 2);
        assert_eq!(rebase_focus(&old, 0, &next), 0);
    }
}
