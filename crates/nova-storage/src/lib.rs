// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 本地持久化层（SQLite）。
//!
//! 这一层负责"重启之后还要在"的东西：服务器列表、媒体索引缓存、用户偏好、
//! 同步游标、图片缓存索引。**凭证不在这里**——访问令牌只经由 `nova-secrets`
//! 进操作系统凭证存储，`ServerInfo` 里只保留凭证存储的键名，本 crate 的 schema
//! 里也没有任何存令牌的列（有测试扫描全部列名做守卫）。
//!
//! # 依赖方向
//!
//! 只依赖 `nova-core`：实体类型（[`nova_core::MediaItem`]、[`nova_core::MediaSource`]、
//! [`nova_core::UserPreferences`]、[`nova_core::ServerInfo`]）由 core 定义，本层负责存。
//! 上层（`nova-library`、`nova-app`）只用 [`Database`] 上的方法，拿不到裸连接。
//!
//! # 连接与并发
//!
//! [`Database`] 内部是 `Mutex<Connection>`，不做连接池（理由见 [`Database`] 的文档）。
//! 读写都是同步阻塞的：媒体中心的存储操作是短促爆发（一批条目、一条进度），
//! 调用方若要放进 async 上下文，应当用阻塞任务池，而不是在这层硬塞 async。
//!
//! # 连接配置
//!
//! - `journal_mode=WAL`：写不阻塞读。同步时一次写入几千条，界面同时可能在浏览。
//! - `synchronous=NORMAL`：WAL 下掉电最多丢最后一次提交，不会损坏数据库；
//!   `FULL` 每次提交都 fsync，批量写入会慢一个数量级。
//! - `foreign_keys=ON`：SQLite 默认关闭，必须在**每个**连接上显式打开，
//!   否则 `ON DELETE CASCADE` 静默失效。
//! - `busy_timeout=5s`：让争锁的连接自己等一会儿，而不是立刻抛 `SQLITE_BUSY`。
//!
//! # 版本与迁移
//!
//! 用 `PRAGMA user_version` 做前向迁移；[`Database::open`] 会自动把旧库补到当前版本，
//! 整个迁移在一个事务里，失败即回滚，不会留下半迁移状态。幂等：版本已是最新时不做写入，
//! 因此连续打开同一个库不会产生副作用。数据库版本高于程序支持版本时直接报错，
//! 不尝试降级。
//!
//! # 数据可重建
//!
//! `items` / `sources` / `sync_state` 都只是缓存，服务端是唯一事实来源，整库删除后
//! 重新同步即可恢复。只有 `preferences`（用户偏好）和 `servers`（本机分配的服务器 id、
//! 凭证键名）是真正不可重建的，改动这两张表时要格外小心。
//!
//! # 数据格式取舍：payload JSON + 少量归一化列
//!
//! 条目与资源各有一列 `payload`，存完整 `nova-core` 结构的 JSON，是**读取的唯一来源**；
//! 同时把过滤/排序要用的字段摊平成列（`kind`、`title`、`sort_title`、`year`、
//! `runtime_ms`、`parent_id`、`index_number`、`played`、`resume_ms`、`updated_at`）。
//!
//! - `nova-core` 加字段**不需要迁移数据库**：旧 payload 缺的字段由 serde 默认值兜底，
//!   新 payload 里的未知字段被旧代码忽略（两个方向都有测试）。
//! - 过滤留在 SQL 侧："继续观看"、按父项列子项、按标题排序都能走索引，
//!   不会把整张表读进内存再筛。
//! - 归一化列只服务于 SQL，不参与重建实体，所以不必与 payload 逐字段保持一致。
//!
//! # 损坏数据
//!
//! payload 解析失败时**报错**（[`StorageError::CorruptPayload`]）而不是跳过该行：
//! 静默跳过会让界面显示一个"莫名其妙少了几部片子"的媒体库，而服务端其实完好，
//! 用户和开发者都无从判断发生了什么。把错误抛给调用方，由它决定重新同步或清空缓存。

#![warn(missing_docs)]

mod codec;
mod database;
mod errors;
mod image_cache;
mod items;
mod libraries;
mod mylist;
mod prefs;
mod schema;
mod servers;
mod sources;
mod sync;

pub use codec::unix_now;
pub use database::Database;
pub use errors::{Result, StorageError};
pub use image_cache::CachedImage;
pub use items::library_items;
pub use libraries::StoredLibrary;
pub use servers::StoredServer;
pub use sync::SyncState;
