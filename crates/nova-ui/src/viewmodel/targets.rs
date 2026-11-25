// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 调试通道的"可交互目标"（Slint `Target` 的 Rust 镜像）。
//!
//! 界面在 `ui/*.slint` 里登记所有用户能点的东西（按钮 / 开关 / 输入框 / 卡片 /
//! 标签 / 滑杆 / 滚动箭头）。这份结构是它们的 Rust 形态，供
//! `--debug-socket` 的 `targets` / `click` / `hover` / `sweep` 使用，也供测试断言
//! "每个页面都有可点目标、id 唯一、矩形落在窗口内"。
//!
//! 约定（新增控件必须遵守，见 `docs/ui-architecture.md`）：
//! - `id` 以页面命名空间开头（`home.` / `library.` / `search.` / `detail.` /
//!   `player.`），浮层用 `banner.` / `dialog.`；同一窗口内唯一。
//! - `kind` 只用 [`TARGET_KINDS`] 里的字符串。
//! - 坐标是**窗口逻辑像素**，来自元素的 `absolute-position`，不含任何额外偏移。

/// 允许的 `kind` 取值（测试与脚本都按这个集合校验）。
pub const TARGET_KINDS: [&str; 7] = [
    "button", "toggle", "input", "card", "tab", "slider", "scroll",
];

/// 一个可交互目标。
#[derive(Clone, Debug, PartialEq)]
pub struct TargetInfo {
    /// 稳定标识，例如 `"library.sort"` / `"home.row.continue.card.2"`。
    pub id: String,
    /// 控件类型，见 [`TARGET_KINDS`]。
    pub kind: String,
    /// 界面上看得见的文字（无障碍 / 报告用）。
    pub label: String,
    /// 左上角 x（窗口逻辑像素）。
    pub x: f32,
    /// 左上角 y（窗口逻辑像素）。
    pub y: f32,
    /// 宽（逻辑像素）。
    pub width: f32,
    /// 高（逻辑像素）。
    pub height: f32,
}

impl TargetInfo {
    /// 中心点：`click` / `hover` 默认打在这里。
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// 是否是一个能点的矩形（宽高为正）。
    pub fn is_clickable(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }

    /// 中心点是否落在 `width × height` 的窗口里（容许 1px 误差）。
    ///
    /// 卡片行会横向溢出视口（Flickable 裁剪），只露出半张的卡不点：
    /// 点它的中心只会命中窗口外或相邻内容，报告"没反应"是噪音。
    pub fn center_in_window(&self, width: f32, height: f32) -> bool {
        let (x, y) = self.center();
        x >= -1.0 && y >= -1.0 && x <= width + 1.0 && y <= height + 1.0
    }

    /// `kind` 是否在允许集合里。
    pub fn has_known_kind(&self) -> bool {
        TARGET_KINDS.contains(&self.kind.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(x: f32, y: f32, width: f32, height: f32) -> TargetInfo {
        TargetInfo {
            id: "a".to_owned(),
            kind: "button".to_owned(),
            label: "A".to_owned(),
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn center_is_the_middle_of_the_rect() {
        assert_eq!(target(10.0, 20.0, 100.0, 40.0).center(), (60.0, 40.0));
    }

    #[test]
    fn zero_sized_targets_are_not_clickable() {
        assert!(!target(0.0, 0.0, 0.0, 10.0).is_clickable());
        assert!(target(0.0, 0.0, 1.0, 1.0).is_clickable());
    }

    #[test]
    fn center_filter_keeps_partially_visible_cards_and_drops_offscreen_ones() {
        let window = (1280.0, 800.0);
        // 左边缘只露出 40px：中心还在窗口里 → 保留。
        assert!(target(-60.0, 100.0, 200.0, 100.0).center_in_window(window.0, window.1));
        // 中心在窗口右侧之外 → 丢掉。
        assert!(!target(1200.0, 100.0, 200.0, 100.0).center_in_window(window.0, window.1));
        // 整行在首屏下方（纵向溢出）→ 丢掉。
        assert!(!target(0.0, 900.0, 200.0, 100.0).center_in_window(window.0, window.1));
    }

    #[test]
    fn kinds_are_restricted_to_the_documented_set() {
        assert!(target(0.0, 0.0, 1.0, 1.0).has_known_kind());
        let mut weird = target(0.0, 0.0, 1.0, 1.0);
        weird.kind = "widget".to_owned();
        assert!(!weird.has_known_kind());
    }
}
