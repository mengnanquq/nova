// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 首页视图模型。

use crate::viewmodel::card::CardView;
use crate::viewmodel::focus::chunked_rows;
use crate::viewmodel::image::{ImageKey, ImageKind};
use crate::viewmodel::library::LibraryTab;

/// 首页。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeView {
    /// 顶部大图（没有内容时为 `None`）。
    pub hero: Option<CardView>,
    /// “继续观看”横排。
    pub continue_watching: Vec<CardView>,
    /// “最近添加”网格。
    pub recently_added: Vec<CardView>,
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

    /// 首页是否完全没有内容（hero 与两个列表都空）。
    pub fn is_empty(&self) -> bool {
        self.hero.is_none() && self.continue_watching.is_empty() && self.recently_added.is_empty()
    }

    /// 首页需要的图片键：hero 需要背景图与海报兜底，卡片需要海报。
    pub fn image_keys(&self) -> Vec<ImageKey> {
        let mut keys = Vec::new();
        if let Some(hero) = &self.hero {
            keys.push(ImageKey::new(hero.item_id.clone(), ImageKind::Backdrop));
            keys.push(hero.poster_key());
        }
        keys.extend(self.continue_watching.iter().map(CardView::poster_key));
        keys.extend(self.recently_added.iter().map(CardView::poster_key));
        keys
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
}
