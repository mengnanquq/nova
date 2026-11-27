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

use nova_core::{ItemId, MediaItem, MediaKind, ServerId};

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

    /// 列出某个父项的子项，按**加入时间倒序**（"最近添加"用）。
    pub fn children_recent(
        &self,
        server_id: &ServerId,
        parent_id: Option<&ItemId>,
    ) -> Result<Vec<MediaItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_CHILDREN_RECENT)?;
        let mut rows = stmt.query(params![server_id.as_str(), parent_id.map(ItemId::as_str)])?;
        read_items(&mut rows)
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

    /// 跨库按标题匹配，只保留 `kinds` 里的类型，最多 `limit` 条。
    ///
    /// 本地搜索用它：列表刷新把电影 / 剧集的父项写成所属媒体库之后，
    /// `children(None)`（`parent_id IS NULL`）几乎一条都拿不到，本地搜索就废了；
    /// 而把整库拉进内存再筛又会被几万条条目拖垮。标题匹配因此下推到 SQL：
    ///
    /// - `%` / `_` / `\` 按字面量处理（`like_pattern` 转义），搜 `%` 不会
    ///   把所有条目都匹配出来；
    /// - 大小写语义对齐 `views::matches`：needle 先 `to_lowercase()`，SQLite 的
    ///   `LIKE` 对 ASCII 大小写不敏感、对中文按字节相等。**已知差异**：非 ASCII
    ///   字母的大小写折叠只在 Rust 侧做，SQL 侧看不到（如标题 `AMÉLIE` 与小写
    ///   查询 `é`），这类极端组合可能漏一条——为了把全表读进内存换这个边缘
    ///   场景不值得；
    /// - 顺序是 `updated_at DESC`（最近加入的在前），与首页「最近添加」一致；
    /// - 类型过滤在 Rust 侧做（`MediaKind` 是 `nova-core` 的枚举），并且**先于**
    ///   `limit` 计数：一页里混着的季 / 集不会占掉电影 / 剧集的名额。
    pub fn search_items(
        &self,
        server_id: &ServerId,
        kinds: &[MediaKind],
        term: &str,
        limit: usize,
    ) -> Result<Vec<MediaItem>> {
        let (items, _) = self.search_scan(server_id, kinds, term, limit)?;
        Ok(items)
    }

    /// 跨库按标题匹配：只扫一遍，同时给出前 `limit` 条与**精确总数**（R6-10）。
    ///
    /// 超出 `limit` 之后只数标题、不再解码 payload：搜索页只展示前 60 条，但结果头
    /// 要说真实总数。以前 `search_items` + `search_item_count` 会各自跑一遍
    /// `LIKE '%词%'` 全表扫（宽泛词“的”在大库上每次按键多花一份扫描时间）。
    pub fn search_items_with_count(
        &self,
        server_id: &ServerId,
        kinds: &[MediaKind],
        term: &str,
        limit: usize,
    ) -> Result<(Vec<MediaItem>, usize)> {
        self.search_scan(server_id, kinds, term, limit)
    }

    /// 跨库按标题匹配的**精确总数**（不受 `limit` 截断，R6-10）。
    ///
    /// 与 [`Self::search_items`] 共用同一个扫描函数（`limit = 0` 时不解码任何
    /// payload），两者的匹配口径因此必然一致。
    pub fn search_item_count(
        &self,
        server_id: &ServerId,
        kinds: &[MediaKind],
        term: &str,
    ) -> Result<usize> {
        let (_, count) = self.search_scan(server_id, kinds, term, 0)?;
        Ok(count)
    }

    /// 搜索的单次遍历：前 `limit` 条解码成条目，其余只计数（不解码）。
    fn search_scan(
        &self,
        server_id: &ServerId,
        kinds: &[MediaKind],
        term: &str,
        limit: usize,
    ) -> Result<(Vec<MediaItem>, usize)> {
        let needle = term.trim().to_lowercase();
        if needle.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let wanted = kind_texts(kinds)?;
        let pattern = like_pattern(&needle);
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_SEARCH)?;
        let mut rows = stmt.query(params![server_id.as_str(), pattern])?;
        let mut out = Vec::new();
        let mut count = 0usize;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(1)?;
            if !wanted.is_empty() && !wanted.iter().any(|candidate| candidate == &kind) {
                continue;
            }
            // SQL 的 `LIKE` 只是预筛（ASCII 大小写不敏感）；最终语义以
            // `to_lowercase().contains` 为准（与首页 / 媒体库共用）。
            let title: String = row.get(2)?;
            if !title.to_lowercase().contains(&needle) {
                continue;
            }
            count += 1;
            if out.len() >= limit {
                // 超出展示上限：只要计数，不再解码 payload。
                continue;
            }
            let id: String = row.get(0)?;
            let payload: String = row.get(3)?;
            let item: MediaItem =
                codec::decode("items", codec::item_key(server_id.as_str(), &id), &payload)?;
            out.push(item);
        }
        Ok((out, count))
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

    /// 跨库的顶层条目，按**加入时间倒序**；只保留 `kinds` 里的类型，最多 `limit` 条。
    ///
    /// 首页「最近添加」用它：顶层条目（电影 / 剧集）的父项现在是所属媒体库，
    /// 按 `parent_id IS NULL` 挑只会得到空列表。季 / 集不参与（它们是播放层级，
    /// 只该出现在详情页）。
    pub fn browse_recent(
        &self,
        server_id: &ServerId,
        kinds: &[MediaKind],
        limit: usize,
    ) -> Result<Vec<MediaItem>> {
        let wanted = kind_texts(kinds)?;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_BROWSE_RECENT)?;
        let mut rows = stmt.query(params![server_id.as_str()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            if out.len() >= limit {
                break;
            }
            let id: String = row.get(0)?;
            let kind: String = row.get(1)?;
            if !wanted.is_empty() && !wanted.iter().any(|candidate| candidate == &kind) {
                continue;
            }
            let payload: String = row.get(2)?;
            out.push(codec::decode(
                "items",
                codec::item_key(server_id.as_str(), &id),
                &payload,
            )?);
        }
        Ok(out)
    }

    /// 某个服务器已缓存的条目数。
    pub fn item_count(&self, server_id: &ServerId) -> Result<u64> {
        let conn = self.conn()?;
        count(&conn, schema::SQL_COUNT_ITEMS, server_id)
    }

    /// 列出某个父项下的条目 id；`kinds` 为空表示不按类型过滤。
    ///
    /// 只读 `id` / `kind` 两列（不解析 payload）：刷新后判断"这个库里的哪些条目
    /// 服务端已经不给了"时，几百上千条也只是几次索引扫。
    pub fn child_ids(
        &self,
        server_id: &ServerId,
        parent: &ItemId,
        kinds: &[MediaKind],
    ) -> Result<Vec<ItemId>> {
        let wanted = kind_texts(kinds)?;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_CHILD_ID_KINDS)?;
        let mut rows = stmt.query(params![server_id.as_str(), parent.as_str()])?;
        read_ids(&mut rows, &wanted)
    }

    /// 列出"父项不可用、任何视图都摆不正确"的条目 id。
    ///
    /// 判定规则（`libraries` 是调用方已知的媒体库 id——库不在 `items` 表里，
    /// 不传进来就会把归属正常的条目全当成悬空）：
    ///
    /// - 电影 / 剧集：父项存在、不是本地条目、也不是某个媒体库 → 归属坏掉；
    /// - 季 / 集：父项不是本地条目（`NULL` 或悬空）→ 层级断了。这类行不可能被
    ///   `children(剧集)` 找到，却会漏进顶层列表（首页「最近添加」/ 离线「全部」
    ///   查的是 `parent_id IS NULL`）。
    ///
    /// 其它类型一律不动：本函数只服务于刷新后的过期清理。
    pub fn unreachable_item_ids(
        &self,
        server_id: &ServerId,
        libraries: &[ItemId],
    ) -> Result<Vec<ItemId>> {
        let libraries: std::collections::HashSet<&str> =
            libraries.iter().map(ItemId::as_str).collect();
        let movie = codec::enum_to_text(&MediaKind::Movie)?;
        let series = codec::enum_to_text(&MediaKind::Series)?;
        let season = codec::enum_to_text(&MediaKind::Season)?;
        let episode = codec::enum_to_text(&MediaKind::Episode)?;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(schema::SQL_SELECT_UNREACHABLE_ID_KINDS)?;
        let mut rows = stmt.query(params![server_id.as_str()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let parent: Option<String> = row.get(2)?;
            let unreachable = if kind == movie || kind == series {
                // 父项为 NULL 的电影 / 剧集是合法的顶层条目，不在此列。
                parent
                    .as_deref()
                    .is_some_and(|parent| !libraries.contains(parent))
            } else {
                kind == season || kind == episode
            };
            if unreachable {
                out.push(ItemId::new(id));
            }
        }
        Ok(out)
    }

    /// 展开一批条目的**全部后代**（剧集 → 季 → 集），返回"这批 + 后代"的 id。
    ///
    /// 只写连接级临时表、外加一次递归读，实测几万条也只要几十毫秒；真正的删除
    /// 交给 [`Database::delete_items`]，好让调用方按批释放数据库锁。
    pub fn item_tree_ids(&self, server_id: &ServerId, ids: &[ItemId]) -> Result<Vec<ItemId>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        conn.execute_batch(schema::SQL_CREATE_PRUNE_TABLES)?;
        conn.execute(schema::SQL_CLEAR_PRUNE_SEEDS, [])?;
        conn.execute(schema::SQL_CLEAR_PRUNE_TREE, [])?;
        {
            let mut seed = conn.prepare(schema::SQL_INSERT_PRUNE_SEED)?;
            for id in ids {
                seed.execute(params![id.as_str()])?;
            }
        }
        conn.execute(schema::SQL_EXPAND_PRUNE_TREE, params![server_id.as_str()])?;
        let tree = {
            let mut stmt = conn.prepare(schema::SQL_SELECT_PRUNE_TREE)?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(ItemId::new(row?));
            }
            out
        };
        conn.execute(schema::SQL_CLEAR_PRUNE_SEEDS, [])?;
        conn.execute(schema::SQL_CLEAR_PRUNE_TREE, [])?;
        Ok(tree)
    }

    /// 按主键删除一批条目（不展开后代），返回实际删除的行数。
    ///
    /// 一条语句一次事务：调用方应当**分批**调用（每批几百条），这样"删完一批就
    /// 放开数据库锁"，界面线程的读不会被一次大清理堵住。`sources` 由外键级联；
    /// `mylist` 没有外键，显式**不动**它（那是用户数据，不是缓存——条目回来后
    /// 收藏还在）。
    pub fn delete_items(&self, server_id: &ServerId, ids: &[ItemId]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut removed: u64 = 0;
        {
            let mut delete = tx.prepare(schema::SQL_DELETE_ITEM_BY_ID)?;
            for id in ids {
                let changed = delete.execute(params![server_id.as_str(), id.as_str()])?;
                removed = removed
                    .saturating_add(codec::as_u64(i64::try_from(changed).unwrap_or(i64::MAX)));
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// 删除一批条目及其全部后代（剧集 → 季 → 集），返回实际删除的行数。
    ///
    /// 便捷封装：[`Database::item_tree_ids`] + [`Database::delete_items`]，**一次**
    /// 持有数据库锁删完。需要保持界面响应（刷新后的清理）的调用方应当自己分批，
    /// 而不是用这个（见 `nova_app::refresh`）。
    pub fn delete_item_trees(&self, server_id: &ServerId, ids: &[ItemId]) -> Result<u64> {
        let tree = self.item_tree_ids(server_id, ids)?;
        self.delete_items(server_id, &tree)
    }
}

/// 把类型过滤条件编成 `kind` 列的文本值（`MediaKind` 以 serde 文本入列，
/// 见 [`crate::codec::enum_to_text`]）。
fn kind_texts(kinds: &[MediaKind]) -> Result<Vec<String>> {
    kinds.iter().map(codec::enum_to_text).collect()
}

/// 把搜索词包成 `LIKE` 模式，并转义通配符。
///
/// `%` / `_` 是 SQL 的通配符，`\` 是语句里声明的转义符：三个都得先转义，
/// 否则搜 `%` 会匹配全表、搜 `_` 会匹配任意单字符。
fn like_pattern(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for ch in needle.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// 把列表刷新 / CLI 同步拿到的条目压成"媒体库的直接子项"。
///
/// 列表刷新与 CLI `items` 共用同一条归属规则（不要各写一份）：
///
/// - 电影 / 剧集的父项**一律**写成库。列表刷新是库归属的权威来源（体检 R4-3 的反面）：
///   服务端对子文件夹里的条目会返回物理 `ParentId`，而 provider 只在响应缺字段时
///   回填库 id（见 `nova_provider::client` 的 `items()`），因此这类条目会带着文件夹
///   父项入库、在按 `ParentId=<库>` 查询的库页里消失。这里统一纠正。
/// - 季 / 集**丢弃**：它们不是库的直接子项，父项该是剧集 / 季（播放层级）。列表响应
///   缺 `ParentId`，硬写库归属会让它们在真正的层级里查不到、在顶层列表里凭空出现；
///   即使服务端无视 `IncludeItemTypes` 把它们混进来，这里也不会入库（详情页打开
///   剧集时用 `children()` 拉，父项是准确的）。
pub fn library_items(items: Vec<MediaItem>, library: &ItemId) -> Vec<MediaItem> {
    items
        .into_iter()
        .filter_map(|mut item| {
            if !matches!(item.kind, MediaKind::Movie | MediaKind::Series) {
                tracing::debug!(
                    item = item.id.as_str(),
                    kind = ?item.kind,
                    "媒体库列表里的季 / 集跳过（由详情页按需拉取）"
                );
                return None;
            }
            if item.parent_id.as_ref() != Some(library) {
                tracing::debug!(
                    item = item.id.as_str(),
                    fresh = ?item.parent_id,
                    library = library.as_str(),
                    "按列表刷新纠正库归属"
                );
                item.parent_id = Some(library.clone());
            }
            Some(item)
        })
        .collect()
}

/// 把 `SELECT id, kind` 的结果读成 id 列表，并按 `wanted` 过滤类型。
fn read_ids(rows: &mut rusqlite::Rows<'_>, wanted: &[String]) -> Result<Vec<ItemId>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let kind: String = row.get(1)?;
        if wanted.is_empty() || wanted.iter().any(|candidate| candidate == &kind) {
            out.push(ItemId::new(id));
        }
    }
    Ok(out)
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
