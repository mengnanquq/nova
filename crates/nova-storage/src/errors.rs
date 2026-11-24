// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 存储层错误。

use std::path::Path;

/// 存储层的统一错误类型。
///
/// 错误信息里只放表名、主键与路径，**不放**任何凭证内容——本 crate 根本不存凭证。
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// 打开数据库失败：路径不可写、文件不是 SQLite 数据库，或文件已被截断/损坏。
    #[error("打开 SQLite 数据库失败（{path}）：{source}；文件可能不是 SQLite 数据库或已损坏")]
    Open {
        /// 数据库文件路径；内存数据库为 `:memory:`。
        path: String,
        /// 底层 SQLite 错误。
        source: rusqlite::Error,
    },

    /// schema 迁移失败。事务已回滚，数据库保持迁移前的状态。
    #[error("schema 迁移失败（{from} → {to}），已回滚：{source}")]
    Migrate {
        /// 迁移前的 `PRAGMA user_version`。
        from: i64,
        /// 本 crate 支持的版本。
        to: i64,
        /// 底层 SQLite 错误。
        source: rusqlite::Error,
    },

    /// 数据库由更新版本的 Nova 写过，当前版本无法安全降级。
    #[error("数据库 schema 版本为 {found}，高于本程序支持的 {supported}；请升级 Nova")]
    SchemaTooNew {
        /// 数据库里的 `PRAGMA user_version`。
        found: i64,
        /// 本 crate 支持的版本。
        supported: i64,
    },

    /// 一条记录里的 JSON 无法解析。
    ///
    /// 读取时**直接报错**而不是跳过该行：静默跳过会让界面显示一个"少了东西"的媒体库，
    /// 而服务端仍然完整。把损坏暴露给调用方，由它决定重新同步还是清空缓存。
    #[error("{table} 中的记录 {key} 已损坏，无法解析：{source}")]
    CorruptPayload {
        /// 表名。
        table: &'static str,
        /// 记录主键的可读形式（`server=… item=…`）。
        key: String,
        /// 解析错误。
        source: serde_json::Error,
    },

    /// 写入前序列化失败。
    #[error("序列化待写入的记录失败：{0}")]
    Encode(#[source] serde_json::Error),

    /// 其他 SQLite 错误。
    #[error("SQLite 错误：{0}")]
    Sqlite(#[from] rusqlite::Error),

    /// 连接互斥锁被毒化：此前有线程在持锁时 panic。
    #[error("数据库连接锁已被毒化（此前有线程在持锁时 panic）")]
    Poisoned,
}

impl StorageError {
    /// 构造 [`StorageError::Open`]，把路径写进错误信息。
    pub(crate) fn open(path: Option<&Path>, source: rusqlite::Error) -> Self {
        Self::Open {
            path: path
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| ":memory:".to_owned()),
            source,
        }
    }
}

/// 存储层的结果类型。
pub type Result<T> = std::result::Result<T, StorageError>;
