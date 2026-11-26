// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 连接状态与降级文案。
//!
//! 桌面端把离线 / 过期状态渲染成顶部**横幅**（`BannerData`）：一句话说明 + 下一步该做的
//! 动作；文案只说应用内按钮真正会做的事（不去终端敲命令），因为横幅动作打开的就是
//! 应用内的添加服务器对话框。不再占用首页 hero 位，hero 恢复显示真实内容或空态。
//!
//! `STATUS_ITEM_ID` 仍保留给历史缓存数据（旧版本可能已把状态卡写进 hero 位的引用路径），
//! 界面回调遇到它直接忽略，不会进入详情页或播放页。

use nova_ui::{BannerData, BannerLevel};

/// 状态卡使用的伪条目 id。
pub const STATUS_ITEM_ID: &str = "nova-status";

/// 当前连接状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// 已连接并且令牌可用。
    Connected {
        /// 服务器显示名。
        server: String,
        /// 服务器地址。
        base_url: String,
    },
    /// 本机还没有保存任何服务器（没跑过 `nova login`）。
    NoServer,
    /// 本机有多台服务器，且启动时没有用 `--server` 指定。
    AmbiguousServer {
        /// 已保存的服务器数量。
        count: usize,
    },
    /// 令牌不在凭证存储里（密钥环不可用、被清掉，或换了机器）。
    MissingToken {
        /// 服务器地址。
        url: String,
        /// 凭证后端的说明（密钥环退化等）。
        note: Option<String>,
    },
    /// 令牌已过期，需要重新登录。
    Expired {
        /// 服务器地址。
        url: String,
    },
    /// 已连接但本次刷新失败（网络、服务端错误、响应不合规……）。
    RefreshFailed {
        /// 失败原因（已脱敏）。
        message: String,
    },
}

impl Status {
    /// 是否可以发请求（`Connected` 或"刷新失败但会话还在"）。
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    /// 结论式标题。
    pub fn headline(&self) -> String {
        match self {
            Self::Connected { server, .. } => format!("已连接 {server}"),
            Self::NoServer => "未连接服务器".to_owned(),
            Self::AmbiguousServer { count } => format!("本机有 {count} 台服务器"),
            Self::MissingToken { .. } => "未登录".to_owned(),
            Self::Expired { .. } => "登录已过期".to_owned(),
            Self::RefreshFailed { .. } => "无法刷新服务器数据".to_owned(),
        }
    }

    /// 下一步提示。
    pub fn hint(&self) -> String {
        match self {
            Self::Connected { base_url, .. } => base_url.clone(),
            Self::NoServer => {
                "在应用内连接服务器（Ctrl+N，或首页「连接服务器」）；本地缓存仍可浏览".to_owned()
            }
            Self::AmbiguousServer { .. } => {
                "启动时用 --server <id> 指定一台；本地缓存仍可浏览".to_owned()
            }
            Self::MissingToken { url, note } => {
                let base = format!("访问令牌不在凭证存储里：用「重新登录」输入密码（{url}）");
                match note {
                    Some(note) => format!("{base}；{note}"),
                    None => format!("{base}；本地缓存仍可浏览"),
                }
            }
            Self::Expired { url } => {
                // 按钮「重新登录」实际打开的是应用内的添加服务器对话框（已预填地址与用户名），
                // 所以文案说按钮会做的事，不让用户去终端敲命令。
                format!("用「重新登录」输入密码（{url}）；本地缓存仍可浏览")
            }
            Self::RefreshFailed { message } => {
                format!("{message}；本地缓存仍可浏览")
            }
        }
    }

    /// 顶部横幅；已连接时返回 `None`（不显示）。
    ///
    /// 令牌问题（过期 / 缺失）是 warning，动作是重新登录；
    /// 没配服务器是 info；刷新失败是 warning，动作是重试。
    pub fn banner_data(&self) -> Option<BannerData> {
        let text = format!("{}：{}", self.headline(), self.hint());
        match self {
            Self::Connected { .. } => None,
            Self::NoServer | Self::AmbiguousServer { .. } => {
                Some(BannerData::new(text, BannerLevel::Info))
            }
            Self::MissingToken { .. } | Self::Expired { .. } => {
                Some(BannerData::new(text, BannerLevel::Warning).with_action("重新登录"))
            }
            Self::RefreshFailed { .. } => {
                Some(BannerData::new(text, BannerLevel::Warning).with_action("重试"))
            }
        }
    }

