// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 首页视图模型。

use crate::viewmodel::card::CardView;
use crate::viewmodel::focus::chunked_rows;
use crate::viewmodel::image::{ImageKey, ImageKind};
use crate::viewmodel::library::LibraryTab;

/// 首页里的一个媒体库行（16:9，标题 = 库名，最多 20 条）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryRowView {
    /// 该行对应的媒体库 id（「查看全部 ›」要跳到这个库的列表页）。
    pub library_id: String,
    /// 行标题（库名）。
    pub title: String,
    /// 行内卡片。
    pub items: Vec<CardView>,
}

/// 首页。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeView {
    /// 顶部大图（没有内容时为 `None`）。
    pub hero: Option<CardView>,
    /// “继续观看”横排。
    pub continue_watching: Vec<CardView>,
    /// “最近添加”网格。
    pub recently_added: Vec<CardView>,
    /// 各媒体库前 20 条的行（继续观看 / 最近添加之后）。
    pub library_rows: Vec<LibraryRowView>,
    /// 顶部导航标签。
    pub library_tabs: Vec<LibraryTab>,
}

impl HomeView {
    /// 空首页：hero 为 `None`，所有列表为空。
    pub fn new() -> Self {
        Self::default()
    }

    /// 带标签的空首页。
    pub fn empty(library_tabs: Vec<LibraryTab>) -> Self {
        Self {
            library_tabs,
            ..Self::default()
        }
    }

    /// 首页是否完全没有内容（hero、两个列表与库行都空）。
    pub fn is_empty(&self) -> bool {
        self.hero.is_none()
            && self.continue_watching.is_empty()
            && self.recently_added.is_empty()
            && self.library_rows.iter().all(|row| row.items.is_empty())
    }

    /// 首页需要的图片键：hero 需要背景图与海报兜底，卡片需要海报。
    pub fn image_keys(&self) -> Vec<ImageKey> {
        let mut keys = Vec::new();
        keys.extend(self.hero_image_keys());
        keys.extend(self.continue_watching.iter().map(CardView::poster_key));
        keys.extend(self.recently_added.iter().map(CardView::poster_key));
        for row in &self.library_rows {
            keys.extend(row.items.iter().map(CardView::poster_key));
        }
        keys
    }

    /// hero 需要的图（背景图 + 海报兜底）；没有 hero 时为空。
    pub fn hero_image_keys(&self) -> Vec<ImageKey> {
        match &self.hero {
            Some(hero) => vec![
                ImageKey::new(hero.item_id.clone(), ImageKind::Backdrop),
                hero.poster_key(),
            ],
            None => Vec::new(),
        }
    }

    /// 首页某一行在 `[first, first + count)` 区间内的图片键。
    ///
    /// `row` 与 [`crate::ImageScope::HomeRow`] 同一套编号：`0` = 继续观看、`1` =
    /// 最近添加、`2 + n` = 第 `n` 个库行。区间由 `.slint` 侧的可见几何算出
    /// （行内 `content-x` 与卡宽 + 页面纵向可见的行），越界区间会被夹住。
    pub fn row_image_keys(&self, row: usize, first: usize, count: usize) -> Vec<ImageKey> {
        let cards: &[CardView] = match row {
            0 => &self.continue_watching,
            1 => &self.recently_added,
            _ => match self.library_rows.get(row - 2) {
                Some(row) => &row.items,
                None => return Vec::new(),
            },
        };
        let start = first.min(cards.len());
        let end = start.saturating_add(count).min(cards.len());
        cards[start..end].iter().map(CardView::poster_key).collect()
    }

    /// 焦点行：继续观看是一行，最近添加按列数切行。
    ///
    /// 继续观看为空时索引直接落在最近添加的第一行。
    pub fn focus_rows(&self, columns: usize) -> Vec<usize> {
        let mut rows = Vec::new();
        if !self.continue_watching.is_empty() {
            rows.push(self.continue_watching.len());
        }
        rows.extend(chunked_rows(self.recently_added.len(), columns));
        rows
    }

    /// 可聚焦条目总数。
    pub fn card_count(&self) -> usize {
        self.continue_watching.len() + self.recently_added.len()
    }

    /// 按扁平焦点索引取卡片。
    pub fn card_at(&self, index: usize) -> Option<&CardView> {
        if index < self.continue_watching.len() {
            return self.continue_watching.get(index);
        }
        self.recently_added
            .get(index - self.continue_watching.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_home_has_no_hero() {
        let home = HomeView::new();
        assert!(home.hero.is_none());
        assert!(home.is_empty());
        assert!(home.focus_rows(6).is_empty());
        assert_eq!(home.card_count(), 0);
        assert_eq!(home.card_at(0), None);
    }

    #[test]
    fn focus_rows_merge_continue_and_recent() {
        let mut home = HomeView::new();
        home.continue_watching = vec![CardView::new("a", "A"), CardView::new("b", "B")];
        home.recently_added = (0..8)
            .map(|index| CardView::new(index.to_string(), "X"))
            .collect();
        assert_eq!(home.focus_rows(6), vec![2, 6, 2]);
        assert_eq!(home.card_at(0).map(|card| card.item_id.as_str()), Some("a"));
        assert_eq!(home.card_at(2).map(|card| card.item_id.as_str()), Some("0"));
        assert_eq!(home.card_at(9).map(|card| card.item_id.as_str()), Some("7"));
        assert_eq!(home.card_at(10), None);
        assert!(!home.is_empty());
    }

    #[test]
    fn image_keys_include_hero_backdrop_and_posters() {
        let mut home = HomeView::new();
        home.hero = Some(CardView::new("hero", "Hero"));
        home.recently_added = vec![CardView::new("x", "X")];
        let keys = home.image_keys();
        assert!(keys.contains(&ImageKey::backdrop("hero")));
        assert!(keys.contains(&ImageKey::poster("hero")));
        assert!(keys.contains(&ImageKey::poster("x")));
    }

    #[test]
    fn row_image_keys_select_the_visible_interval_of_each_row() {
        let mut home = HomeView::new();
        home.continue_watching = (0..3)
            .map(|index| CardView::new(format!("c{index}"), "C"))
            .collect();
        home.recently_added = (0..10)
            .map(|index| CardView::new(format!("r{index}"), "R"))
            .collect();
        home.library_rows = vec![LibraryRowView {
            library_id: "lib".to_owned(),
            title: "库".to_owned(),
            items: (0..4)
                .map(|index| CardView::new(format!("l{index}"), "L"))
                .collect(),
        }];

        assert_eq!(
            home.row_image_keys(0, 1, 2),
            vec![ImageKey::poster("c1"), ImageKey::poster("c2")],
            "继续观看行"
        );
        assert_eq!(
            home.row_image_keys(1, 8, 5),
            vec![ImageKey::poster("r8"), ImageKey::poster("r9")],
            "越界区间要夹住"
        );
        assert_eq!(
            home.row_image_keys(2, 0, 4),
            vec![
                ImageKey::poster("l0"),
                ImageKey::poster("l1"),
                ImageKey::poster("l2"),
                ImageKey::poster("l3"),
            ],
            "库行"
        );
        assert!(home.row_image_keys(3, 0, 1).is_empty(), "越界的行");
        assert!(home.row_image_keys(0, 9, 1).is_empty(), "区间整体在行外");
    }

    #[test]
    fn hero_image_keys_are_empty_without_hero() {
        assert!(HomeView::new().hero_image_keys().is_empty());
    }
}
