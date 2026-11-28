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
    /// 是否有播放入口：按条目类型判断——**电影 / 单集可播，剧集 / 季不可播**
    /// （`MediaKind::is_playable`）。片源挂在单集上，剧集 / 季本体没有任何可播版本，
    /// 点「播放」只会落到错误卡上（真机 R9-A2 实测：「这个条目没有可播放的版本」）。
    /// 选集尚未实现，所以剧集详情不下架播放按钮就没有可用的播放入口。电影离线时
    /// 也可能暂时没有片源，但联网后能播，不能按「片源为空」下架，只能按条目类型判断。
    ///
    /// 由生产方（nova-app 的 `detail_from_item`）按条目类型填；界面上没有
    /// 播放按钮的条目，键盘 Enter 也不会复活它（见 `play_current` 的门控，
    /// Enter 落到「我的列表」上）。
    pub playable: bool,
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
            // 默认按「能播」处理：这个结构大多由测试 / 演示直接构造，缺省值
            // 保持旧行为；nova-app 的装配函数会按条目类型覆写。
            playable: true,
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
