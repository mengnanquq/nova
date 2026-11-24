// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 图片推入层：上层把解码好的 `slint::Image` 按 `ImageKey` 放进来。
//!
//! 这里不做下载、解码或缓存淘汰，只保存当前窗口需要的那几张图。

use std::collections::HashMap;

use crate::viewmodel::{ImageKey, ImageKind};

/// 当前会话的图片表。
#[derive(Clone, Debug, Default)]
pub struct ImageStore {
    images: HashMap<ImageKey, slint::Image>,
}

impl ImageStore {
    /// 空图片表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 推入一张图片。
    pub fn set(&mut self, key: ImageKey, image: slint::Image) {
        self.images.insert(key, image);
    }

    /// 推入海报。
    pub fn set_poster(&mut self, item_id: impl Into<String>, image: slint::Image) {
        self.set(ImageKey::new(item_id, ImageKind::Poster), image);
    }

    /// 推入背景图。
    pub fn set_backdrop(&mut self, item_id: impl Into<String>, image: slint::Image) {
        self.set(ImageKey::new(item_id, ImageKind::Backdrop), image);
    }

    /// 取图片；没有时返回空 `slint::Image`（界面据此显示占位）。
    pub fn get(&self, key: &ImageKey) -> slint::Image {
        self.images.get(key).cloned().unwrap_or_default()
    }

    /// 是否已有这张图。
    pub fn contains(&self, key: &ImageKey) -> bool {
        self.images.contains_key(key)
    }

    /// 移除一张图（例如服务端换图后）。
    pub fn remove(&mut self, key: &ImageKey) -> Option<slint::Image> {
        self.images.remove(key)
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.images.clear();
    }

    /// 已缓存数量。
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}
