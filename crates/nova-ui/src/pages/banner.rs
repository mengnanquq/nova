// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 横幅绑定层：视图模型 → Slint 属性。

use crate::AppWindow;
use crate::viewmodel::{BannerData, BannerLevel};

/// 把横幅写进 Slint；`None` 表示隐藏。
pub fn apply(ui: &AppWindow, banner: Option<&BannerData>) {
    let Some(banner) = banner else {
        ui.set_banner_visible(false);
        return;
    };
    ui.set_banner_visible(true);
    ui.set_banner_level(match banner.level {
        BannerLevel::Info => 0,
        BannerLevel::Warning => 1,
        BannerLevel::Danger => 2,
    });
    ui.set_banner_text(banner.text.clone().into());
    ui.set_banner_action_label(banner.action_label.clone().unwrap_or_default().into());
}
