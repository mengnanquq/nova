// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 服务器信息与认证状态。
//!
//! 安全约束：这里**永不**保存密码或访问令牌。令牌只写入操作系统凭证存储，
//! 本结构中只保留一个不透明的引用名，日志与界面都可以安全地打印整个结构。

use serde::{Deserialize, Serialize};

use crate::ids::ServerId;
use crate::language::Language;

/// 服务器类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerKind {
    /// Emby。
    Emby,
    /// Jellyfin。
    Jellyfin,
}

impl ServerKind {
    /// 人类可读名称。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Emby => "Emby",
            Self::Jellyfin => "Jellyfin",
        }
    }
}

/// 认证状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum AuthState {
    /// 尚未认证。
    Unauthorized,
    /// 已认证。令牌在凭证存储里，以 `credential_key` 引用。
    Authorized {
        /// 显示在界面上的用户名。
        user_name: String,
        /// 凭证存储中的键名（不含令牌本身）。
        credential_key: String,
    },
    /// 令牌失效，需要重新登录。
    Expired {
        /// 上次成功认证的用户名。
        user_name: String,
    },
}

impl AuthState {
    /// 是否可以直接发请求。
    pub fn is_authorized(&self) -> bool {
        matches!(self, Self::Authorized { .. })
    }

    /// 用户名（若有）。
    pub fn user_name(&self) -> Option<&str> {
        match self {
            Self::Unauthorized => None,
            Self::Authorized { user_name, .. } | Self::Expired { user_name } => Some(user_name),
        }
    }
}

/// 已配对的服务器。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// 本机分配的服务器标识。
    pub id: ServerId,
    /// 服务器类型。
    pub kind: ServerKind,
    /// 服务器显示名（来自服务端，可能被用户改写）。
    pub name: String,
    /// 基址，例如 `https://media.example.com:8920`。适配层负责校验 scheme 与证书。
    pub base_url: String,
    /// 服务端版本。
    pub version: Option<String>,
    /// 服务端用户标识。
    pub user_id: Option<String>,
    /// 认证状态。
    pub auth: AuthState,
    /// 界面语言偏好（服务端元数据语言）。
    pub metadata_language: Language,
}

impl ServerInfo {
    /// 服务器地址是否使用明文 HTTP——只有本机 / 局域网自建场景才可接受，
    /// 界面应据此提示用户。
    pub fn is_plaintext(&self) -> bool {
        self.base_url
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(base_url: &str) -> ServerInfo {
        ServerInfo {
            id: ServerId::new("srv"),
            kind: ServerKind::Jellyfin,
            name: "Home".into(),
            base_url: base_url.into(),
            version: Some("10.9.0".into()),
            user_id: None,
            auth: AuthState::Authorized {
                user_name: "alice".into(),
                credential_key: "jellyfin/srv/token".into(),
            },
            metadata_language: Language::new("zh-CN"),
        }
    }

    #[test]
    fn auth_state_helpers() {
        assert!(server("https://x").auth.is_authorized());
        assert_eq!(server("https://x").auth.user_name(), Some("alice"));
        assert!(!AuthState::Unauthorized.is_authorized());
        assert_eq!(
            AuthState::Expired {
                user_name: "bob".into()
            }
            .user_name(),
            Some("bob")
        );
    }

    #[test]
    fn plaintext_detection() {
        assert!(server("http://192.168.1.10:8096").is_plaintext());
        assert!(server("HTTP://nas.local:8096").is_plaintext());
        assert!(!server("https://media.example.com").is_plaintext());
    }

    #[test]
    fn display_names() {
        assert_eq!(ServerKind::Emby.display_name(), "Emby");
        assert_eq!(ServerKind::Jellyfin.display_name(), "Jellyfin");
    }
}
