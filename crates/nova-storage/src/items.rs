// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体条目的读写。
//!
//! 写入时双写：`payload` 列存完整 [`MediaItem`] 的 JSON（读取的唯一来源），
//! 另外把少量查询/排序需要的字段摊平成列（`kind`、`title`、`sort_title`、`year`、
//! `runtime_ms`、`parent_id`、`index_number`、`played`、`resume_ms`、`updated_at`）。
//!
//! 这样做的收益是双向的：
//!
//! - `nova-core` 给 [`MediaItem`] 加字段时**不需要迁移**数据库，旧 payload 由 serde
//!   默认值兜底，新 payload 的额外字段被旧代码忽略；
//! - 过滤与排序留在 SQL 侧（可以走索引），不用把整张表读进内存再筛。

use rusqlite::{Connection, OptionalExtension, params};

use nova_core::{ItemId, MediaItem, ServerId};

use crate::codec;
use crate::database::Database;
use crate::errors::Result;
use crate::schema;

impl Database {
    /// 批量写入条目，全部在一个事务里。
    ///
    /// 幂等：以 `(server_id, id)` 为主键 upsert，重复写同一条不会产生第二行，
    /// 只会刷新内容与 `updated_at`。空切片是合法的空操作。
    pub fn upsert_items(
        &self,
        server_id: &ServerId,
        items: &[MediaItem],
        updated_at: i64,
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(schema::SQL_UPSERT_ITEM)?;
            for item in items {
                stmt.execute(params![
                    server_id.as_str(),
                    item.id.as_str(),
                    codec::enum_to_text(&item.kind)?,
                    item.title.as_str(),
                    item.sort_key(),
                    item.year.map(i64::from),
                    codec::millis(item.runtime),
                    item.parent_id.as_ref().map(ItemId::as_str),
                    item.index_number.map(i64::from),
                    if item.user_data.played { 1_i64 } else { 0_i64 },
                    codec::millis(Some(item.user_data.resume_position)),
                    codec::encode(item)?,
                    updated_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 读一条条目；不存在返回 `None`。
    pub fn item(&self, server_id: &ServerId, id: &ItemId) -> Result<Option<MediaItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_ITEM)?;
        let row = stmt
            .query_row(params![server_id.as_str(), id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?;
        match row {
            Some((server_id, item_id, payload)) => Ok(Some(codec::decode(
                "items",
                codec::item_key(&server_id, &item_id),
                &payload,
            )?)),
            None => Ok(None),
        }
    }

    /// 列出某个父项的子项，按 `index_number` 升序（缺集号的排最后）。
    ///
    /// `parent_id == None` 时列出该服务器的顶层条目（电影、剧集）。
    pub fn children(
        &self,
        server_id: &ServerId,
        parent_id: Option<&ItemId>,
    ) -> Result<Vec<MediaItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_CHILDREN)?;
        let mut rows = stmt.query(params![server_id.as_str(), parent_id.map(ItemId::as_str)])?;
        read_items(&mut rows)
    }

    /// "继续观看"：未看完、有进度、且不接近片尾。
    ///
    /// 语义等同于 [`MediaItem::is_in_progress`]，但过滤在 SQL 里完成——
    /// 媒体库可能有几万条条目，不能全表拉出来再筛。
    pub fn continue_watching(
        &self,
        server_id: &ServerId,
        min_seconds: u64,
    ) -> Result<Vec<MediaItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_IN_PROGRESS)?;
        let min_ms = codec::seconds(min_seconds).saturating_mul(1000);
        let mut rows = stmt.query(params![server_id.as_str(), min_ms])?;
        read_items(&mut rows)
    }

    /// 某个服务器已缓存的条目数。
    pub fn item_count(&self, server_id: &ServerId) -> Result<u64> {
        let conn = self.conn()?;
        count(&conn, schema::SQL_COUNT_ITEMS, server_id)
    }
}

pub(crate) fn count(conn: &Connection, sql: &str, server_id: &ServerId) -> Result<u64> {
    let value: i64 = conn.query_row(sql, params![server_id.as_str()], |row| row.get(0))?;
    Ok(codec::as_u64(value))
}

/// 把 `SELECT server_id, id, payload` 的游标读成条目列表。
pub(crate) fn read_items(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<MediaItem>> {
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let server_id: String = row.get(0)?;
        let id: String = row.get(1)?;
        let payload: String = row.get(2)?;
        items.push(codec::decode(
            "items",
            codec::item_key(&server_id, &id),
            &payload,
        )?);
    }
    Ok(items)
}
