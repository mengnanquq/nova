// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 记录编解码：payload JSON、枚举文本列、以及一些不丢信息的整数转换。

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::errors::{Result, StorageError};

/// 把实体序列化成 payload JSON。
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(StorageError::Encode)
}

/// 解析 payload JSON。
///
/// 未出现在当前类型里的字段会被 serde 忽略，因此"新版本写的 payload 被旧版本读到"
/// 不会失败——旧版本只是看不到新字段。
pub(crate) fn decode<T: DeserializeOwned>(
    table: &'static str,
    key: impl Into<String>,
    raw: &str,
) -> Result<T> {
    serde_json::from_str(raw).map_err(|source| StorageError::CorruptPayload {
        table,
        key: key.into(),
        source,
    })
}

/// 把 unit-variant 枚举存成可读文本（走 serde 的命名，避免和 `nova-core` 的表示漂移）。
pub(crate) fn enum_to_text<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value).map_err(StorageError::Encode)? {
        Value::String(text) => Ok(text),
        other => Ok(other.to_string()),
    }
}

/// 从文本列还原 unit-variant 枚举。
pub(crate) fn enum_from_text<T: DeserializeOwned>(
    table: &'static str,
    column: &'static str,
    raw: &str,
) -> Result<T> {
    serde_json::from_value(Value::String(raw.to_owned())).map_err(|source| {
        StorageError::CorruptPayload {
            table,
            key: column.to_owned(),
            source,
        }
    })
}

/// 毫秒时长 → `INTEGER` 列（饱和处理，绝不回绕成负数）。
pub(crate) fn millis(value: Option<std::time::Duration>) -> Option<i64> {
    value.map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
}

/// 秒 → `INTEGER` 列。
pub(crate) fn seconds(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// `u64` 字节数 → `INTEGER` 列。
pub(crate) fn bytes(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// `INTEGER` 列 → 非负 `u64`（历史脏数据里的负数按 0 处理）。
pub(crate) fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// 记录某条 payload 的可读主键，只用于错误信息。
pub(crate) fn item_key(server_id: &str, item_id: &str) -> String {
    format!("server={server_id} item={item_id}")
}

/// 记录资源的可读主键，只用于错误信息。
pub(crate) fn source_key(server_id: &str, item_id: &str, source_id: &str) -> String {
    format!("server={server_id} item={item_id} source={source_id}")
}

/// 当前 Unix 时间（秒）。
///
/// 其他接口的时间戳都由调用方传入（便于测试与回放）；这里只用于图片缓存记一条
/// "刚用过" 的时间。系统时间早于 1970 时返回 0。
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map(|seconds| i64::try_from(seconds).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
