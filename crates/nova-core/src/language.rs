// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 语言标签。
//!
//! 服务端返回的语言字段非常不统一（`zh`、`chi`、`zh-Hans`、`Chinese`、空字符串都有）。
//! 这里只做保守的原样保存 + 主语言子标签比较：宁可匹配失败，也不要匹配错误语言的字幕
//! 或音轨——错配字幕比没有字幕更糟。

use serde::{Deserialize, Serialize};
use std::fmt;

/// BCP-47 风格的语言标签。ETag 级别的原样字符串，比较时只看主语言子标签。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Language(String);

impl Language {
    /// 原样保存语言标签。
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    /// 未标注语言（服务端常见）。
    pub fn undetermined() -> Self {
        Self::new("und")
    }

    /// 英语。
    pub fn english() -> Self {
        Self::new("en")
    }

    /// 底层字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 主语言子标签（`zh-Hans-CN` -> `zh`）。
    pub fn primary(&self) -> &str {
        self.0.split(['-', '_']).next().unwrap_or(&self.0)
    }

    /// 语言未知。
    pub fn is_undetermined(&self) -> bool {
        matches!(self.primary(), "" | "und" | "unknown")
    }

    /// 主语言子标签是否相同（大小写不敏感）。任一方未标注时恒为 `false`。
    pub fn matches(&self, other: &Self) -> bool {
        !self.is_undetermined()
            && !other.is_undetermined()
            && self.primary().eq_ignore_ascii_case(other.primary())
    }
}

impl Default for Language {
    fn default() -> Self {
        Self::undetermined()
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Language {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Language {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<Option<String>> for Language {
    fn from(value: Option<String>) -> Self {
        match value {
            Some(tag) if !tag.trim().is_empty() => Self::new(tag),
            _ => Self::undetermined(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_subtag_is_extracted() {
        assert_eq!(Language::new("zh-Hans-CN").primary(), "zh");
        assert_eq!(Language::new("pt_BR").primary(), "pt");
        assert_eq!(Language::new("en").primary(), "en");
        assert_eq!(Language::new("").primary(), "");
    }

    #[test]
    fn matching_ignores_region_and_case() {
        let zh_hans = Language::new("zh-Hans");
        let zh_tw = Language::new("ZH-TW");
        assert!(zh_hans.matches(&zh_tw));
        assert!(!zh_hans.matches(&Language::english()));
    }

    #[test]
    fn undetermined_never_matches() {
        let und = Language::undetermined();
        assert!(und.is_undetermined());
        assert!(!und.matches(&Language::undetermined()));
        assert!(!Language::new("").matches(&Language::new("")));
        // 服务端偶尔返回空字符串，不能因此认为两者语言一致。
        assert!(!Language::from(Some(String::new())).matches(&Language::english()));
    }

    #[test]
    fn from_optional_string() {
        assert_eq!(Language::from(None::<String>), Language::undetermined());
        assert_eq!(Language::from(Some("  ".into())), Language::undetermined());
        assert_eq!(Language::from(Some("ja".into())), Language::new("ja"));
    }
}
