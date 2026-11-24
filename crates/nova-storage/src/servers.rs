// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 服务器记录的读写。

use rusqlite::{Row, params};

use nova_core::{AuthState, Language, ServerId, ServerInfo, ServerKind};

use crate::codec;
use crate::database::Database;
use crate::errors::Result;
use crate::schema;

/// 一条服务器记录：`ServerInfo` 加上本地维护的时间戳。
///
/// 时间戳由调用方提供（Unix 秒），便于测试与"离线播放不推进时间"这类场景。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredServer {
    /// 服务器信息（不含令牌；令牌在系统凭证存储里）。
    pub info: ServerInfo,
    /// 首次添加时间（Unix 秒）。
    pub created_at: i64,
    /// 最近一次成功联络时间（Unix 秒）。
    pub last_seen_at: i64,
}

impl Database {
    /// 写入服务器；已存在时更新元数据并刷新 `last_seen_at`，`created_at` 保持不变。
    pub fn upsert_server(&self, info: &ServerInfo, now: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            schema::SQL_UPSERT_SERVER,
            params![
                info.id.as_str(),
                codec::enum_to_text(&info.kind)?,
                info.name.as_str(),
                info.base_url.as_str(),
                info.version.as_deref(),
                info.user_id.as_deref(),
                codec::encode(&info.auth)?,
                info.metadata_language.as_str(),
                now,
            ],
        )?;
        Ok(())
    }

    /// 只刷新"上次见到"的时间；返回是否命中了记录。
    pub fn touch_server(&self, id: &ServerId, now: i64) -> Result<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(schema::SQL_TOUCH_SERVER, params![id.as_str(), now])?;
        Ok(changed > 0)
    }

    /// 列出全部服务器（按显示名排序）。
    pub fn servers(&self) -> Result<Vec<StoredServer>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_SERVERS)?;
        let mut rows = stmt.query([])?;
        let mut servers = Vec::new();
        while let Some(row) = rows.next()? {
            servers.push(read_server(row)?);
        }
        Ok(servers)
    }

    /// 按 id 取一台服务器。
    pub fn server(&self, id: &ServerId) -> Result<Option<StoredServer>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_SERVER)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(read_server(row)?)),
            None => Ok(None),
        }
    }

    /// 删除服务器；`items` / `sources` / `sync_state` 由外键 `ON DELETE CASCADE` 一起删。
    /// 返回是否真的删掉了记录。
    pub fn remove_server(&self, id: &ServerId) -> Result<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(schema::SQL_DELETE_SERVER, params![id.as_str()])?;
        Ok(changed > 0)
    }
}

fn read_server(row: &Row<'_>) -> Result<StoredServer> {
    let id: String = row.get(0)?;
    let key = format!("server={id}");
    let kind: ServerKind = codec::enum_from_text("servers", "kind", &row.get::<_, String>(1)?)?;
    let auth: AuthState = codec::decode("servers", &key, &row.get::<_, String>(6)?)?;
    Ok(StoredServer {
        info: ServerInfo {
            id: ServerId::new(id),
            kind,
            name: row.get(2)?,
            base_url: row.get(3)?,
            version: row.get(4)?,
            user_id: row.get(5)?,
            auth,
            metadata_language: Language::new(row.get::<_, String>(7)?),
        },
        created_at: row.get(8)?,
        last_seen_at: row.get(9)?,
    })
}
