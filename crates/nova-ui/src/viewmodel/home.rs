// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 首页视图模型。

use crate::viewmodel::card::CardView;
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

    /// 焦点行：**每个视觉行一行**。
    ///
    /// 继续观看 / 最近添加 / 各库行在界面上都是横向 Flickable（`home.slint` 的
    /// `CardRow`），不是网格——所以「最近添加」那 200 张不能按列数切块：切块会让
    /// `↓` / `↑` 在同一行里横跳（体检 R7-2：每次步进 6 张），而库行根本进不去
    /// （行数只有 2，`↓` 到底就停）。
    ///
    /// **长度为 0 的行一律跳过**：`move_up` 用 `rows.first()` 判断“是否在第一行”，
    /// 留住一个空行会让它永远判为第一行，`↑` 变成原地 no-op（库里第一行是空库时
    /// 就回不到 hero / 顶栏）。
    pub fn focus_rows(&self) -> Vec<usize> {
        let mut rows = Vec::new();
        if !self.continue_watching.is_empty() {
            rows.push(self.continue_watching.len());
        }
        if !self.recently_added.is_empty() {
            rows.push(self.recently_added.len());
        }
        rows.extend(
            self.library_rows
                .iter()
                .map(|row| row.items.len())
                .filter(|len| *len > 0),
        );
        rows
    }

    /// 扁平焦点索引所在的可视行：`0` = 继续观看、`1` = 最近添加、`2 + n` = 库行。
    ///
    /// 这个编号是**界面行号**（与 `.slint` 的 `keyboard-scroll-row` /
    /// `library-row-starts` 同一套），和 [`Self::focus_rows`] 的下标不同：后者会
    /// 跳过空行。首页滚动要按可视行算行上沿，所以不能复用过滤后的下标。
    pub fn focus_row_at(&self, index: usize) -> Option<usize> {
        if index < self.continue_watching.len() {
            return Some(0);
        }
        let index = index - self.continue_watching.len();
        if index < self.recently_added.len() {
            return Some(1);
        }
        let mut index = index - self.recently_added.len();
        for (row, library_row) in self.library_rows.iter().enumerate() {
            if index < library_row.items.len() {
                return Some(2 + row);
            }
            index -= library_row.items.len();
        }
        None
    }

    /// 各库行在扁平焦点索引里的起点（与 `library_rows` 一一对应，空行也保留）。
    ///
    /// `.slint` 侧要靠它把「页面级扁平焦点」换算成「某一行里的局部下标」：
    /// 库行是 `for` 重复元素，页面没法按行号去问某一行。
    pub fn library_row_starts(&self) -> Vec<usize> {
        let mut start = self.continue_watching.len() + self.recently_added.len();
        self.library_rows
            .iter()
            .map(|row| {
                let at = start;
                start += row.items.len();
                at
            })
            .collect()
    }

    /// 可聚焦条目总数。
    pub fn card_count(&self) -> usize {
        self.continue_watching.len()
            + self.recently_added.len()
            + self
                .library_rows
                .iter()
                .map(|row| row.items.len())
                .sum::<usize>()
    }

    /// 按扁平焦点索引取卡片（继续观看 → 最近添加 → 各库行）。
    pub fn card_at(&self, index: usize) -> Option<&CardView> {
        if index < self.continue_watching.len() {
            return self.continue_watching.get(index);
        }
        let index = index - self.continue_watching.len();
        if index < self.recently_added.len() {
            return self.recently_added.get(index);
        }
        let mut index = index - self.recently_added.len();
        for row in &self.library_rows {
            match row.items.get(index) {
                Some(card) => return Some(card),
                None => index -= row.items.len(),
            }
        }
        None
    }
}

