// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 详情页视图模型与多版本播放源。

use crate::viewmodel::image::ImageKey;

/// 一个可播放版本（同一部片子的不同分辨率/编码/音轨）。
#[derive(Clone, Debug, PartialEq)]
pub struct SourceOption {
    /// 版本标识（回调时回传）。
    pub source_id: String,
    /// 主标题，例如 `"4K HDR · 简体中文"`。
    pub title: String,
    /// 细节，例如 `"HEVC · 22.4 Mbps"`。
    pub subtitle: Option<String>,
    /// 是否当前选中。
    pub selected: bool,
}

impl SourceOption {
    /// 构造一个未选中的版本。
    pub fn new(source_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            title: title.into(),
            subtitle: None,
            selected: false,
        }
    }

    /// 设置细节。
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// 设置选中态。
    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

/// 详情页。
#[derive(Clone, Debug, PartialEq)]
pub struct DetailView {
    /// 条目标识。
    pub item_id: String,
    /// 标题。
    pub title: String,
    /// 原始语言标题。
    pub original_title: Option<String>,
    /// 元信息行：`"2021 · 2 小时 18 分 · 动作 / 科幻"`。
    pub meta_line: String,
    /// 简介。
    pub overview: Option<String>,
    /// 播放按钮文案：`"播放"` / `"从 12:34 继续"`。
    pub play_label: String,
    /// 是否已在「我的列表」（本地持久化，不同步服务端）。
    pub in_my_list: bool,
    /// 可播放版本。
    pub sources: Vec<SourceOption>,
    /// 类型标签（详情页单独展示为胶囊）。
    pub genres: Vec<String>,
    /// 角标。
    pub badge: Option<String>,
}

impl Default for DetailView {
    fn default() -> Self {
        Self {
            item_id: String::new(),
            title: String::new(),
            original_title: None,
            meta_line: String::new(),
            overview: None,
            play_label: "播放".to_string(),
            in_my_list: false,
            sources: Vec::new(),
            genres: Vec::new(),
            badge: None,
        }
    }
}

impl DetailView {
    /// 构造只有 id 与标题的详情页。
    pub fn new(item_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            item_id: item_id.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    /// 详情页需要的图片键。
    pub fn image_keys(&self) -> Vec<ImageKey> {
        vec![
            ImageKey::poster(self.item_id.clone()),
            ImageKey::backdrop(self.item_id.clone()),
        ]
    }

    /// 海报键。
    pub fn poster_key(&self) -> ImageKey {
        ImageKey::poster(self.item_id.clone())
    }

    /// 背景图键。
    pub fn backdrop_key(&self) -> ImageKey {
        ImageKey::backdrop(self.item_id.clone())
    }

    /// 按索引取版本。
    pub fn source_at(&self, index: usize) -> Option<&SourceOption> {
        self.sources.get(index)
    }
}
