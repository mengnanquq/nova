// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 服务端作用域内的稳定标识符。

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// 由服务端返回的原始标识构造。
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// 借用底层字符串。
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// 取出底层字符串。
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

id_type!(
    /// 媒体条目标识（电影 / 剧集 / 季 / 集）。只在单个服务器内唯一，
    /// 跨服务器组合时需与 [`ServerId`] 配对使用。
    ItemId
);

id_type!(
    /// 媒体资源标识（一个可播放的文件版本）。
    SourceId
);

id_type!(
    /// 服务器标识，首次配对时由本机生成，与服务器地址无关。
    ServerId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_transparently() {
        let json = serde_json::to_string(&ItemId::new("abc")).unwrap();
        assert_eq!(json, "\"abc\"");
        let back: ItemId = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_str(), "abc");
    }

    #[test]
    fn ids_display_and_order() {
        assert_eq!(SourceId::from("42").to_string(), "42");
        let mut ids = [ItemId::new("b"), ItemId::new("a")];
        ids.sort();
        assert_eq!(ids[0].as_str(), "a");
    }
}
