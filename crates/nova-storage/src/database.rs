// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 数据库句柄、连接配置与迁移。

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

use crate::errors::{Result, StorageError};
use crate::schema::{self, SCHEMA_VERSION};

/// 打开数据库时等待锁释放的上限。
///
/// 媒体中心的写入是短促爆发（一次同步一批条目），但同一进程里可能同时有图片缓存的
/// 后台写入；5 秒足以吃掉一次 checkpoint 或另一个连接的短事务，又不至于把界面卡住。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 本地 SQLite 数据库。
///
/// 内部是一个 `Mutex<Connection>`：**不做连接池**。理由是媒体中心的访问模式是
/// "一次性拉一批条目 + 偶尔写一条进度"，单条语句都是毫秒级，池化带来的
/// 并发度用不上，却要付出连接间 `PRAGMA` 一致性、WAL 文件句柄数量、
/// 以及事务可见性管理的复杂度。真正的并发压力（HTTP、解码）都不在本层。
/// 需要多线程时，`Mutex` 保证串行化，调用方不必关心这一点。
///
/// 打开时会做三件事：配置连接、跑迁移、校验 `user_version`。任何一步失败都返回
/// 错误而不是 panic；迁移在事务内执行，失败即回滚，不会留下半迁移状态。
pub struct Database {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

impl Database {
    /// 打开（必要时创建）指定路径的数据库文件。
    ///
    /// 传进来一个不是 SQLite 数据库、或已被截断损坏的文件会返回
    /// [`StorageError::Open`]，错误信息里带上路径。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn =
            Connection::open(path).map_err(|source| StorageError::open(Some(path), source))?;
        Self::initialize(conn, Some(path))
    }

    /// 打开一个纯内存数据库（测试与"临时缓存"场景）。
    pub fn open_in_memory() -> Result<Self> {
        let conn =
            Connection::open_in_memory().map_err(|source| StorageError::open(None, source))?;
        Self::initialize(conn, None)
    }

    /// 当前的 `PRAGMA user_version`，即已应用的 schema 版本。
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.conn()?;
        read_user_version(&conn).map_err(|source| StorageError::open(None, source))
    }

    fn initialize(mut conn: Connection, path: Option<&Path>) -> Result<Self> {
        configure(&conn, path)?;
        migrate(&mut conn, path)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 借用连接。公开 API 只暴露具体方法，连接本身不外流。
    pub(crate) fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| StorageError::Poisoned)
    }
}

/// 连接级配置。理由逐条写在语句旁。
fn configure(conn: &Connection, path: Option<&Path>) -> Result<()> {
    // 多连接（或另一个进程）争锁时让 SQLite 自己重试，而不是立刻抛 SQLITE_BUSY。
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|source| StorageError::open(path, source))?;

    // WAL：写不阻塞读。媒体同步会一次性写入几千条，同时界面可能正在浏览，
    // 回滚日志模式会让读者在这期间看到 SQLITE_BUSY。
    // journal_mode 会回读实际生效的模式：内存数据库只能是 "memory"，不算错误。
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(|source| StorageError::open(path, source))?;
    if !mode.eq_ignore_ascii_case("wal") {
        // 内存库只能是 "memory"（合法，不需要 WAL）；文件库降级说明介质可能不支持 WAL。
        if path.is_some() {
            tracing::warn!(journal_mode = %mode, "SQLite 未启用 WAL");
        } else {
            tracing::debug!(journal_mode = %mode, "内存库不使用 WAL");
        }
    }

    // NORMAL：WAL 下最常用的折中——掉电最多丢最后一次提交，不会损坏数据库；
    // FULL 每次提交都要 fsync，批量写入会慢一个数量级。
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|source| StorageError::open(path, source))?;

    // 外键默认是关的，必须每个连接显式打开，否则级联删除不生效。
    // 放在事务外：SQLite 在事务内会忽略这个 pragma。
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| StorageError::open(path, source))?;

    // 主动读一次 schema 版本：这是打开后第一次真正读文件，
    // 非 SQLite / 被截断的文件会在这里报 "file is not a database" / "malformed"，
    // 从而得到一个带路径的明确错误，而不是等到业务查询时才炸。
    read_user_version(conn).map_err(|source| StorageError::open(path, source))?;
    Ok(())
}

/// 前向迁移：`user_version` 落后就一路补到 [`SCHEMA_VERSION`]。
///
/// 幂等：已经是最新版本时直接返回，不做任何写入。整个迁移包在一个事务里，
/// 中途失败即回滚（SQLite 的 DDL 也是事务性的），`user_version` 也不会前进。
fn migrate(conn: &mut Connection, path: Option<&Path>) -> Result<()> {
    let from = read_user_version(conn).map_err(|source| StorageError::open(path, source))?;
    if from > SCHEMA_VERSION {
        return Err(StorageError::SchemaTooNew {
            found: from,
            supported: SCHEMA_VERSION,
        });
    }
    if from == SCHEMA_VERSION {
        return Ok(());
    }

    let to = SCHEMA_VERSION;
    let tx = conn
        .transaction()
        .map_err(|source| StorageError::Migrate { from, to, source })?;
    if from < 1 {
        tx.execute_batch(schema::MIGRATION_V1)
            .map_err(|source| StorageError::Migrate { from, to, source })?;
    }
    if from < 2 {
        tx.execute_batch(schema::MIGRATION_V2)
            .map_err(|source| StorageError::Migrate { from, to, source })?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|source| StorageError::Migrate { from, to, source })?;
    tx.commit()
        .map_err(|source| StorageError::Migrate { from, to, source })?;
    tracing::debug!(from, to, "已完成 SQLite schema 迁移");
    Ok(())
}

fn read_user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_one(None, "user_version")
}
