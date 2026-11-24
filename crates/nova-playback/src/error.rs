// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放错误分类与用户可读文案。
//!
//! 目标：界面上永远不出现 `reqwest` / `mpv` 的原始错误。用户要看到"服务器不可达，检查网络或
//! 服务器状态"这种能立刻行动的话；原始细节只留在日志里，而且**不能带 URL 与令牌**
//! （播放直链的查询串里就有 `api_key`）。

use std::fmt;

/// 播放失败的分类。
///
/// 分类决定三件事：界面文案（[`hint`](PlaybackError::hint)）、是否值得重试
/// （[`is_retryable`](PlaybackError::is_retryable)）、以及是否要换一条资源而不是重试。
#[derive(Clone, PartialEq, Eq)]
pub enum PlaybackError {
    /// 容器或编码引擎解不了（需要服务端转码，或换一个版本）。
    Unsupported,
    /// 网络不可达 / 连接被重置 / 读取超时。
    Network,
    /// 认证失败或令牌过期。
    Auth,
    /// 服务端返回 404：文件被移动或删除。
    NotFound,
    /// 解码器中途失败（文件损坏、码流异常）。
    Decode,
    /// 连续 15 秒没有任何进度或新帧，判定卡死。
    Stalled,
    /// 引擎内部错误。原始文本只用于日志，**不要**直接展示给用户。
    Engine(String),
}

impl PlaybackError {
    /// 构造引擎内部错误。
    pub fn engine(message: impl Into<String>) -> Self {
        Self::Engine(message.into())
    }

    /// 给用户看的可行动文案（中文，不含 URL / 令牌 / 引擎原始输出）。
    pub fn hint(&self) -> &'static str {
        match self {
            Self::Unsupported => {
                "这个文件的容器或编码当前引擎解不了，换一个版本，或让服务器转码后再试"
            }
            Self::Network => "服务器不可达，检查网络或服务器状态",
            Self::Auth => "登录状态已失效，请重新登录服务器",
            Self::NotFound => "服务器上找不到这个文件，可能已被移动或删除",
            Self::Decode => "文件损坏或编码不兼容，解码失败了",
            Self::Stalled => "连续 15 秒没有收到画面，网络或服务器可能卡住了，点重试可继续",
            Self::Engine(_) => "播放引擎内部出错，重试通常能恢复；若反复出现请查看日志",
        }
    }

    /// 直接重试是否有意义。
    ///
    /// `Unsupported` / `NotFound` / `Auth` 重试多少次都一样失败；`Decode` 是确定性的数据问题。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Network | Self::Stalled | Self::Engine(_))
    }

    /// 是否需要用户先处理账号或地址（界面应引导到设置页而不是重试）。
    pub fn needs_user_action(&self) -> bool {
        matches!(self, Self::Auth | Self::NotFound)
    }

    /// 引擎原始文本（已脱敏）。仅用于日志，**不要**塞进界面文案。
    pub fn raw_message(&self) -> Option<String> {
        match self {
            Self::Engine(message) => Some(redact_secrets(message)),
            _ => None,
        }
    }
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.hint())
    }
}

impl fmt::Debug for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(message) => f
                .debug_tuple("Engine")
                .field(&redact_secrets(message))
                .finish(),
            Self::Unsupported => f.write_str("Unsupported"),
            Self::Network => f.write_str("Network"),
            Self::Auth => f.write_str("Auth"),
            Self::NotFound => f.write_str("NotFound"),
            Self::Decode => f.write_str("Decode"),
            Self::Stalled => f.write_str("Stalled"),
        }
    }
}

impl std::error::Error for PlaybackError {}

/// 把自由文本里的地址与令牌替换成 `<已脱敏>`。
///
/// 规则刻意保守：任何含 `://`、`api_key`、`apikey`、`token`、`password` 的"词"整体替换，
/// 宁可多抹掉一些诊断信息，也不要漏出播放直链里的一次性令牌。
pub fn redact_secrets(text: &str) -> String {
    text.split_whitespace()
        .map(redact_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_word(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let sensitive = lower.contains("://")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("authorization");
    if sensitive {
        "<已脱敏>".to_owned()
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_are_actionable_chinese_and_hide_secrets() {
        let errors = [
            PlaybackError::Unsupported,
            PlaybackError::Network,
            PlaybackError::Auth,
            PlaybackError::NotFound,
            PlaybackError::Decode,
            PlaybackError::Stalled,
            PlaybackError::engine(
                "mpv: failed to open http://192.168.1.9:8096/video?api_key=SECRET",
            ),
        ];
        for error in errors {
            let hint = error.hint();
            assert!(!hint.is_empty());
            assert!(
                hint.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "文案应为中文：{hint}"
            );
            for forbidden in ["http", "://", "api_key", "SECRET", "192.168"] {
                assert!(
                    !hint.contains(forbidden),
                    "提示里不许出现 {forbidden}：{hint}"
                );
            }
        }
    }

    #[test]
    fn engine_message_is_redacted_everywhere_but_raw_access() {
        let error =
            PlaybackError::engine("open https://host/x?api_key=abc123 timeout after token=zzz");
        let debug = format!("{error:?}");
        let display = format!("{error}");
        for text in [&debug, &display] {
            assert!(!text.contains("abc123"), "{text}");
            assert!(!text.contains("host"), "{text}");
            assert!(!text.contains("token="), "{text}");
        }
        let raw = error.raw_message().unwrap_or_default();
        assert!(!raw.contains("abc123"));
        assert!(raw.contains("timeout"), "非敏感的上下文应保留：{raw}");
        assert_eq!(PlaybackError::Network.raw_message(), None);
    }

    #[test]
    fn retry_classification() {
        assert!(PlaybackError::Network.is_retryable());
        assert!(PlaybackError::Stalled.is_retryable());
        assert!(PlaybackError::engine("x").is_retryable());
        assert!(!PlaybackError::Unsupported.is_retryable());
        assert!(!PlaybackError::Auth.is_retryable());
        assert!(!PlaybackError::NotFound.is_retryable());
        assert!(!PlaybackError::Decode.is_retryable());

        assert!(PlaybackError::Auth.needs_user_action());
        assert!(PlaybackError::NotFound.needs_user_action());
        assert!(!PlaybackError::Stalled.needs_user_action());
    }
}
