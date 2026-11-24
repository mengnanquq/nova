// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 增量同步游标的读写。
//!
//! 一个服务器的每个媒体库各有一条记录：上次同步时间 + 服务端给的游标。
//! 游标只是不透明字符串，语义由 provider 决定。

use rusqlite::{OptionalExtension, params};

use nova_core::ServerId;

use crate::database::Database;
use crate::errors::Result;
use crate::schema;

/// 一个媒体库的同步状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncState {
    /// 所属服务器。
    pub server_id: ServerId,
    /// 媒体库标识（服务端作用域）。
    pub library_id: String,
    /// 最近一次成功同步的时间（Unix 秒）。
    pub last_sync_at: i64,
    /// 增量游标；`None` 表示下次需要全量同步。
    pub cursor: Option<String>,
}

impl Database {
    /// 写入（或覆盖）一个媒体库的同步状态。
    pub fn set_sync_state(&self, state: &SyncState) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            schema::SQL_UPSERT_SYNC_STATE,
            params![
                state.server_id.as_str(),
                state.library_id.as_str(),
                state.last_sync_at,
                state.cursor.as_deref(),
            ],
        )?;
        Ok(())
    }

    /// 读一个媒体库的同步状态。
    pub fn sync_state(&self, server_id: &ServerId, library_id: &str) -> Result<Option<SyncState>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_SYNC_STATE)?;
        let state = stmt
            .query_row(params![server_id.as_str(), library_id], |row| {
                Ok(SyncState {
                    server_id: ServerId::new(row.get::<_, String>(0)?),
                    library_id: row.get(1)?,
                    last_sync_at: row.get(2)?,
                    cursor: row.get(3)?,
                })
            })
            .optional()?;
        Ok(state)
    }
}
