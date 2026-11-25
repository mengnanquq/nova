// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! schema 定义：全部 DDL 与 DML 语句集中在本模块。
//!
//! 约定（全 crate 遵守）：
//!
//! - 所有值都走占位符绑定（`?1`、`?2` …），**从不**把值拼进 SQL 字符串；
//! - 表名、列名只在本模块出现，其他模块只引用这里的语句常量；
//! - 本模块的语句都是静态字面量，不存在运行期拼装。

/// 当前 schema 版本，写入 `PRAGMA user_version`。
pub(crate) const SCHEMA_VERSION: i64 = 2;

/// `preferences` 表里 [`nova_core::UserPreferences`] 的键名。
pub(crate) const PREFERENCES_KEY: &str = "user_preferences";

/// v1 迁移：建表与索引。
///
/// 设计取舍：**归一化少量可查/可排序的列 + `payload` 存完整 `nova_core` 结构的 JSON**。
///
/// - 查询（按父项列子项、按标题排序、"继续观看"过滤）只碰归一化列，能走索引；
/// - 读取永远以 `payload` 为准，所以 `nova-core` 以后给 `MediaItem` / `MediaSource`
///   加字段时，**不需要**写数据库迁移——旧记录缺的字段由 serde 默认值兜底；
/// - 反过来，归一化列只服务于 SQL，不参与重建实体，因此不必和 payload 逐字段同步。
///
/// 外键：`items` / `sync_state` 挂 `servers`，`sources` 挂 `items`，全部
/// `ON DELETE CASCADE`。级联删除交给 SQLite 而不是手写多表事务，
/// 理由是一处定义、任何删除路径都生效（包括以后直接 `DELETE FROM servers` 的运维手段），
/// 代价只是每个连接必须记得开 `PRAGMA foreign_keys=ON`（见 [`crate::Database`]）。
///
/// `items.parent_id` 故意**不**做自引用外键：增量同步可能先拿到子项再拿到父项，
/// 而删一个季也不应该连带删掉它的集——集的去留由服务端同步决定。
pub(crate) const MIGRATION_V1: &str = "
CREATE TABLE servers (
    id                TEXT PRIMARY KEY NOT NULL,
    kind              TEXT    NOT NULL,
    name              TEXT    NOT NULL,
    base_url          TEXT    NOT NULL,
    version           TEXT,
    user_id           TEXT,
    auth_state        TEXT    NOT NULL,
    metadata_language TEXT    NOT NULL,
    created_at        INTEGER NOT NULL,
    last_seen_at      INTEGER NOT NULL
);

CREATE TABLE items (
    server_id    TEXT    NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    id           TEXT    NOT NULL,
    kind         TEXT    NOT NULL,
    title        TEXT    NOT NULL,
    sort_title   TEXT    NOT NULL,
    year         INTEGER,
    runtime_ms   INTEGER,
    parent_id    TEXT,
    index_number INTEGER,
    played       INTEGER NOT NULL,
    resume_ms    INTEGER NOT NULL,
    payload      TEXT    NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (server_id, id)
);

CREATE TABLE sources (
    server_id TEXT NOT NULL,
    item_id   TEXT NOT NULL,
    id        TEXT NOT NULL,
    payload   TEXT NOT NULL,
    PRIMARY KEY (server_id, item_id, id),
    FOREIGN KEY (server_id, item_id) REFERENCES items(server_id, id) ON DELETE CASCADE
);