/// 视图整体替换（刷新 / 重新排序）后，首页焦点下标该落在哪里。
///
/// 与媒体库的 [`crate::viewmodel::rebase_focus`] 同一口彿：焦点按**条目 id**
/// 重新定位，而不是沿用旧下标。首页也会重排——「继续观看」按最近播放排序、
/// 「最近添加」按加入时间排序，同一个下标指向的往往是另一部作品；沿用旧下标会让
/// 焦点惄惄地飘到另一张卡上（键盘用户看到的高亮不在了）。
///
/// - 旧焦点卡的 `item_id` 在新视图里还在 → 用它新的下标；
/// - 找不到（条目被移除 / 换了数据源）→ 退回旧下标并夹到新卡片范围内；
/// - 新视图为空 → `0`。
pub fn rebase_home_focus(old: &HomeView, old_focus: usize, next: &HomeView) -> usize {
    let total = next.card_count();
    let fallback = old_focus.min(total.saturating_sub(1));
    let Some(id) = old.card_at(old_focus).map(|card| card.item_id.as_str()) else {
        return fallback;
    };
    (0..total)
        .find(|index| next.card_at(*index).is_some_and(|card| card.item_id == id))
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_home_has_no_hero() {
        let home = HomeView::new();
        assert!(home.hero.is_none());
        assert!(home.is_empty());
        assert!(home.focus_rows().is_empty());
        assert_eq!(home.card_count(), 0);
        assert_eq!(home.card_at(0), None);
        assert!(home.library_row_starts().is_empty());
    }

    #[test]
    fn focus_rows_merge_continue_and_recent() {
        let mut home = HomeView::new();
        home.continue_watching = vec![CardView::new("a", "A"), CardView::new("b", "B")];
        home.recently_added = (0..8)
            .map(|index| CardView::new(index.to_string(), "X"))
            .collect();
        // 最近添加是一整行（不是 6 列块），所以焦点行只有一个 8。
        assert_eq!(home.focus_rows(), vec![2, 8]);
        assert_eq!(home.card_at(0).map(|card| card.item_id.as_str()), Some("a"));
        assert_eq!(home.card_at(2).map(|card| card.item_id.as_str()), Some("0"));
        assert_eq!(home.card_at(9).map(|card| card.item_id.as_str()), Some("7"));
        assert_eq!(home.card_at(10), None);
        assert!(!home.is_empty());
    }

    /// 体检 R7-2：库行也是焦点行——`↓` 要到得了，`Enter` 要能打开。
    #[test]
    fn library_rows_are_focus_rows() {
        let mut home = HomeView::new();
        home.continue_watching = vec![CardView::new("c0", "C0")];
        home.recently_added = (0..3)
            .map(|index| CardView::new(format!("r{index}"), "R"))
            .collect();
        home.library_rows = vec![
            LibraryRowView {
                library_id: "lib-a".to_owned(),
                title: "A 库".to_owned(),
                items: (0..4)
                    .map(|index| CardView::new(format!("a{index}"), "A"))
                    .collect(),
            },
            LibraryRowView {
                library_id: "lib-b".to_owned(),
                title: "B 库".to_owned(),
                items: (0..2)
                    .map(|index| CardView::new(format!("b{index}"), "B"))
                    .collect(),
            },
        ];

        assert_eq!(home.focus_rows(), vec![1, 3, 4, 2]);
        // 扁平下标 = 1(继续观看) + 3(最近添加) + 行内偏移。
        assert_eq!(home.library_row_starts(), vec![4, 8]);
        assert_eq!(home.card_count(), 10);
        assert_eq!(
            home.card_at(4).map(|card| card.item_id.as_str()),
            Some("a0")
        );
        assert_eq!(
            home.card_at(9).map(|card| card.item_id.as_str()),
            Some("b1")
        );
        assert_eq!(home.card_at(10), None);
    }

    /// 空行不能留在 `focus_rows` 里：`move_up` 用 `rows.first()` 判断第一行。
    #[test]
    fn focus_rows_skip_empty_rows() {
        let mut home = HomeView::new();
        home.library_rows = vec![
            LibraryRowView {
                library_id: "lib-empty".to_owned(),
                title: "空库".to_owned(),
                items: Vec::new(),
            },
            LibraryRowView {
                library_id: "lib-a".to_owned(),
                title: "A 库".to_owned(),
                items: vec![CardView::new("a0", "A0")],
            },
        ];

        // 继续观看 / 最近添加为空 + 第一行库行为空：焦点行只有 A 库的 1 张。
        assert_eq!(home.focus_rows(), vec![1]);
        // `.slint` 的行号仍按可视行（空行也占位）算。
        assert_eq!(home.library_row_starts(), vec![0, 0]);
        // 可视行号：空库行是第 2 行，A 库是第 3 行（空行照样占一行）。
        assert_eq!(home.focus_row_at(0), Some(3));
        assert_eq!(home.focus_row_at(1), None);
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

    /// A3：首页重排后焦点要跟着条目 id 走。
    #[test]
    fn rebase_home_focus_follows_the_item_through_a_reorder() {
        let mut old = HomeView::new();
        old.continue_watching = vec![
            CardView::new("a", "A"),
            CardView::new("b", "B"),
            CardView::new("c", "C"),
        ];
        let mut next = old.clone();
        next.continue_watching = vec![
            CardView::new("b", "B"),
            CardView::new("c", "C"),
            CardView::new("a", "A"),
        ];
        assert_eq!(rebase_home_focus(&old, 1, &next), 0, "b 跑到下标 0");
        assert_eq!(rebase_home_focus(&old, 0, &next), 2, "a 跑到下标 2");

        // 条目被移除：退回旧下标并夹到范围内。
        let mut shrunk = old.clone();
        shrunk.continue_watching.remove(1);
        assert_eq!(rebase_home_focus(&old, 1, &shrunk), 1);
        shrunk.continue_watching.truncate(1);
        assert_eq!(rebase_home_focus(&old, 1, &shrunk), 0);

        // 新视图为空：0（不 panic、不越界）。
        assert_eq!(rebase_home_focus(&old, 2, &HomeView::new()), 0);
    }
}
