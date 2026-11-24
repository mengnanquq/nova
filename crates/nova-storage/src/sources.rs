// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 可播放资源（文件版本及其流）的读写。
//!
//! `sources` 通过外键挂在 `items` 上：写资源前必须已经写过对应条目，
//! 条目被删时资源跟着级联删除，不会留下指向不存在条目的孤儿行。

use rusqlite::params;

use nova_core::{ItemId, MediaSource, ServerId};

use crate::codec;
use crate::database::Database;
use crate::errors::Result;
use crate::items::count;
use crate::schema;

impl Database {
    /// 批量写入某个服务器的可播放资源，单事务、幂等。
    pub fn upsert_sources(&self, server_id: &ServerId, sources: &[MediaSource]) -> Result<()> {
        if sources.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(schema::SQL_UPSERT_SOURCE)?;
            for source in sources {
                stmt.execute(params![
                    server_id.as_str(),
                    source.item_id.as_str(),
                    source.id.as_str(),
                    codec::encode(source)?,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 列出一条条目的全部资源（按资源 id 排序）。
    pub fn sources(&self, server_id: &ServerId, item_id: &ItemId) -> Result<Vec<MediaSource>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_SOURCES)?;
        let mut rows = stmt.query(params![server_id.as_str(), item_id.as_str()])?;
        let mut sources = Vec::new();
        while let Some(row) = rows.next()? {
            let server: String = row.get(0)?;
            let item: String = row.get(1)?;
            let id: String = row.get(2)?;
            let payload: String = row.get(3)?;
            sources.push(codec::decode(
                "sources",
                codec::source_key(&server, &item, &id),
                &payload,
            )?);
        }
        Ok(sources)
    }

    /// 某个服务器已缓存的资源数。
    pub fn source_count(&self, server_id: &ServerId) -> Result<u64> {
        let conn = self.conn()?;
        count(&conn, schema::SQL_COUNT_SOURCES, server_id)
    }
}
