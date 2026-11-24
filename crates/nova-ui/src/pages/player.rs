// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放页绑定层：视图模型 → Slint 属性。
//!
//! 所有判断都在 [`crate::viewmodel::PlayerView`] 里，这里只搬运字段。
//! [`TICK_INTERVAL`] 必须与 `player_overlay.slint` 里 `Timer.interval` 一致：
//! 视图模型的自动隐藏计时靠这个增量推进。

use std::time::Duration;

use crate::AppWindow;
use crate::pages::{assets::ImageStore, convert};
use crate::viewmodel::PlayerView;

/// 自动隐藏计时的心跳间隔（与 `player_overlay.slint` 的 `Timer.interval` 一致）。
pub const TICK_INTERVAL: Duration = Duration::from_millis(250);

/// 把播放页视图模型写进 Slint。
pub fn apply(ui: &AppWindow, view: &PlayerView, images: &ImageStore) {
    apply_for(ui, view, images, "");
}

/// 把播放页视图模型写进 Slint，并带上当前条目标识（供上层回传与取背景图）。
pub fn apply_for(ui: &AppWindow, view: &PlayerView, images: &ImageStore, item_id: &str) {
    let mut data = convert::player_data(view, images, item_id);
    data.item_id = item_id.into();
    ui.set_player(data);
}

/// 拖动进度条：只更新气泡文本，不动播放位置（松手才 seek）。
pub fn apply_scrub(ui: &AppWindow, view: &PlayerView, ratio: f32) {
    ui.set_player_scrub_text(view.scrub_text(ratio).into());
}

/// 结束拖动：清空气泡文本。
pub fn clear_scrub(ui: &AppWindow) {
    ui.set_player_scrub_text("".into());
}
