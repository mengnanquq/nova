// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 图片推入层：上层把解码好的 `slint::Image` 按 `ImageKey` 放进来。
//!
//! 这里不做下载、解码或磁盘缓存淘汰，只保存"当前窗口需要的那几张图"。
//!
//! # 内存上限
//!
//! 图片按可见窗口请求之后，"滚过的图"会留在内存里：一张 300 宽的 2:3 海报解码后
//! 是 300×450×4 ≈ 0.51 MiB，海外电影 4546 张全滚一遍 ≈ 2.3 GiB。所以这里按
//! **字节预算**保留最近放入的图，超出时淘汰最旧的（[`ImageStore::trim`]）。
//! 淘汰时保护"当前可见窗口"的键（由上层传入），屏幕上不会出现回退成骨架的情况。
//! 被淘汰的键要由上层解除"已请求"标记，重新进入视口时才能再装载（磁盘缓存命中）。

use std::collections::{HashMap, HashSet, VecDeque};

use crate::viewmodel::{ImageKey, ImageKind};

/// 一张图（`slint::Image`）在内存里的字节数估算：RGBA8 = 宽 × 高 × 4。
fn image_bytes(image: &slint::Image) -> u64 {
    let size = image.size();
    (size.width as u64)
        .saturating_mul(size.height as u64)
        .saturating_mul(4)
}

/// 当前会话的图片表。
#[derive(Clone, Debug, Default)]
pub struct ImageStore {
    images: HashMap<ImageKey, (slint::Image, u64)>,
    /// 放入顺序（淘汰从最旧的开始）。
    order: VecDeque<ImageKey>,
    /// 当前所有图的总字节数。
    bytes: u64,
}

impl ImageStore {
    /// 空图片表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 推入一张图片。
    pub fn set(&mut self, key: ImageKey, image: slint::Image) {
        let bytes = image_bytes(&image);
        if let Some((old, old_bytes)) = self.images.insert(key.clone(), (image, bytes)) {
            let _ = old;
            self.bytes = self.bytes.saturating_sub(old_bytes);
        } else {
            self.order.push_back(key);
        }
        self.bytes = self.bytes.saturating_add(bytes);
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
        self.images
            .get(key)
            .map(|(image, _)| image.clone())
            .unwrap_or_default()
    }

    /// 是否已有这张图。
    pub fn contains(&self, key: &ImageKey) -> bool {
        self.images.contains_key(key)
    }

    /// 移除一张图（例如服务端换图后）。
    pub fn remove(&mut self, key: &ImageKey) -> Option<slint::Image> {
        let (image, bytes) = self.images.remove(key)?;
        self.bytes = self.bytes.saturating_sub(bytes);
        if let Some(index) = self.order.iter().position(|queued| queued == key) {
            self.order.remove(index);
        }
        Some(image)
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.images.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// 已缓存数量。
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// 估算的常驻字节数（宽 × 高 × 4 累加）。
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// 按字节预算淘汰最旧的图，返回被淘汰的键（按淘汰顺序）。
    ///
    /// 淘汰顺序就是**首次放入顺序（FIFO）**：`order` 只在 `set` 第一次插入某个
    /// 键时 `push_back`，重复 `set` 同一键不会把它移到队尾（更新已存在的图不算
    /// "最近使用"）。`protect` 里的键（当前可见窗口）不参与淘汰，被跳过后会
    /// 被放回队尾（留给下一轮）。
    ///
    /// `protect` 里的键（当前可见窗口）不会被淘汰：它们在屏幕上，淘汰了会当场
    /// 变回骨架。最坏情况下（可见窗口本身就超预算）保留全部可见图、不淘汰其它
    /// 更旧的图——宁可短暂超预算，也不要在视口里挖洞。
    pub fn trim(&mut self, protect: &HashSet<ImageKey>, budget: u64) -> Vec<ImageKey> {
        let mut evicted = Vec::new();
        let mut protected = Vec::new();
        let mut remaining = self.order.len();
        while self.bytes > budget && remaining > 0 {
            remaining -= 1;
            let Some(key) = self.order.pop_front() else {
                break;
            };
            if protect.contains(&key) {
                protected.push(key);
                continue;
            }
            if let Some((_, bytes)) = self.images.remove(&key) {
                self.bytes = self.bytes.saturating_sub(bytes);
                evicted.push(key);
            }
        }
        for key in protected {
            self.order.push_back(key);
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> slint::Image {
        use i_slint_core::graphics::{Rgba8Pixel, SharedPixelBuffer};
        slint::Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(width, height))
    }

    /// 每张 10×10 RGBA = 400 字节；预算 1000 字节 ≈ 两张。
    fn small(key: &str) -> ImageKey {
        ImageKey::poster(key)
    }

    #[test]
    fn trim_evicts_oldest_first_and_reports_evicted_keys() {
        let mut store = ImageStore::new();
        for key in ["a", "b", "c"] {
            store.set(small(key), image(10, 10));
        }
        assert_eq!(store.bytes(), 3 * 400);
        let evicted = store.trim(&HashSet::new(), 1000);
        assert_eq!(evicted, vec![small("a")], "只淘汰最旧的一张就够回到预算内");
        assert!(!store.contains(&small("a")));
        assert!(store.contains(&small("c")));
        assert_eq!(store.bytes(), 800);
    }

    #[test]
    fn trim_never_evicts_protected_keys() {
        let mut store = ImageStore::new();
        for key in ["a", "b", "c"] {
            store.set(small(key), image(10, 10));
        }
        let protect: HashSet<ImageKey> = [small("a")].into_iter().collect();
        let evicted = store.trim(&protect, 400);
        assert_eq!(
            evicted,
            vec![small("b"), small("c")],
            "最旧的 a 在视口里，必须跳过它继续淘汰"
        );
        assert!(store.contains(&small("a")), "在屏上的图不能被淘汰");
    }

    #[test]
    fn visible_window_bigger_than_budget_is_kept_whole() {
        let mut store = ImageStore::new();
        for key in ["a", "b"] {
            store.set(small(key), image(10, 10));
        }
        let protect: HashSet<ImageKey> = [small("a"), small("b")].into_iter().collect();
        assert!(store.trim(&protect, 0).is_empty(), "宁可超预算也不挖洞");
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn replacing_an_image_does_not_double_count_bytes() {
        let mut store = ImageStore::new();
        store.set(small("a"), image(10, 10));
        store.set(small("a"), image(10, 10));
        assert_eq!(store.bytes(), 400);
        assert_eq!(store.len(), 1);
        assert_eq!(store.order.len(), 1);
    }
}
