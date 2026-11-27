// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 错误类型与地址脱敏。
//!
//! 两条硬规则贯穿本 crate 的所有错误：
//!
//! 1. 错误里**不放完整 URL**。媒体服务器的图片地址通常带访问令牌
//!    （`?api_key=…`），而错误会进日志、进界面提示、被用户复制出去。
//!    这里一律用 [`endpoint_of`] 把地址压成 `scheme://host/path`，
//!    查询串与片段直接丢掉。
//! 2. 不吞掉"为什么失败"：状态码、字节数、底层原因都保留，否则线上只能看到
//!    "加载失败"四个字。

/// 把图片地址压成可以安全外传的形式：`scheme://host[:port]/path`。
///
/// 丢弃查询串与片段（令牌通常在查询串里），丢弃 userinfo（`user:pass@host` 里的口令）。
/// 无法识别的输入按原样返回查询串之前的部分——宁可少信息，不可泄露令牌。
///
/// 自定义 [`Fetcher`](crate::Fetcher) 实现构造错误时也应当用这个口径，
/// 否则错误会顺着日志或界面提示把令牌带出去。
pub fn endpoint_of(url: &str) -> String {
    // `split` 至少产出一项，`unwrap_or` 只是给一个不 panic 的兜底。
    let head = url.split(['?', '#']).next().unwrap_or(url);

    let (scheme_end, authority_and_path) = match head.find("://") {
        Some(index) => (index + 3, &head[index + 3..]),
        None => (0, head),
    };

    let authority_end = match authority_and_path.find('/') {
        Some(index) => index,
        None => authority_and_path.len(),
    };
    let authority = &authority_and_path[..authority_end];
    let authority = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };

    let mut endpoint = String::with_capacity(head.len());
    endpoint.push_str(&head[..scheme_end]);
    endpoint.push_str(authority);
    endpoint.push_str(&authority_and_path[authority_end..]);
    endpoint
}

/// 下载图片失败。
///
/// 变体里只有脱敏地址、状态码、大小上限，**没有**响应体与完整 URL。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchError {
    /// 构造 HTTP 客户端失败（证书库初始化、TLS 后端配置等）。
    ///
    /// 发生在发出任何请求之前，因此没有地址可报。
    #[error("创建 HTTP 客户端失败：{source}")]
    Client {
        /// 已经去掉 URL 的底层错误。
        #[source]
        source: reqwest::Error,
    },

    /// 服务端返回了非 2xx 状态码。
    ///
    /// 404 通常意味着服务端刚换过图、缓存里的版本已经失效；401/403 意味着令牌过期；
    /// 两者都不该被重试成"看起来像网络故障"。状态码原样交给调用方判断。
    #[error("图片请求返回 HTTP {status}（{endpoint}）")]
    Http {
        /// HTTP 状态码。
        status: u16,
        /// 脱敏地址（`scheme://host/path`）。
        endpoint: String,
    },

    /// 网络层失败：连接被拒、TLS 握手失败、超时、响应体读到一半断开。
    ///
    /// `reqwest::Error` 默认会把完整 URL（含查询串）打进 `Display`，所以构造时就
    /// 调用 [`reqwest::Error::without_url`] 去掉它，只留下原因。
    #[error("图片请求失败（{endpoint}）：{source}")]
    Transport {
        /// 脱敏地址（`scheme://host/path`）。
        endpoint: String,
        /// 已经去掉 URL 的底层错误。
        #[source]
        source: reqwest::Error,
    },

    /// 响应体超过本 crate 接受的上限。
    ///
    /// 图片是外部输入，一个几 GB 的"图片"足够把内存吃光；宁可报错也不分配。
    #[error("图片体积超过上限 {limit} 字节（{endpoint}）")]
    TooLarge {
        /// 允许的最大字节数。
        limit: u64,
        /// 脱敏地址（`scheme://host/path`）。
        endpoint: String,
    },

    /// 当前没有可用的下载会话（未登录 / 离线），本次未尝试下载。
    ///
    /// 与“下载失败”严格区分：缓存未命中时只是跳过本次装载，**不能**记成
    /// “这张图坏了、以后不再试”——否则登录之后图片也永远不会重试。
    #[error("没有可用的下载会话，跳过本次图片请求")]
    Unavailable,
}

/// 图片缓存的统一错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImageError {
    /// 缓存目录/文件操作失败。
    ///
    /// 路径是本地缓存路径（形如 `<cache>/ab/<hash>.img`），不含服务器地址与令牌。
    #[error("图片缓存文件操作失败（{path}）：{source}")]
    Io {
        /// 本地路径。
        path: String,
        /// 底层 I/O 错误。
        #[source]
        source: std::io::Error,
    },

    /// 图片缓存索引（SQLite）失败。
    #[error("图片缓存索引操作失败：{0}")]
    Storage(#[from] nova_storage::StorageError),

    /// 下载失败，转发 [`FetchError`]。
    #[error(transparent)]
    Fetch(#[from] FetchError),

    /// 服务端返回了 2xx 但响应体是空的。
    ///
    /// 与"解码失败"分开：空响应多半是服务端或反向代理的问题，不是图片格式问题。
    #[error("图片响应为空（{endpoint}）")]
    EmptyBody {
        /// 脱敏地址（`scheme://host/path`）。
        endpoint: String,
    },

    /// 拿到字节了，但不是能解码的图片（截断、HTML 错误页、别的格式）。
    ///
    /// 只带脱敏地址与字节数：字节数足以区分"下了一点点就断了"和"下了一整个错误页"。
    #[error("无法解码图片（{endpoint}，{bytes} 字节）：{source}")]
    Decode {
        /// 脱敏地址（`scheme://host/path`）。
        endpoint: String,
        /// 参与解码的字节数。
        bytes: usize,
        /// `image` crate 的解码错误。
        #[source]
        source: image::ImageError,
    },
}

impl ImageError {
    /// 构造 [`ImageError::Io`]，把路径写进错误信息。
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_drops_query_and_fragment() {
        assert_eq!(
            endpoint_of("https://media.example.com:8096/Items/42/Images/Primary?api_key=secret"),
            "https://media.example.com:8096/Items/42/Images/Primary"
        );
        assert_eq!(
            endpoint_of("http://host/Images/backdrop#frag"),
            "http://host/Images/backdrop"
        );
    }

    #[test]
    fn endpoint_drops_userinfo() {
        assert_eq!(
            endpoint_of("https://user:pass@host/path?token=1"),
            "https://host/path"
        );
    }

    #[test]
    fn endpoint_keeps_bare_hosts_and_odd_input() {
        assert_eq!(endpoint_of("https://host"), "https://host");
        assert_eq!(endpoint_of("https://host?x=1"), "https://host");
        assert_eq!(endpoint_of("not a url?token=secret"), "not a url");
    }
}
