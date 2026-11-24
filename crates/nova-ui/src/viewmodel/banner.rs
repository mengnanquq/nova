// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 顶部横幅（通知位）的视图模型：一句话 + 语义级别 + 可选动作。

/// 横幅的语义级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerLevel {
    /// 提示（面板底、正常文字色）。
    Info,
    /// 需要注意（warning 底、深色文字）。
    Warning,
    /// 错误（danger 底、深色文字）。
    Danger,
}

/// 一条顶部横幅。
///
/// `action_label` 为 `Some` 时右侧显示动作按钮，点击触发 `on_banner_action`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BannerData {
    /// 一句话说明。
    pub text: String,
    /// 语义级别。
    pub level: BannerLevel,
    /// 动作按钮文案；`None` 表示没有动作。
    pub action_label: Option<String>,
}

impl BannerData {
    /// 新建一条横幅（无动作按钮）。
    pub fn new(text: impl Into<String>, level: BannerLevel) -> Self {
        Self {
            text: text.into(),
            level,
            action_label: None,
        }
    }

    /// 附带动作按钮文案。
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_defaults_have_no_action() {
        let banner = BannerData::new("登录已过期", BannerLevel::Warning);
        assert_eq!(banner.text, "登录已过期");
        assert_eq!(banner.level, BannerLevel::Warning);
        assert!(banner.action_label.is_none());
    }

    #[test]
    fn banner_action_is_chainable() {
        let banner = BannerData::new("未连接服务器", BannerLevel::Info).with_action("重新登录");
        assert_eq!(banner.action_label.as_deref(), Some("重新登录"));
    }
}
