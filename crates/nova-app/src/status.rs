// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 连接状态与降级文案。
//!
//! `nova-ui` 没有专门的"横幅"属性（阶段 3 的界面只有四个页面），所以桌面端把状态
//! 渲染成首页 hero 位上的**状态卡**：标题是结论（"未连接服务器" / "登录已过期"），
//! 副标题是下一步该敲的命令。缓存里的继续观看/最近添加仍然保留在下方，离线可浏览。
//!
//! 状态卡的 `item_id` 固定为 [`STATUS_ITEM_ID`]，界面回调遇到它直接忽略，
//! 不会进入详情页或播放页。

use nova_ui::{CardView, HomeView};

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
            Self::NoServer => "先在终端运行 nova login；本地缓存仍可浏览".to_owned(),
            Self::AmbiguousServer { .. } => {
                "启动时用 --server <id> 指定一台；本地缓存仍可浏览".to_owned()
            }
            Self::MissingToken { url, note } => {
                let base = format!("访问令牌不在凭证存储里：先在终端运行 nova login（{url}）");
                match note {
                    Some(note) => format!("{base}；{note}"),
                    None => format!("{base}；本地缓存仍可浏览"),
                }
            }
            Self::Expired { url } => {
                format!("重新运行 nova login（{url}）；本地缓存仍可浏览")
            }
            Self::RefreshFailed { message } => {
                format!("{message}；本地缓存仍可浏览")
            }
        }
    }

    /// 状态卡；已连接时返回 `None`（不占据 hero 位）。
    pub fn banner(&self) -> Option<CardView> {
        if self.is_connected() {
            return None;
        }
        Some(
            CardView::new(STATUS_ITEM_ID, self.headline())
                .with_subtitle(self.hint())
                .with_badge("离线"),
        )
    }

    /// 供 `--print-view` 使用的稳定文本行。
    pub fn summary_lines(&self) -> Vec<String> {
        vec![
            format!("status: {}", self.headline()),
            format!("hint: {}", self.hint()),
        ]
    }
}

/// 把状态放进首页：非连接状态下覆盖 hero。
pub fn decorate_home(home: &mut HomeView, status: &Status) {
    if let Some(banner) = status.banner() {
        home.hero = Some(banner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_states_are_explicit_and_actionable() {
        let no_server = Status::NoServer;
        assert_eq!(no_server.headline(), "未连接服务器");
        assert!(no_server.hint().contains("nova login"));
        assert!(!no_server.is_connected());

        let expired = Status::Expired {
            url: "http://127.0.0.1:8096".to_owned(),
        };
        assert_eq!(expired.headline(), "登录已过期");
        assert!(expired.hint().contains("nova login"));

        let missing = Status::MissingToken {
            url: "http://host:8096".to_owned(),
            note: Some("密钥环不可用".to_owned()),
        };
        assert_eq!(missing.headline(), "未登录");
        assert!(missing.hint().contains("密钥环不可用"));
    }

    #[test]
    fn connected_status_has_no_banner() {
        let status = Status::Connected {
            server: "家里".to_owned(),
            base_url: "http://host:8096".to_owned(),
        };
        assert!(status.is_connected());
        assert!(status.banner().is_none());
        assert!(status.headline().contains("家里"));
    }

    #[test]
    fn banner_uses_the_status_pseudo_id() {
        let banner = Status::NoServer.banner().expect("离线应有状态卡");
        assert_eq!(banner.item_id, STATUS_ITEM_ID);
        assert_eq!(banner.badge.as_deref(), Some("离线"));
    }

    #[test]
    fn decorate_home_keeps_lists_but_replaces_hero() {
        let mut home = HomeView::new();
        home.recently_added = vec![CardView::new("a", "A")];
        home.hero = Some(CardView::new("a", "A"));
        decorate_home(&mut home, &Status::NoServer);
        assert_eq!(home.hero.expect("状态卡").title, "未连接服务器");
        assert_eq!(home.recently_added.len(), 1, "缓存内容必须保留");
    }
}
