// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 图片缓存：封面图 / 背景图的获取、解码与磁盘缓存。
//!
//! 界面层要的只有一件事：给一个图片地址，拿到一块 RGBA8 像素。这一层把中间的
//! 麻烦都吃掉：
//!
//! ```text
//! ImageRequest ──缓存键──> 磁盘命中? ──是──> 解码/缩放 ──> DecodedImage
//!                                 └─否─> Fetcher 下载 ──> 校验解码 ──> 原子落盘 ──> 返回
//! ```
//!
//! # 缓存键必须带版本标记
//!
//! 服务端换图时 URL 往往不变（`/Items/42/Images/Primary` 换个 `ImageTag` 而已），
//! 所以 [`ImageRequest::tag`] 参与缓存键：`sha256(url ‖ 0x00 ‖ tag)`（空 tag 等价于没有）。
//! 传 `None` 就等于"这张图永远不会变"——对 `ImageSet::poster` / `backdrop`
//! 使用时**一定要**把 [`nova_core::ImageSet::poster_tag`] /
//! [`nova_core::ImageSet::backdrop_tag`] 一起传进来，否则换图后会永远命中旧图。
//!
//! # 分层
//!
//! - 下载通过 [`Fetcher`] 注入：令牌怎么带（查询串还是请求头）只有服务端适配层知道，
//!   本 crate 不认识 Emby / Jellyfin，因此可以离线测试。附带的 [`HttpFetcher`]
//!   是一个不重试、限大小的默认实现。
//! - 索引（"哪张图在哪、多大、什么时候用过"）由 [`nova_storage`] 的 `image_cache` 表
//!   负责，本 crate 负责文件本身。索引先于文件的删除由本 crate 的 [`ImageCache::evict`]
//!   收口。
//! - 解码统一成 RGBA8；`max_width` 只缩不放，等比。
//!
//! # 缓存布局
//!
//! `<dir>/<hash 前两位>/<hash>.img`，内容是下载到的**原始编码字节**（PNG/JPEG/WebP），
//! 不是解码后的 RGBA（后者大 5–20 倍，且不同 `max_width` 需要不同尺寸）。
//! 布局、一致性与并发策略的取舍写在 `cache` 模块文档里（`cargo doc --document-private-items` 可见）。
//!
//! # 安全
//!
//! 图片地址通常带访问令牌（`?api_key=…`）。本 crate 的任何 `Debug`、错误与日志
//! 都只用 [`endpoint_of`] 的输出（`scheme://host/path`），不含查询串；
//! 缓存键是 SHA-256，从键推不回 URL。

#![warn(missing_docs)]

mod cache;
mod decode;
mod error;
mod fetch;

pub use cache::{ImageCache, ImageRequest, cache_key};
pub use decode::DecodedImage;
pub use error::{FetchError, ImageError, endpoint_of};
pub use fetch::{DEFAULT_TIMEOUT, FetchFuture, Fetcher, HttpFetcher, MAX_IMAGE_BYTES};
