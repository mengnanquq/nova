// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 统一媒体数据模型。
//!
//! 这一层只描述"媒体是什么"，不做任何 I/O：
//!
//! - `nova-provider`（Emby / Jellyfin 适配层）把各自的响应映射成这里的类型；
//! - `nova-playback` 只依赖这里的类型描述播放会话；
//! - Slint 界面层只依赖这里的类型渲染。
//!
//! 依赖方向是单向的：任何 crate 都可以依赖 `nova-core`，`nova-core` 不依赖它们。
//!
//! 设计约束（与安全/隐私要求对应）：
//! - 这里**不存放**服务器密码或访问令牌，只存放凭证存储的引用（见 [`server::AuthState`]）。
//! - 所有来自服务端的字段都视为不可信输入，由提供方适配层负责校验后填入。

#![warn(missing_docs)]

mod ids;
mod language;
mod media;
mod playback;
mod prefs;
mod server;
mod stream;
mod track_selection;

pub use ids::{ItemId, ServerId, SourceId};
pub use language::Language;
pub use media::{ImageSet, MediaItem, MediaKind, UserData};
pub use playback::{PlayMethod, PlaybackProgress, PlaybackSession, PlaybackStatus};
pub use prefs::{SubtitleMode, Theme, UserPreferences};
pub use server::{AuthState, ServerInfo, ServerKind};
pub use stream::{
    AudioDetails, HdrFormat, MediaSource, MediaStream, StreamKind, SubtitleDetails, VideoDetails,
};
pub use track_selection::{TrackChoice, select_audio, select_subtitle};
