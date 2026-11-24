// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体实体：电影、剧集、季、集。
//!
//! 四种实体共享一个结构而不是四个结构：服务端 API（Emby / Jellyfin）本身就用同一种
//! 条目返回它们，界面的列表与详情页也按同一套字段渲染。用 `kind` 区分语义。

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::ids::ItemId;

/// 条目类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// 电影。
    Movie,
    /// 剧集（剧集本身不可播放）。
    Series,
    /// 季（不可播放）。
    Season,
    /// 集（可播放）。
    Episode,
}

impl MediaKind {
    /// 是否可以直接播放（电影与单集）。
    pub fn is_playable(self) -> bool {
        matches!(self, Self::Movie | Self::Episode)
    }

    /// 是否是一个容器（下面还有子条目需要再请求一层）。
    pub fn is_container(self) -> bool {
        matches!(self, Self::Series | Self::Season)
    }
}

/// 图片地址集合。存绝对 URL（由适配层基于服务器地址拼接并校验），界面层只负责加载。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSet {
    /// 竖版海报。
    pub poster: Option<String>,
    /// 横版背景图。
    pub backdrop: Option<String>,
    /// 标题 logo（透明 PNG）。
    pub logo: Option<String>,
    /// 缩略图 / 截图。
    pub thumb: Option<String>,
    /// 海报的版本标记（服务端 `ImageTags.Primary`）。
    ///
    /// 图片 URL 在服务端换图后通常不变，所以缓存键必须带上它，否则会一直命中旧图。
    #[serde(default)]
    pub poster_tag: Option<String>,
    /// 背景图的版本标记。
    #[serde(default)]
    pub backdrop_tag: Option<String>,
}

impl ImageSet {
    /// 首选展示图：优先背景图，退化为海报。
    pub fn hero(&self) -> Option<&str> {
        self.backdrop
            .as_deref()
            .or(self.poster.as_deref())
            .or(self.thumb.as_deref())
    }
}

/// 跨服共享的用户观看状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserData {
    /// 是否已看完。
    pub played: bool,
    /// 上次观看位置（用于"继续观看"）。
    pub resume_position: Duration,
    /// 播放次数。
    pub play_count: u32,
    /// 是否收藏。
    pub favorite: bool,
}

/// 统一的媒体条目。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    /// 条目标识（服务器作用域内唯一）。
    pub id: ItemId,
    /// 条目类型。
    pub kind: MediaKind,
    /// 显示标题。
    pub title: String,
    /// 排序标题（缺失时用 `title`）。
    pub sort_title: String,
    /// 原始语言标题。
    pub original_title: Option<String>,
    /// 发行年份。
    pub year: Option<u16>,
    /// 简介。
    pub overview: Option<String>,
    /// 类型标签（动作 / 剧情 …）。
    pub genres: Vec<String>,
    /// 时长。剧集条目上通常为 0，需从可播放资源读取。
    pub runtime: Option<Duration>,
    /// 父条目：集的父是季，季的父是剧集，电影的父为空。
    pub parent_id: Option<ItemId>,
    /// 季号 / 集号。
    pub index_number: Option<u32>,
    /// 集号区间结束（多集合并成一个文件时使用）。
    pub index_number_end: Option<u32>,
    /// 子条目数量（剧集有季数，季有集数）。
    pub child_count: Option<u32>,
    /// 图片。
    pub images: ImageSet,
    /// 用户观看状态。
    pub user_data: UserData,
}

impl MediaItem {
    /// 该条目自身能否直接播放。
    pub fn is_playable(&self) -> bool {
        self.kind.is_playable()
    }

    /// 观看进度比例 `0.0..=1.0`；时长未知或为 0 时返回 `None`。
    pub fn progress_fraction(&self) -> Option<f32> {
        let total = self.runtime?.as_secs_f32();
        if total <= 0.0 {
            return None;
        }
        Some((self.user_data.resume_position.as_secs_f32() / total).clamp(0.0, 1.0))
    }

    /// 是否应出现在"继续观看"里：未看完、有进度、且不是刚开头。
    pub fn is_in_progress(&self, min_seconds: u64) -> bool {
        !self.user_data.played
            && self.user_data.resume_position.as_secs() >= min_seconds
            && self
                .progress_fraction()
                .is_some_and(|fraction| fraction < 0.95)
    }

    /// 排序标题：服务端没给就用显示标题。
    pub fn sort_key(&self) -> &str {
        if self.sort_title.is_empty() {
            &self.title
        } else {
            &self.sort_title
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        kind: MediaKind,
        runtime: Option<Duration>,
        resume: Duration,
        played: bool,
    ) -> MediaItem {
        MediaItem {
            id: ItemId::new("1"),
            kind,
            title: "T".into(),
            sort_title: String::new(),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime,
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet::default(),
            user_data: UserData {
                played,
                resume_position: resume,
                ..UserData::default()
            },
        }
    }

    #[test]
    fn only_movie_and_episode_are_playable() {
        assert!(MediaKind::Movie.is_playable());
        assert!(MediaKind::Episode.is_playable());
        assert!(!MediaKind::Series.is_playable());
        assert!(MediaKind::Series.is_container());
    }

    #[test]
    fn progress_fraction_clamps_and_handles_unknown_runtime() {
        let halfway = item(
            MediaKind::Movie,
            Some(Duration::from_secs(100)),
            Duration::from_secs(50),
            false,
        );
        assert_eq!(halfway.progress_fraction(), Some(0.5));

        let no_runtime = item(MediaKind::Movie, None, Duration::from_secs(50), false);
        assert_eq!(no_runtime.progress_fraction(), None);

        let zero_runtime = item(
            MediaKind::Movie,
            Some(Duration::ZERO),
            Duration::from_secs(50),
            false,
        );
        assert_eq!(zero_runtime.progress_fraction(), None);

        let overdue = item(
            MediaKind::Movie,
            Some(Duration::from_secs(100)),
            Duration::from_secs(500),
            false,
        );
        assert_eq!(overdue.progress_fraction(), Some(1.0));
    }

    #[test]
    fn in_progress_requires_unfinished_and_meaningful_position() {
        let runtime = Some(Duration::from_secs(3600));
        let fresh = item(MediaKind::Movie, runtime, Duration::from_secs(0), false);
        assert!(!fresh.is_in_progress(30));

        let ten_seconds = item(MediaKind::Movie, runtime, Duration::from_secs(10), false);
        assert!(!ten_seconds.is_in_progress(30));

        let resumed = item(MediaKind::Movie, runtime, Duration::from_secs(600), false);
        assert!(resumed.is_in_progress(30));

        let finished = item(MediaKind::Movie, runtime, Duration::from_secs(3500), false);
        assert!(!finished.is_in_progress(30));

        let watched = item(MediaKind::Movie, runtime, Duration::from_secs(600), true);
        assert!(!watched.is_in_progress(30));
    }

    #[test]
    fn sort_key_falls_back_to_title() {
        let mut entry = item(MediaKind::Movie, None, Duration::ZERO, false);
        entry.title = "Blade Runner".into();
        assert_eq!(entry.sort_key(), "Blade Runner");
        entry.sort_title = "Blade Runner 2049".into();
        assert_eq!(entry.sort_key(), "Blade Runner 2049");
    }

    #[test]
    fn serde_roundtrip() {
        let entry = item(
            MediaKind::Episode,
            Some(Duration::from_secs(1440)),
            Duration::ZERO,
            false,
        );
        let json = serde_json::to_string(&entry).unwrap();
        let back: MediaItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }
}