CREATE TABLE preferences (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE sync_state (
    server_id    TEXT    NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    library_id   TEXT    NOT NULL,
    last_sync_at INTEGER NOT NULL,
    cursor       TEXT,
    PRIMARY KEY (server_id, library_id)
);

CREATE TABLE image_cache (
    url_hash  TEXT PRIMARY KEY NOT NULL,
    rel_path  TEXT    NOT NULL,
    bytes     INTEGER NOT NULL,
    last_used INTEGER NOT NULL
);

-- 按父项列子项（含 index_number 排序）。
CREATE INDEX idx_items_parent ON items(server_id, parent_id, index_number);
-- 按标题排序浏览。
CREATE INDEX idx_items_sort_title ON items(server_id, sort_title);
-- 继续观看时按最近更新倒序。
CREATE INDEX idx_items_updated_at ON items(server_id, updated_at);
-- 图片缓存的 LRU 淘汰。
CREATE INDEX idx_image_cache_last_used ON image_cache(last_used);
";

// ---------------------------------------------------------------------------
// servers
// ---------------------------------------------------------------------------

/// 写入服务器；`created_at` 只在首次插入时保留。
pub(crate) const SQL_UPSERT_SERVER: &str = "
INSERT INTO servers (
    id, kind, name, base_url, version, user_id,
    auth_state, metadata_language, created_at, last_seen_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
ON CONFLICT(id) DO UPDATE SET
    kind              = excluded.kind,
    name              = excluded.name,
    base_url          = excluded.base_url,
    version           = excluded.version,
    user_id           = excluded.user_id,
    auth_state        = excluded.auth_state,
    metadata_language = excluded.metadata_language,
    last_seen_at      = excluded.last_seen_at
";

/// 只刷新"上次见到"时间。
pub(crate) const SQL_TOUCH_SERVER: &str = "UPDATE servers SET last_seen_at = ?2 WHERE id = ?1";

/// 服务器列表（按显示名排序）。
pub(crate) const SQL_SELECT_SERVERS: &str = "
SELECT id, kind, name, base_url, version, user_id,
       auth_state, metadata_language, created_at, last_seen_at
FROM servers
ORDER BY name COLLATE NOCASE, id
";

/// 按 id 取一台服务器。
pub(crate) const SQL_SELECT_SERVER: &str = "
SELECT id, kind, name, base_url, version, user_id,
       auth_state, metadata_language, created_at, last_seen_at
FROM servers
WHERE id = ?1
";

/// 删除服务器（`items` / `sources` / `sync_state` 由外键级联）。
pub(crate) const SQL_DELETE_SERVER: &str = "DELETE FROM servers WHERE id = ?1";

// ---------------------------------------------------------------------------
// items
// ---------------------------------------------------------------------------

/// 写入条目；重复写同一条只更新内容与 `updated_at`，不产生新行。
pub(crate) const SQL_UPSERT_ITEM: &str = "
INSERT INTO items (
    server_id, id, kind, title, sort_title, year, runtime_ms,
    parent_id, index_number, played, resume_ms, payload, updated_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
ON CONFLICT(server_id, id) DO UPDATE SET
    kind         = excluded.kind,
    title        = excluded.title,
    sort_title   = excluded.sort_title,
    year         = excluded.year,
    runtime_ms   = excluded.runtime_ms,
    parent_id    = excluded.parent_id,
    index_number = excluded.index_number,
    played       = excluded.played,
    resume_ms    = excluded.resume_ms,
    payload      = excluded.payload,
    updated_at   = excluded.updated_at
";

/// 读一条条目的 payload。
pub(crate) const SQL_SELECT_ITEM: &str = "
SELECT server_id, id, payload FROM items
WHERE server_id = ?1 AND id = ?2
";

/// 按父项列子项：`index_number` 升序，缺失的排在最后。
pub(crate) const SQL_SELECT_CHILDREN: &str = "
SELECT server_id, id, payload FROM items
WHERE server_id = ?1
  AND ((?2 IS NULL AND parent_id IS NULL) OR parent_id = ?2)
ORDER BY index_number IS NULL, index_number, sort_title
";

/// 同 [`SQL_SELECT_CHILDREN`]，但按**加入时间倒序**（"最近添加"排序）。
///
/// `updated_at` 是同步时写入的时间戳（有 `idx_items_updated_at` 索引）。
/// 之前"最近添加"直接沿用 `SQL_SELECT_CHILDREN` 的顺序，而那一条是按
/// `index_number, sort_title` 排的——电影没有集号，于是变成**字母序**，
/// 与标签写的"最近添加"根本不是一回事。
pub(crate) const SQL_SELECT_CHILDREN_RECENT: &str = "
SELECT server_id, id, payload FROM items
WHERE server_id = ?1
  AND ((?2 IS NULL AND parent_id IS NULL) OR parent_id = ?2)
ORDER BY updated_at DESC, sort_title, id
";

/// "继续观看"：过滤条件在 SQL 侧完成，等价于 `MediaItem::is_in_progress(min_seconds)`。
///
/// `resume_ms * 100 < runtime_ms * 95` 是 `progress_fraction() < 0.95` 的整数形式
/// （`progress_fraction` 会把比例 clamp 到 1.0，所以 `resume > runtime` 时同样被排除）。
pub(crate) const SQL_SELECT_IN_PROGRESS: &str = "
SELECT server_id, id, payload FROM items
WHERE server_id = ?1
  AND played = 0
  AND runtime_ms > 0
  AND resume_ms >= ?2
  AND resume_ms * 100 < runtime_ms * 95
ORDER BY updated_at DESC, sort_title
";

/// 某个服务器的条目总数。
pub(crate) const SQL_COUNT_ITEMS: &str = "SELECT COUNT(*) FROM items WHERE server_id = ?1";

// ---------------------------------------------------------------------------
// sources
// ---------------------------------------------------------------------------

/// 写入可播放资源；外键要求所属条目已存在。
pub(crate) const SQL_UPSERT_SOURCE: &str = "
INSERT INTO sources (server_id, item_id, id, payload)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(server_id, item_id, id) DO UPDATE SET payload = excluded.payload
";

/// 列出一条条目的全部资源。
pub(crate) const SQL_SELECT_SOURCES: &str = "
SELECT server_id, item_id, id, payload FROM sources
WHERE server_id = ?1 AND item_id = ?2
ORDER BY id
";

/// 某个服务器的资源总数。
pub(crate) const SQL_COUNT_SOURCES: &str = "SELECT COUNT(*) FROM sources WHERE server_id = ?1";

// ---------------------------------------------------------------------------
// preferences
// ---------------------------------------------------------------------------

/// 写入用户偏好（单键）。
pub(crate) const SQL_UPSERT_PREFERENCES: &str = "
INSERT INTO preferences (key, value) VALUES (?1, ?2)
ON CONFLICT(key) DO UPDATE SET value = excluded.value
";

/// 读用户偏好。
pub(crate) const SQL_SELECT_PREFERENCES: &str = "SELECT value FROM preferences WHERE key = ?1";

// ---------------------------------------------------------------------------
// sync_state
// ---------------------------------------------------------------------------

/// 写入媒体库同步游标。
pub(crate) const SQL_UPSERT_SYNC_STATE: &str = "
INSERT INTO sync_state (server_id, library_id, last_sync_at, cursor)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(server_id, library_id) DO UPDATE SET
    last_sync_at = excluded.last_sync_at,
    cursor       = excluded.cursor
";

/// 读一个媒体库的同步游标。
pub(crate) const SQL_SELECT_SYNC_STATE: &str = "
SELECT server_id, library_id, last_sync_at, cursor FROM sync_state
WHERE server_id = ?1 AND library_id = ?2
";

// ---------------------------------------------------------------------------
// image_cache
// ---------------------------------------------------------------------------

/// 记录一张已落盘的图片。
pub(crate) const SQL_UPSERT_IMAGE: &str = "
INSERT INTO image_cache (url_hash, rel_path, bytes, last_used)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(url_hash) DO UPDATE SET
    rel_path  = excluded.rel_path,
    bytes     = excluded.bytes,
    last_used = excluded.last_used
";

/// 刷新使用时间（LRU 用）。
pub(crate) const SQL_TOUCH_IMAGE: &str =
    "UPDATE image_cache SET last_used = ?2 WHERE url_hash = ?1";

/// 读一条图片缓存记录。
pub(crate) const SQL_SELECT_IMAGE: &str = "
SELECT url_hash, rel_path, bytes, last_used FROM image_cache WHERE url_hash = ?1
";

/// 缓存占用总字节数。
pub(crate) const SQL_SUM_IMAGE_BYTES: &str = "SELECT COALESCE(SUM(bytes), 0) FROM image_cache";

/// 按 LRU 顺序（最久未使用在前）列出全部记录。
pub(crate) const SQL_SELECT_IMAGES_BY_LRU: &str = "
SELECT url_hash, rel_path, bytes, last_used FROM image_cache
ORDER BY last_used, rowid
";

/// 淘汰一条记录。
pub(crate) const SQL_DELETE_IMAGE: &str = "DELETE FROM image_cache WHERE url_hash = ?1";

/// v2：本地「我的列表」。
///
/// 只存"这台服务器上的哪个条目被用户加入列表"以及加入时间，不同步到服务端
/// （服务端收藏要走 provider 的写接口，尚未实现）。放在本地的好处是它**真的
/// 会持久化**，而不是详情页里一个刷新就归零的开关。
pub(crate) const MIGRATION_V2: &str = "
CREATE TABLE mylist (
    server_id TEXT    NOT NULL,
    item_id   TEXT    NOT NULL,
    added_at  INTEGER NOT NULL,
    PRIMARY KEY (server_id, item_id)
);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_have_no_value_interpolation() {
        // 守卫用的小测试：所有语句都只应包含 `?N` 占位符，不应出现引号包裹的字面量值。
        // 这不是完整的 SQL 解析，只是防止以后有人图省事把 URL / 标题拼进语句。
        for sql in [
            SQL_UPSERT_SERVER,
            SQL_TOUCH_SERVER,
            SQL_SELECT_SERVERS,
            SQL_SELECT_SERVER,
            SQL_DELETE_SERVER,
            SQL_UPSERT_ITEM,
            SQL_SELECT_ITEM,
            SQL_SELECT_CHILDREN,
            SQL_SELECT_IN_PROGRESS,
            SQL_COUNT_ITEMS,
            SQL_UPSERT_SOURCE,
            SQL_SELECT_SOURCES,
            SQL_COUNT_SOURCES,
            SQL_UPSERT_PREFERENCES,
            SQL_SELECT_PREFERENCES,
            SQL_UPSERT_SYNC_STATE,
            SQL_SELECT_SYNC_STATE,
            SQL_UPSERT_IMAGE,
            SQL_TOUCH_IMAGE,
            SQL_SELECT_IMAGE,
            SQL_SUM_IMAGE_BYTES,
            SQL_SELECT_IMAGES_BY_LRU,
            SQL_DELETE_IMAGE,
        ] {
            assert!(!sql.contains('\''), "SQL 中出现了字符串字面量：{sql}");
            assert!(!sql.contains("format!"), "SQL 中出现了 format!：{sql}");
        }
    }
}
