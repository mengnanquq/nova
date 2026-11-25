// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 的 Slint 界面与纯 Rust 视图模型。
//!
//! # 定位
//!
//! 只做界面：无 I/O、无 async、无网络、无数据库、不读文件、不依赖 `nova-core`。
//! 上层（`nova-app`）把媒体模型压成 [`viewmodel`] 里的结构，再把解码好的
//! `slint::Image` 通过 [`ImageStore`] 推进来。
//!
//! # 分层
//!
//! * [`viewmodel`]：纯 Rust 数据与格式化/导航/焦点逻辑，可用普通单测覆盖。
//! * `ui/*.slint`：页面与组件，只负责摆放与动画。
//! * [`pages`]：视图模型 → Slint 属性的薄绑定，以及 [`App`] 的回调接线。

#![deny(unsafe_code)]

pub mod pages;
pub mod viewmodel;

slint::include_modules!();

pub use pages::{App, ImageStore, KeyCommand, Page, PlayerKey, SweepEntry, SweepReport};
pub use viewmodel::{
    AUTO_HIDE_DELAY, AddServerError, AddServerErrorKind, AddServerState, AddServerView, BannerData,
    BannerLevel, CardView, DetailView, HomeView, ImageKey, ImageKind, LibraryRowView, LibraryTab,
    LibraryView, Navigator, PlayerShellView, PlayerView, Route, SearchState, SearchView,
    SourceOption, TARGET_KINDS, TargetInfo, TimeMode, TrackItem, VOLUME_HINT_DURATION, VOLUME_STEP,
    badge, clock_text, detail_meta_line, episode_code, format_duration, meta_line, move_focus,
    progress_percent, validate_address,
};
