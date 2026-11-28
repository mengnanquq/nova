// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 卡片视图模型：首页横排、最近添加、媒体库网格共用。

use crate::viewmodel::format::progress_percent;
use crate::viewmodel::image::{ImageKey, ImageKind};

/// 一张海报卡。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardView {
    /// 条目标识（回调与图片键使用）。
    pub item_id: String,
    /// 标题。
    pub title: String,
    /// 副标题：`"第 2 季 · 第 5 集"` / `"2021 · 动作"`。
    pub subtitle: Option<String>,
    /// 观看进度 `0.0..=1.0`，仅“继续观看”有值。
    pub progress: Option<f32>,
    /// 角标：`"4K"` / `"HDR"` / `"S01E05"`。
    pub badge: Option<String>,
    /// 简介：只有首页 billboard 用到（最多 3 行）。
    pub overview: Option<String>,
    /// 条目自身能否直接播放（电影 / 单集）。首页 hero 据此决定要不要给「▶ 播放」：
    /// hero 取的是「继续观看 / 最近添加」的第一条，可能是剧集（片源挂在单集上）。
    pub playable: bool,
}

impl CardView {
    /// 构造一张只有 id 与标题的卡片。
    pub fn new(item_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            item_id: item_id.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    /// 设置副标题。
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// 设置进度（自动夹到 `0.0..=1.0`）。
    pub fn with_progress(mut self, progress: f32) -> Self {
        self.progress = Some(progress.clamp(0.0, 1.0));
        self
    }

    /// 设置角标。
    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// 设置简介（billboard 用）。
    pub fn with_overview(mut self, overview: impl Into<String>) -> Self {
        self.overview = Some(overview.into());
        self
    }

    /// 设置条目能否直接播放（首页 hero 的播放入口用）。
    pub fn with_playable(mut self, playable: bool) -> Self {
        self.playable = playable;
        self
    }

    /// 进度百分比文本：`Some(0.37)` → `"37%"`，`None` → 空串。
    pub fn progress_percent(&self) -> String {
        progress_percent(self.progress)
    }

    /// 占位首字母；标题为空时用 `"?"`。
    pub fn initial(&self) -> String {
        self.title
            .trim()
            .chars()
            .next()
            .map(|first| first.to_uppercase().collect())
            .unwrap_or_else(|| "?".to_string())
    }

    /// 这张卡需要的海报键。
    pub fn poster_key(&self) -> ImageKey {
        ImageKey::new(self.item_id.clone(), ImageKind::Poster)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_formats_progress_and_initial() {
        let card = CardView::new("1", "dune").with_progress(0.37);
        assert_eq!(card.progress_percent(), "37%");
        assert_eq!(card.initial(), "D");
        assert_eq!(CardView::new("2", "沙丘").initial(), "沙");
        assert_eq!(CardView::default().initial(), "?");
    }

    #[test]
    fn progress_is_clamped_when_built() {
        assert_eq!(
            CardView::new("1", "x").with_progress(-3.0).progress,
            Some(0.0)
        );
        assert_eq!(
            CardView::new("1", "x").with_progress(9.0).progress,
            Some(1.0)
        );
    }

    #[test]
    fn poster_key_uses_item_id() {
        let card = CardView::new("abc", "x");
        assert_eq!(card.poster_key(), ImageKey::poster("abc"));
    }
}
