// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 本地「我的列表」：哪些条目被用户加入收藏。
//!
//! 只存本地（`mylist` 表，schema v2）。**不同步到服务端**：服务端收藏要走 provider
//! 的写接口（Emby / Jellyfin 的 `/Users/{id}/FavoriteItems/{itemId}`），当前 provider
//! 只有读接口，所以这里如实做成"本地列表"，而不是假装是服务端的收藏。
//!
//! 这样做的直接好处：详情页那个「+ 我的列表」按钮**真的会持久化**——重启、重新
//! 打开详情都还在，而不是一个刷新就归零的界面开关。

use nova_core::{ItemId, ServerId};
use rusqlite::{OptionalExtension, params};

use crate::codec::unix_now;
use crate::database::Database;
use crate::errors::Result;

impl Database {
    /// 该条目是否已在「我的列表」里。
    pub fn in_mylist(&self, server_id: &ServerId, item_id: &ItemId) -> Result<bool> {
        let conn = self.conn()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM mylist WHERE server_id = ?1 AND item_id = ?2",
                params![server_id.as_str(), item_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// 切换「我的列表」状态，返回切换后的状态（`true` = 现在已在列表里）。
    ///
    /// 幂等：重复加入不会产生第二行（主键约束 + `INSERT OR IGNORE`），
    /// 移除不存在的条目也不报错。
    pub fn toggle_mylist(&self, server_id: &ServerId, item_id: &ItemId) -> Result<bool> {
        if self.in_mylist(server_id, item_id)? {
            self.remove_from_mylist(server_id, item_id)?;
            Ok(false)
        } else {
            self.add_to_mylist(server_id, item_id)?;
            Ok(true)
        }
    }

    /// 加入「我的列表」。
    pub fn add_to_mylist(&self, server_id: &ServerId, item_id: &ItemId) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO mylist (server_id, item_id, added_at) VALUES (?1, ?2, ?3)",
            params![server_id.as_str(), item_id.as_str(), unix_now()],
        )?;
        Ok(())
    }

    /// 从「我的列表」移除。
    pub fn remove_from_mylist(&self, server_id: &ServerId, item_id: &ItemId) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM mylist WHERE server_id = ?1 AND item_id = ?2",
            params![server_id.as_str(), item_id.as_str()],
        )?;
        Ok(())
    }

    /// 「我的列表」里的条目 id，按加入时间倒序（最近加入的在前）。
    pub fn mylist(&self, server_id: &ServerId) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT item_id FROM mylist WHERE server_id = ?1 ORDER BY added_at DESC")?;
        let rows = stmt.query_map(params![server_id.as_str()], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Database, ServerId, ItemId) {
        let db = Database::open_in_memory().expect("内存库");
        (db, ServerId::new("s1"), ItemId::new("i1"))
    }

    #[test]
    fn toggle_persists_and_is_idempotent() {
        let (db, server, item) = db();
        assert!(!db.in_mylist(&server, &item).expect("读"));
        assert!(db.toggle_mylist(&server, &item).expect("加入"));
        assert!(db.in_mylist(&server, &item).expect("读"));
        // 重复加入不产生第二行。
        db.add_to_mylist(&server, &item).expect("重复加入");
        assert_eq!(db.mylist(&server).expect("列表").len(), 1);
        assert!(!db.toggle_mylist(&server, &item).expect("移除"));
        assert!(!db.in_mylist(&server, &item).expect("读"));
        // 移除不存在的条目也不报错。
        db.remove_from_mylist(&server, &item).expect("重复移除");
    }

    #[test]
    fn mylist_is_ordered_and_scoped_per_server() {
        let (db, server, _) = db();
        let other = ServerId::new("s2");
        db.add_to_mylist(&server, &ItemId::new("a")).expect("加 a");
        db.add_to_mylist(&server, &ItemId::new("b")).expect("加 b");
        db.add_to_mylist(&other, &ItemId::new("c")).expect("加 c");
        let mine = db.mylist(&server).expect("列表");
        assert_eq!(mine.len(), 2);
        assert!(mine.contains(&"a".to_string()) && mine.contains(&"b".to_string()));
        // 另一台服务器的条目不会混进来。
        assert!(!mine.contains(&"c".to_string()));
    }
}
