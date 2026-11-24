// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 下载抽象：[`Fetcher`] 与本 crate 提供的默认实现 [`HttpFetcher`]。
//!
//! 为什么下载要由调用方注入：图片地址的认证方式属于服务端适配层
//! （Emby / Jellyfin 的令牌可能放查询串、也可能放请求头），缓存层只认
//! "给一个地址、还一堆字节"。这样缓存逻辑可以离线测试，也不会把令牌处理
//! 扩散到两个 crate 里。
//!
//! ## 为什么是 `Pin<Box<dyn Future>>` 而不是 `impl Future`
//!
//! 注入是运行期的（[`ImageCache::get_or_fetch`](crate::ImageCache::get_or_fetch)
//! 收 `&dyn Fetcher`），而带 `impl Future` 返回类型的方法会让 trait 失去 dyn 兼容性，
//! 两者不可兼得。这里选 dyn：调用方可以按服务器切换实现、可以在测试里塞假实现，
//! 代价是每次请求一次堆分配——单张图片的下载与解码面前，这一次分配可以忽略。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::error::{FetchError, endpoint_of};

/// 允许的最大图片字节数（32 MiB）。
///
/// 4K 截图与 HDR 海报也远小于这个数；再大的东西不是图片，或者不该以"整张读进内存"的方式加载。
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// 默认的单次请求超时。
///
/// 图片是"锦上添花"的请求：卡住 15 秒还没有响应，界面就该先显示占位图，
/// 而不是让用户对着空白格子等。重试由上层决定。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// [`Fetcher::fetch`] 返回的 future 类型。
pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, FetchError>> + Send + 'a>>;

/// 图片下载器。
///
/// 实现者只需把地址换成字节；缓存键、解码、落盘都由本 crate 负责。
/// 约定：
///
/// - 成功返回**原始响应体**（已经是 PNG/JPEG/WebP 的字节），不做解码；
/// - 返回空 `Vec` 表示"拿到了 2xx 但体为空"，缓存层会报
///   [`ImageError::EmptyBody`](crate::ImageError::EmptyBody)；
/// - 错误里不要放完整 URL，用 [`endpoint_of`](crate::endpoint_of) 同等口径脱敏。
pub trait Fetcher: Send + Sync {
    /// 下载 `url` 指向的图片字节。
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a>;
}

/// 用 `reqwest` 实现的默认下载器。
///
/// 不做重试（重试策略属于服务端适配层：要不要带 `Retry-After`、哪些状态码该等，
/// 只有它们知道），但会限制响应体大小。
pub struct HttpFetcher {
    client: reqwest::Client,
    max_bytes: u64,
}

impl HttpFetcher {
    /// 用调用方提供的客户端构造。
    ///
    /// 传入的客户端决定代理、超时、证书库等全部行为——本 crate 不做任何 TLS 放宽。
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            max_bytes: MAX_IMAGE_BYTES,
        }
    }

    /// 用一个默认配置的客户端构造：15 秒超时、系统根证书。
    pub fn try_default() -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|source| FetchError::Client {
                source: source.without_url(),
            })?;
        Ok(Self::new(client))
    }

    /// 设置响应体大小上限（字节）。
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let endpoint = endpoint_of(url);

        let response =
            self.client
                .get(url)
                .send()
                .await
                .map_err(|source| FetchError::Transport {
                    endpoint: endpoint.clone(),
                    source: source.without_url(),
                })?;

        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Http {
                status: status.as_u16(),
                endpoint,
            });
        }

        // `Content-Length` 只是服务端的声明，先按它拦一次，避免为一个大响应分配缓冲。
        if let Some(length) = response.content_length() {
            if length > self.max_bytes {
                return Err(FetchError::TooLarge {
                    limit: self.max_bytes,
                    endpoint,
                });
            }
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|source| FetchError::Transport {
                endpoint: endpoint.clone(),
                source: source.without_url(),
            })?;

        // 没有 `Content-Length`（分块传输）时按真实长度再拦一次。
        if bytes.len() as u64 > self.max_bytes {
            return Err(FetchError::TooLarge {
                limit: self.max_bytes,
                endpoint,
            });
        }

        Ok(bytes.to_vec())
    }
}

impl std::fmt::Debug for HttpFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetcher")
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

impl Fetcher for HttpFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        Box::pin(self.download(url))
    }
}
