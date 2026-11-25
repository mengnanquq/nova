// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 界面绑定层：每个页面的视图模型到 Slint 属性的薄封装，外加应用级回调接线。

pub mod add_server;
pub mod app;
pub mod assets;
pub mod banner;
pub mod convert;
pub mod detail;
pub mod home;
pub mod library;
pub mod player;
pub mod search;

pub use app::{App, KeyCommand, Page, PlayerKey};
pub use assets::ImageStore;
