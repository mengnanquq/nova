// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 命令行错误。
//!
//! 两条规则（和 `nova-provider` 的错误模型一致）：
//!
//! 1. 错误信息里**不出现**访问令牌，也不出现带查询串的完整地址；
//! 2. 每条错误尽量配一句 [`CliError::hint`]——告诉用户下一步该敲什么命令，
//!    而不是只说"失败了"。

use nova_provider::ProviderError;
use nova_secrets::SecretError;
use nova_storage::StorageError;

/// 命令行错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CliError {
    /// 参数不合法（未知命令、缺必需参数、数值解析失败）。
    #[error("用法错误：{0}")]
    Usage(String),

    /// 运行环境不满足要求（状态目录无法确定、URL 不合规等）。
    #[error("配置错误：{0}")]
    Config(String),

    /// 本机还没有保存任何服务器。
    #[error("本机还没有保存任何服务器")]
    NoServer,

    /// `--server` 指的 id 不存在。
    #[error("本机没有 id 为 `{0}` 的服务器")]
    ServerNotFound(String),

    /// 本机保存了多台服务器，且没有用 `--server` 指定。
    #[error("本机保存了多台服务器，请用 `--server <id>` 指定其中一台：{0}")]
    AmbiguousServer(String),

    /// 令牌不在凭证存储里（或本地记录缺用户标识），无法继续。
    #[error("没有可用的会话：{reason}。请重新运行 `nova login --url {url} --user <NAME>`")]
    NoSession {
        /// 服务器地址。
        url: String,
        /// 为什么没有会话（密钥环不可用、令牌已被清除……）。
        reason: String,
    },

    /// 默认拒绝把密码发往明文连接。
    #[error("拒绝把密码发往明文连接 {url}")]
    PlaintextRefused {
        /// 服务器地址。
        url: String,
    },

    /// 服务端返回 401。
    #[error("服务端拒绝了请求（401）：{what}")]
    Unauthorized {
        /// 当时在做什么。
        what: String,
    },

    /// 服务端返回 404。
    #[error("服务端没有 {what}")]
    NotFound {
        /// 当时在找什么。
        what: String,
    },

    /// 服务端返回其他非成功状态码。
    #[error("服务端返回 HTTP {status}（{what}）")]
    Http {
        /// 状态码。
        status: u16,
        /// 当时在做什么。
        what: String,
    },

    /// 响应结构与预期不符。
    #[error("无法理解服务端响应：{0}")]
    Response(String),

    /// 网络层错误或超时（文本已剔除请求地址）。
    #[error("网络错误：{0}")]
    Network(String),

    /// 登录失败（用户名或密码不对）。
    #[error("认证失败：{0}")]
    Auth(String),

    /// 凭证存储故障。
    #[error("凭证存储错误：{0}")]
    Secret(#[from] SecretError),

    /// 本地数据库故障。
    #[error("本地数据库错误：{0}")]
    Storage(#[from] StorageError),

    /// provider 层错误（未做翻译的那些）。
    #[error("服务端错误：{0}")]
    Provider(#[from] ProviderError),

    /// 标准 IO。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),
}

impl CliError {
    /// 下一步该做什么。`None` 表示错误信息本身已经够用。
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Usage(_) => Some("运行 `nova --help` 查看完整用法".to_owned()),
            Self::NoServer => Some(
                "配对一台服务器：`nova login --url http://<主机>:8096 --user <用户名> --password-stdin`"
                    .to_owned(),
            ),
            Self::ServerNotFound(_) => Some("用 `nova servers` 查看本机已保存的服务器 id".to_owned()),
            Self::AmbiguousServer(_) => {
                Some("`nova servers` 第一列就是 `--server` 的取值".to_owned())
            }
            Self::NoSession { url, .. } => Some(format!(
                "重新登录：`nova login --url {url} --user <用户名> --password-stdin`"
            )),
            Self::PlaintextRefused { .. } => Some(
                "确认这是自己的局域网服务器后再加 `--insecure-plaintext`。\
                 该开关只允许把密码发往明文连接，**不会**放宽 TLS 证书校验——\
                 Nova 不提供跳过证书校验的选项。"
                    .to_owned(),
            ),
            Self::NotFound { .. } => Some(
                "用 `nova items --library <id>` 或 `nova libraries` 找正确的 id".to_owned(),
            ),
            Self::Http { status: 400, .. } => Some(
                "多半是参数不对：库 / 条目 id 用 `nova libraries` / `nova items` 确认".to_owned(),
            ),
            Self::Unauthorized { .. } => {
                Some("运行 `nova login ...` 重新登录（令牌可能已被服务端吊销）".to_owned())
            }
            Self::Secret(_) => Some(
                "解锁系统密钥环（GNOME Keyring / KWallet / macOS Keychain）后重试；\
                 没有可用密钥环时令牌无法持久化"
                    .to_owned(),
            ),
            Self::Provider(ProviderError::Unauthorized) => {
                Some("运行 `nova login ...` 重新登录".to_owned())
            }
            Self::Provider(ProviderError::Credentials { .. }) => Some(
                "系统密钥环不可用：解锁它，或用 `NOVA_CREDENTIALS=memory` 明确接受\
                 \"本次进程内有效、退出即丢\""
                    .to_owned(),
            ),
            Self::Provider(ProviderError::Network(_)) => {
                Some("检查服务器是否在运行、地址与端口是否正确".to_owned())
            }
            _ => None,
        }
    }

    /// 进程退出码：用法错误走 2（shell 惯例），其余 1。
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            _ => 1,
        }
    }
}

/// `ProviderError` → `CliError`。认证失败单独翻译成带引导的文本。
pub(crate) fn provider(error: ProviderError) -> CliError {
    match error {
        ProviderError::Unauthorized => CliError::Auth("用户名或密码不对，或令牌已失效".to_owned()),
        ProviderError::Unsupported(message) => CliError::Config(message.to_owned()),
        other => CliError::Provider(other),
    }
}
