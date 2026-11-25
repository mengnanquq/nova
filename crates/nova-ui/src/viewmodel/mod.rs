// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 纯 Rust 视图模型：不含 I/O、async、网络、数据库或 Slint 依赖。
//!
//! 上层把 `nova-core` 的媒体模型压成这些结构，界面只读它们。

pub mod add_server;
pub mod banner;
pub mod card;
pub mod detail;
pub mod focus;
pub mod format;
pub mod home;
pub mod image;
pub mod library;
pub mod nav;
pub mod player;
pub mod search;

pub use add_server::{
    AddServerError, AddServerErrorKind, AddServerState, AddServerView, AddressError,
    is_plaintext_address, validate_address,
};
pub use banner::{BannerData, BannerLevel};
pub use card::CardView;
pub use detail::{DetailView, SourceOption};
pub use focus::move_focus;
pub use format::{
    badge, detail_meta_line, episode_code, format_duration, meta_line, progress_percent,
};
pub use home::{HomeView, LibraryRowView};
pub use image::{ImageKey, ImageKind};
pub use library::{LibraryTab, LibraryView};
pub use nav::{Navigator, Route};
pub use player::{
    AUTO_HIDE_DELAY, PlayerView, TimeMode, TrackItem, VOLUME_HINT_DURATION, VOLUME_STEP, clock_text,
};
pub use search::{SearchState, SearchView};

/// 阶段 3 的旧名字，保留给 `nova-app` 的存量调用点。
pub type PlayerShellView = PlayerView;