    /// 供 `--print-view` 使用的稳定文本行。
    pub fn summary_lines(&self) -> Vec<String> {
        vec![
            format!("status: {}", self.headline()),
            format!("hint: {}", self.hint()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_states_are_explicit_and_actionable() {
        let no_server = Status::NoServer;
        assert_eq!(no_server.headline(), "未连接服务器");
        assert!(no_server.hint().contains("Ctrl+N"));
        assert!(!no_server.is_connected());

        let expired = Status::Expired {
            url: "http://127.0.0.1:8096".to_owned(),
        };
        assert_eq!(expired.headline(), "登录已过期");
        assert!(expired.hint().contains("重新登录"));

        let missing = Status::MissingToken {
            url: "http://host:8096".to_owned(),
            note: Some("密钥环不可用".to_owned()),
        };
        assert_eq!(missing.headline(), "未登录");
        assert!(missing.hint().contains("密钥环不可用"));
    }

    /// 横幅文案不得让桌面用户去终端敲命令：带动作按钮的两个状态，按钮打开的是
    /// 应用内的添加服务器对话框（`ui.rs` 的 `on_banner_action`），文案必须与之对应。
    #[test]
    fn banner_copy_never_sends_the_user_to_the_terminal() {
        for status in [
            Status::NoServer,
            Status::AmbiguousServer { count: 2 },
            Status::MissingToken {
                url: "http://host:8096".to_owned(),
                note: None,
            },
            Status::Expired {
                url: "http://host:8096".to_owned(),
            },
            Status::RefreshFailed {
                message: "网络不可达".to_owned(),
            },
        ] {
            let text = status.banner_data().expect("离线应有横幅").text;
            assert!(
                !text.contains("nova login") && !text.contains("终端"),
                "横幅不应把用户推回终端：{text}"
            );
        }
        // 按钮确实存在时，文案里必须有同名按钮的名字（点它就能做）。
        for status in [
            Status::Expired {
                url: "http://host:8096".to_owned(),
            },
            Status::MissingToken {
                url: "http://host:8096".to_owned(),
                note: None,
            },
        ] {
            let banner = status.banner_data().expect("应有横幅");
            let action = banner.action_label.expect("过期 / 缺令牌应有动作按钮");
            assert!(
                banner.text.contains(&action),
                "文案应指向「{action}」按钮：{}",
                banner.text
            );
        }
    }

    #[test]
    fn connected_status_has_no_banner() {
        let status = Status::Connected {
            server: "家里".to_owned(),
            base_url: "http://host:8096".to_owned(),
        };
        assert!(status.is_connected());
        assert!(status.banner_data().is_none());
        assert!(status.headline().contains("家里"));
    }

    #[test]
    fn offline_states_have_banners_with_levels_and_actions() {
        let no_server = Status::NoServer.banner_data().expect("离线应有横幅");
        assert_eq!(no_server.level, BannerLevel::Info);
        assert_eq!(no_server.action_label, None);
        assert!(no_server.text.contains("Ctrl+N"));

        let expired = Status::Expired {
            url: "http://127.0.0.1:8096".to_owned(),
        }
        .banner_data()
        .expect("过期应有横幅");
        assert_eq!(expired.level, BannerLevel::Warning);
        assert_eq!(expired.action_label.as_deref(), Some("重新登录"));

        let failed = Status::RefreshFailed {
            message: "网络不可达".to_owned(),
        }
        .banner_data()
        .expect("刷新失败应有横幅");
        assert_eq!(failed.level, BannerLevel::Warning);
        assert_eq!(failed.action_label.as_deref(), Some("重试"));
    }
}
