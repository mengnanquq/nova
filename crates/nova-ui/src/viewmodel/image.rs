// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 图片契约：界面只声明需要哪些图，由上层把 `slint::Image` 推进来。

/// 图片种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKind {
    /// 竖版海报（2:3）。
    Poster,
    /// 横版背景图。
    Backdrop,
    /// 透明标题 logo。
    Logo,
}

/// 图片键：条目 id + 图片种类。
///
/// 上层缓存时还应带上服务端的版本 tag（`ImageSet::poster_tag`），
/// 这里只负责让界面表达“我要哪张图”。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey {
    /// 条目标识。
    pub item_id: String,
    /// 图片种类。
    pub kind: ImageKind,
}

impl ImageKey {
    /// 构造图片键。
    pub fn new(item_id: impl Into<String>, kind: ImageKind) -> Self {
        Self {
            item_id: item_id.into(),
            kind,
        }
    }

    /// 海报键。
    pub fn poster(item_id: impl Into<String>) -> Self {
        Self::new(item_id, ImageKind::Poster)
    }

    /// 背景图键。
    pub fn backdrop(item_id: impl Into<String>) -> Self {
        Self::new(item_id, ImageKind::Backdrop)
    }
}
