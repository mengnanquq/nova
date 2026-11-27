// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 提供方错误模型。
//!
//! 设计约束（与"凭证不进日志"对应）：
//!
//! - 任何变体都**不**承载访问令牌，也**不**承载带查询串的完整请求地址。
//!   请求地址里的 `api_key` 一旦进错误信息，就会顺着日志、崩溃报告、界面报错漏出去。
//! - 服务端返回的文本会被截断到 [`MAX_SERVER_MESSAGE`] 字符，并做一次脱敏：
//!   `?` 之后的内容（查询串里通常是 `api_key`）一律抹掉。它来自不可信输入，
//!   既不该无限膨胀错误信息，也不该把整段 HTML 错误页塞进界面。

/// 服务端文本进入错误信息时的最大长度（字符数，不是字节数）。
pub const MAX_SERVER_MESSAGE: usize = 200;

/// 提供方错误。
///
/// 变体与"调用方该做什么"一一对应：
///
/// | 变体 | 调用方应对 |
/// |---|---|
/// | [`NoToken`](Self::NoToken) | 本机没有令牌；与“服务器拒绝令牌”区分，不要标成会话过期 |
/// | [`Unauthorized`](Self::Unauthorized) | 重新登录，不要重试 |
/// | [`NotFound`](Self::NotFound) | 该条目已从服务端消失，可从本地缓存剔除 |
/// | [`Http`](Self::Http) | 服务端拒绝；4xx 不要重试，5xx 可延后重试 |
/// | [`Network`](Self::Network) | 网络/超时；幂等 GET 已在本层重试过 |
/// | [`Decode`](Self::Decode) | 响应不合规，多半是版本不兼容 |
/// | [`InvalidResponse`](Self::InvalidResponse) | 同上，用静态字符串以免带出响应内容 |
/// | [`Server`](Self::Server) | 服务端给出的可读原因 |
/// | [`Unsupported`](Self::Unsupported) | 本层不支持（含地址不合规） |
/// | [`Credentials`](Self::Credentials) | 本机凭证存储故障，与服务端无关 |
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// 本机没有访问令牌（未登录 / 钥匙环里被清掉）。
    ///
    /// 与 [`Unauthorized`](Self::Unauthorized) 分开是因为两者该做的事不同：
    /// 这个错误出现在**没发请求**的时候（构造 provider 时不要求有令牌），
    /// 把它当成服务端 401 会把“本地还没登录”误标成“服务器判定会话过期”，
    /// 并且写进本地库。
    #[error("本机没有访问令牌")]
    NoToken,

    /// 未认证，或令牌已失效。
    #[error("未认证或令牌已失效")]
    Unauthorized,

    /// 服务端没有该资源。
    #[error("服务端没有该资源")]
    NotFound,

    /// 服务端返回了非成功的 HTTP 状态码（401 / 404 之外的）。
    #[error("服务端返回 HTTP {status}")]
    Http {
        /// HTTP 状态码。
        status: u16,
    },

    /// 网络层错误或超时。文本已剔除请求地址，不含查询串与令牌。
    #[error("网络错误：{0}")]
    Network(String),

    /// 响应无法解析成预期的 JSON 结构。
    #[error("无法解析服务端响应：{0}")]
    Decode(String),

    /// 响应结构不合规（缺关键字段、类型无法映射等）。只带静态字符串。
    #[error("服务端响应不合规：{0}")]
    InvalidResponse(&'static str),

    /// 服务端给出的可读错误原因（来自响应体的 `Message` 字段）。
    #[error("服务端错误：{message}")]
    Server {
        /// 服务端文本，已抹掉查询串并截断。
        message: String,
    },

    /// 本层不支持的操作，或不合规的服务器地址。
    #[error("不支持：{0}")]
    Unsupported(&'static str),

    /// 本机凭证存储故障（钥匙串未解锁、后端不可用等）。
    ///
    /// 单独一个变体是为了不把"本机钥匙串打不开"误报成服务端错误：
    /// 两者的用户提示与重试策略完全不同。
    #[error("凭证存储错误：{message}")]
    Credentials {
        /// 凭证存储给出的诊断文本（不含任何明文）。
        message: String,
    },
}

impl ProviderError {
    /// 是否值得由调用方延迟重试。
    ///
    /// 本层已经对幂等 GET 的 5xx / 429 与超时做过有界重试，这里描述的是
    /// “上层再试一次还有没有意义”：认证失败、404、解码失败都不值得。
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::Http { status } => *status >= 500 || *status == 429,
            _ => false,
        }
    }

    /// 由 HTTP 状态码构造错误。
    pub fn from_status(status: u16) -> Self {
        match status {
            401 | 403 => Self::Unauthorized,
            404 => Self::NotFound,
            other => Self::Http { status: other },
        }
    }

    /// 构造服务端文本错误，按 [`MAX_SERVER_MESSAGE`] 截断。
    ///
    /// 服务端可能把请求地址（连同 `api_key` 查询串）回显在错误里，
    /// 所以先做一次脱敏：`?` 及其之后的内容一律丢掉。宁可少几个字，
    /// 也不要让令牌顺着错误信息进日志、崩溃报告或界面。
    ///
    /// 截断按字符数而不是字节数，避免把多字节字符切成两半。
    pub fn server_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let trimmed = message.trim();
        let without_query = match trimmed.split_once('?') {
            Some((head, _)) => head.trim_end(),
            None => trimmed,
        };
        let truncated: String = without_query.chars().take(MAX_SERVER_MESSAGE).collect();
        Self::Server { message: truncated }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert!(matches!(
            ProviderError::from_status(401),
            ProviderError::Unauthorized
        ));
        assert!(matches!(
            ProviderError::from_status(403),
            ProviderError::Unauthorized
        ));
        assert!(matches!(
            ProviderError::from_status(404),
            ProviderError::NotFound
        ));
        assert!(matches!(
            ProviderError::from_status(500),
            ProviderError::Http { status: 500 }
        ));
    }

    #[test]
    fn transience_rules() {
        assert!(!ProviderError::Unauthorized.is_transient());
        assert!(!ProviderError::NoToken.is_transient());
        assert!(!ProviderError::NotFound.is_transient());
        assert!(!ProviderError::from_status(400).is_transient());
        assert!(ProviderError::from_status(429).is_transient());
        assert!(ProviderError::from_status(503).is_transient());
        assert!(ProviderError::Network("timeout".into()).is_transient());
        assert!(!ProviderError::Decode("bad".into()).is_transient());
    }

    #[test]
    fn server_message_strips_query_strings() {
        // 服务端可能回显请求地址；查询串里通常带 api_key，绝不能进错误信息/日志。
        let error = ProviderError::server_message(
            "无法访问 https://media.example.com/Items/a?api_key=SECRET-TOKEN&x=1 请稍后再试",
        );
        let rendered = error.to_string();
        assert!(
            !rendered.contains("SECRET-TOKEN"),
            "查询串必须被抹掉：{rendered}"
        );
        assert!(rendered.contains("https://media.example.com/Items/a"));

        // 没有 `?` 的普通文本原样保留。
        assert_eq!(
            ProviderError::server_message("权限不足").to_string(),
            "服务端错误：权限不足"
        );
    }

    #[test]
    fn server_message_is_truncated_by_chars() {
        let long = "错".repeat(MAX_SERVER_MESSAGE + 50);
        let error = ProviderError::server_message(long);
        let ProviderError::Server { message } = &error else {
            panic!("期望 Server 变体");
        };
        assert_eq!(message.chars().count(), MAX_SERVER_MESSAGE);

        let short = ProviderError::server_message("  权限不足  ");
        assert_eq!(
            short.to_string(),
            "服务端错误：权限不足",
            "应当去掉首尾空白"
        );
    }

    #[test]
    fn display_has_no_url_placeholder() {
        let error = ProviderError::from_status(502);
        assert_eq!(error.to_string(), "服务端返回 HTTP 502");
        assert_eq!(
            ProviderError::Unsupported("服务器地址必须是 http 或 https").to_string(),
            "不支持：服务器地址必须是 http 或 https"
        );
    }
}
