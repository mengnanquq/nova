// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 图片缓存索引。
//!
//! 只记"哪张图存在哪里、多大、什么时候用过"；图片本体在文件系统上，按内容哈希命名。
//! 本层**不删文件**：淘汰只返回被淘汰记录的相对路径，由调用方删掉对应文件后
//! 再关心失败重试——数据库已经不需要这条记录了，文件删不掉最多多占一点磁盘。

use rusqlite::{OptionalExtension, params};

use crate::codec;
use crate::database::Database;
use crate::errors::Result;
use crate::schema;

/// 一条图片缓存记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedImage {
    /// 去掉查询参数后 URL 的哈希（缓存文件名）。
    pub url_hash: String,
    /// 相对缓存根目录的路径。
    pub rel_path: String,
    /// 文件字节数。
    pub bytes: u64,
    /// 最近一次使用时间（Unix 秒）。
    pub last_used: i64,
}

impl Database {
    /// 记录一张已经落盘的图片，并把 `last_used` 置为当前时间。
    ///
    /// 同一 `url_hash` 重复记录只更新路径与大小（图片重新下载的情况）。
    pub fn record_image(&self, url_hash: &str, rel_path: &str, bytes: u64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            schema::SQL_UPSERT_IMAGE,
            params![url_hash, rel_path, codec::bytes(bytes), codec::unix_now()],
        )?;
        Ok(())
    }

    /// 刷新一条记录的使用时间（LRU 用）；返回是否命中了记录。
    pub fn touch_image(&self, url_hash: &str, now: i64) -> Result<bool> {
        let conn = self.conn()?;
        let changed = conn.execute(schema::SQL_TOUCH_IMAGE, params![url_hash, now])?;
        Ok(changed > 0)
    }

    /// 读一条图片缓存记录。
    pub fn cached_image(&self, url_hash: &str) -> Result<Option<CachedImage>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_IMAGE)?;
        let image = stmt
            .query_row(params![url_hash], |row| {
                Ok(CachedImage {
                    url_hash: row.get(0)?,
                    rel_path: row.get(1)?,
                    bytes: codec::as_u64(row.get::<_, i64>(2)?),
                    last_used: row.get(3)?,
                })
            })
            .optional()?;
        Ok(image)
    }

    /// 当前缓存占用的总字节数。
    pub fn image_cache_bytes(&self) -> Result<u64> {
        let conn = self.conn()?;
        let total: i64 = conn.query_row(schema::SQL_SUM_IMAGE_BYTES, [], |row| row.get(0))?;
        Ok(codec::as_u64(total))
    }

    /// 按 LRU 淘汰到 `max_bytes` 以内，返回被淘汰记录的 `rel_path`（淘汰顺序）。
    ///
    /// 调用方负责删文件。整个淘汰过程在一个事务里：要么预算达成，
    /// 要么（例如中途报错）一条都不删，不会出现"索引删了但预算没算对"的状态。
    pub fn evict_images_to_budget(&self, max_bytes: u64) -> Result<Vec<String>> {
        let budget = codec::bytes(max_bytes);
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut evicted = Vec::new();
        {
            let mut select = tx.prepare(schema::SQL_SELECT_IMAGES_BY_LRU)?;
            let mut rows = select.query([])?;
            let mut total: i64 = 0;
            let mut candidates: Vec<(String, String, i64)> = Vec::new();
            while let Some(row) = rows.next()? {
                let bytes: i64 = row.get(2)?;
                total = total.saturating_add(bytes);
                candidates.push((row.get(0)?, row.get(1)?, bytes));
            }

            if total <= budget {
                return Ok(evicted);
            }
            let mut remove = tx.prepare(schema::SQL_DELETE_IMAGE)?;
            for (url_hash, rel_path, bytes) in candidates {
                if total <= budget {
                    break;
                }
                remove.execute(params![url_hash])?;
                total -= bytes;
                evicted.push(rel_path);
            }
        }
        tx.commit()?;
        Ok(evicted)
    }
}
