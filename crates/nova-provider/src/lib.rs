// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体服务端接入层。
//!
//! 这一层把 Emby / Jellyfin 的 HTTP API 收敛成 [`MediaProvider`] 一个 trait，
//! 上层（`nova-library`、`nova-ui`）只看到 [`nova_core`] 的统一模型。
//!
//! ## 分层位置
//!
//! ```text
//! nova-app -> nova-library -> nova-provider -> nova-core
//!                                      \----> nova-secrets
//! ```
//!
//! 依赖只向下：[`MediaProvider`] 的入参与返回值全部是 `nova_core` 类型，
//! 令牌一律经由 `nova_secrets::SecretString` / `CredentialStore` 出入，
//! 本 crate 的公开类型里没有任何"裸明文令牌"字段。
//!
//! ## 一套 DTO 覆盖两个服务端
//!
//! Emby 与 Jellyfin 是同一个代码库的两个分支，响应字段差异极小
//! （认证请求的密码字段名、认证头名字、个别 HDR 字段），
//! 因此这里**只有一套 DTO**（见 [`mod@mapping`] 的模块文档），差异集中在 [`Flavor`] 枚举上。
//! 分两套解析代码会让维护成本翻倍，而收益只是省掉几个 `match`。
//! 将来若某个服务端出现结构性差异，再按"同一套 DTO + 可选字段"扩展，而不是分叉。
//!
//! ## 安全约束
//!
//! 1. 令牌只写进注入的 [`CredentialStore`](nova_secrets::CredentialStore)，
//!    `AuthState::Authorized` 里只放键名（`jellyfin/<server-id>/token`）。
//! 2. 只有 [`StreamTarget`] 需要把令牌塞进 URL 查询串（播放器拿到的是一条裸地址），
//!    所以它的 `Debug` / `Display` 都会把查询串抹成 `?<redacted>`。
//! 3. 任何错误变体都不承载令牌，也不承载带查询串的完整地址；日志只打印路径。
//! 4. TLS 不做任何放宽：没有"跳过证书校验"入口，`http://` 只在
//!    [`nova_core::ServerInfo::is_plaintext`] 里被识别并提示给用户。

#![warn(missing_docs)]

mod client;
mod error;
mod http;
mod mapping;
mod provider;
mod types;

pub use client::{DeviceInfo, Provider, ProviderConfig};
pub use error::ProviderError;
pub use http::{RetryPolicy, parse_base_url};
pub use mapping::Flavor;
pub use provider::MediaProvider;
pub use types::{
    ImageKind, ImageRequest, ItemQuery, Library, Page, PlaybackSources, PublicServerInfo, SortBy,
    StreamTarget,
};

/// 本 crate 在认证头里声明的客户端名。
pub const CLIENT_NAME: &str = "Nova";

/// 本 crate 在认证头里声明的客户端版本。
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
