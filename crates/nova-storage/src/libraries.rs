// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 服务端媒体库列表的持久化（schema v3，v4 起保存服务端顺序）。
//!
//! 媒体库本身不是缓存条目（`items` 表里没有库行），但顶部标签与库页的
//! `ParentId` 都靠它：只放在内存里的话，进程一退、离线一启动，真实库名就消失了，
//! 标签只能退化成一个合成的「全部」。
//!
//! 这里只做"存 / 读"，不掺业务判断。与 `nova-provider::Library` 的转换放在
//! `nova-app`（本 crate 不依赖 provider，见 crate 文档的依赖方向）。

use rusqlite::params;

use nova_core::{ItemId, MediaKind, ServerId};

use crate::codec;
use crate::database::Database;
use crate::errors::Result;
use crate::schema;

/// 持久化的媒体库记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredLibrary {
    /// 库标识（服务端作用域）。
    pub id: ItemId,
    /// 显示名。
    pub name: String,
    /// 库里装的是什么（电影 / 剧集）。
    pub kind: MediaKind,
    /// 服务端原始 `CollectionType`（`movies` / `tvshows`）。
    pub collection_type: Option<String>,
    /// 服务端给的条目数；没给就是 `None`。
    pub item_count: Option<u32>,
}

impl Database {
    /// 读一台服务器持久化的媒体库列表（按**服务端返回顺序**，见 schema v4）。
    pub fn libraries(&self, server_id: &ServerId) -> Result<Vec<StoredLibrary>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_LIBRARIES)?;
        let mut rows = stmt.query(params![server_id.as_str()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let kind: String = row.get(2)?;
            out.push(StoredLibrary {
                id: ItemId::new(id),
                name: row.get(1)?,
                kind: codec::enum_from_text("libraries", "kind", &kind)?,
                collection_type: row.get(3)?,
                item_count: row
                    .get::<_, Option<i64>>(4)?
                    .and_then(|count| u32::try_from(count).ok()),
            });
        }
        Ok(out)
    }

    /// 覆盖一台服务器的媒体库列表（一个事务：先删旧行再写新行）。
    ///
    /// 切片顺序就是服务端 `/Views` 的顺序：这里把它写成 `position` 列，
    /// 读回时保持同一顺序（真机 R6-8：以前按显示名读回，后台刷新后顶栏
    /// 标签会重新排列）。
    ///
    /// 空切片也是合法调用：新会话还不知道服务端有哪些库时用它清空
    /// （`Local::adopt_server`）。
    pub fn replace_libraries(
        &self,
        server_id: &ServerId,
        libraries: &[StoredLibrary],
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(schema::SQL_DELETE_LIBRARIES, params![server_id.as_str()])?;
        {
            let mut insert = tx.prepare(schema::SQL_INSERT_LIBRARY)?;
            for (position, library) in libraries.iter().enumerate() {
                insert.execute(params![
                    server_id.as_str(),
                    library.id.as_str(),
                    library.name.as_str(),
                    codec::enum_to_text(&library.kind)?,
                    library.collection_type.as_deref(),
                    library.item_count.map(i64::from),
                    i64::try_from(position).unwrap_or(i64::MAX),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 一台服务器持久化了几个媒体库。
    pub fn library_count(&self, server_id: &ServerId) -> Result<u64> {
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM libraries WHERE server_id = ?1",
            params![server_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(codec::as_u64(count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Database, ServerId) {
        let db = Database::open_in_memory().expect("内存库");
        let server = nova_core::ServerInfo {
            id: ServerId::new("s1"),
            kind: nova_core::ServerKind::Jellyfin,
            name: "测试".to_owned(),
            base_url: "http://127.0.0.1:8096".to_owned(),
            version: None,
            user_id: None,
            auth: nova_core::AuthState::Unauthorized,
            metadata_language: nova_core::Language::undetermined(),
        };
        db.upsert_server(&server, 1).expect("写服务器");
        (db, server.id)
    }

    fn library(id: &str, name: &str, kind: MediaKind) -> StoredLibrary {
        let collection_type = match kind {
            MediaKind::Movie => "movies",
            MediaKind::Series => "tvshows",
            MediaKind::Season | MediaKind::Episode => "",
        };
        StoredLibrary {
            id: ItemId::new(id),
            name: name.to_owned(),
            kind,
            collection_type: Some(collection_type.to_owned()),
            item_count: Some(7),
        }
    }

    #[test]
    fn libraries_roundtrip_and_replace_drops_stale_rows() {
        let (db, server) = db();
        assert!(db.libraries(&server).expect("读空列表").is_empty());

        db.replace_libraries(
            &server,
            &[
                library("lib-b", "剧集", MediaKind::Series),
                library("lib-a", "电影", MediaKind::Movie),
            ],
        )
        .expect("写媒体库");
        let stored = db.libraries(&server).expect("读媒体库");
        assert_eq!(stored.len(), 2);
        let movies = stored
            .iter()
            .find(|library| library.id.as_str() == "lib-a")
            .expect("电影库还在");
        assert_eq!(movies.name, "电影");
        assert_eq!(movies.kind, MediaKind::Movie);
        assert_eq!(movies.collection_type.as_deref(), Some("movies"));
        assert_eq!(movies.item_count, Some(7));
        assert!(
            stored
                .iter()
                .any(|library| library.kind == MediaKind::Series),
            "剧集库也读回来了"
        );
        assert_eq!(db.library_count(&server).expect("计数"), 2);

        // 覆盖写：服务端删掉的库不能留在标签里。
        db.replace_libraries(&server, &[library("lib-a", "电影", MediaKind::Movie)])
            .expect("覆盖写");
        let stored = db.libraries(&server).expect("读媒体库");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id.as_str(), "lib-a");

        // 空切片 = 清空（新会话还没拿到库列表时）。
        db.replace_libraries(&server, &[]).expect("清空");
        assert!(db.libraries(&server).expect("读媒体库").is_empty());
    }

    /// 读回顺序 == 写入（服务端）顺序，而不是显示名顺序（R6-8）。
    ///
    /// 用反字母序的中文 / ASCII 混排：按 `name` 排会得到另一组顺序。
    #[test]
    fn libraries_read_back_in_server_order() {
        let (db, server) = db();
        let server_order = [
            library("lib-z", "儿童节目", MediaKind::Movie),
            library("lib-a", "动画电影", MediaKind::Movie),
            library("lib-m", "Zebra", MediaKind::Series),
            library("lib-b", "Aardvark", MediaKind::Series),
        ];
        db.replace_libraries(&server, &server_order)
            .expect("写媒体库");

        let ids: Vec<String> = db
            .libraries(&server)
            .expect("读媒体库")
            .iter()
            .map(|library| library.id.as_str().to_owned())
            .collect();
        let expected: Vec<String> = server_order
            .iter()
            .map(|library| library.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, expected, "排序必须跟随服务端顺序");

        // 服务端换了顺序（同一批库）：覆盖写之后读回也是新顺序。
        let mut reversed = server_order.to_vec();
        reversed.reverse();
        db.replace_libraries(&server, &reversed)
            .expect("按新顺序覆盖写");
        let ids: Vec<String> = db
            .libraries(&server)
            .expect("读媒体库")
            .iter()
            .map(|library| library.id.as_str().to_owned())
            .collect();
        let expected: Vec<String> = reversed
            .iter()
            .map(|library| library.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, expected);
    }
}
