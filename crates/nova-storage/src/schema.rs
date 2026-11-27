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
pub(crate) const SCHEMA_VERSION: i64 = 4;

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

/// 跨库、按标题匹配（本地搜索在输入时的即时反馈与离线结果）。
///
/// 匹配在 SQL 侧做，因为库归属写对之后顶层行几乎不存在（`parent_id IS NULL`
/// 只有历史脏数据），不能再用 `children(None)` 拉一小撮再筛；而整库几万行也不
/// 能全读进内存。`LIKE` 的通配符由调用方转义（见
/// [`Database::search_items`](../items/struct.Database.html#method.search_items)），
/// 这里的 `ESCAPE '\\'` 只是告诉 SQLite 用反斜杠做转义符。
///
/// 同时下发 `title` 列：匹配超出 `limit` 之后只用它计数、不再解码 `payload`
/// （[`Database::search_items_with_count`]）。
///
/// 类型过滤仍在 Rust 侧（`MediaKind` 是 `nova-core` 的枚举），索引不需要额外
/// 加：`LIKE '%词%'` 前缀是通配符，任何索引都用不上，扫 `(server_id)` 这一段
/// 已经是最省的读法。
pub(crate) const SQL_SELECT_SEARCH: &str = "
SELECT id, kind, title, payload FROM items
WHERE server_id = ?1
  AND title LIKE ?2 ESCAPE '\\'
ORDER BY updated_at DESC, sort_title, id
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

/// 跨库的顶层条目，按**加入时间倒序**（首页「最近添加」）。
///
/// 不能再用 `parent_id IS NULL` 挑顶层条目：列表刷新把电影 / 剧集的父项写成
/// 所属媒体库（库归属的权威来源），顶层条目的父项基本都不是 NULL。类型过滤在
/// Rust 侧做（`MediaKind` 是 `nova-core` 的枚举），语句本身只负责顺序。
pub(crate) const SQL_SELECT_BROWSE_RECENT: &str = "
SELECT id, kind, payload FROM items
WHERE server_id = ?1
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

/// 某个父项下的条目 id 与类型（只读 id 列，不碰 payload）。
///
/// 用于刷新后清理过期条目："这个库里现在还有哪些电影 / 剧集"只需要 id 与类型，
/// 类型过滤放在调用方做（`MediaKind` 是 `nova-core` 的枚举，SQL 里没有它）。
pub(crate) const SQL_SELECT_CHILD_ID_KINDS: &str = "
SELECT id, kind FROM items WHERE server_id = ?1 AND parent_id = ?2
";

/// 父项已不可用的条目（归属坏掉、层级断掉，或父项已被删）。
///
/// 取"没有非空父项指向本地条目"的行，判定交给调用方（电影 / 剧集要再看父项
/// 是否是媒体库；季 / 集只要父项不是本地条目就算断了）——
/// 见 [`Database::unreachable_item_ids`](../items/struct.Database.html)。
///
/// 媒体库本身不在 `items` 表里（v3 的 `libraries` 表不参与这条查询），所以这里的子查询
/// 查不到库 id，调用方必须用当前库列表把它们排除，否则会把整个库的条目当成悬空。
/// `parent_id = ''`（空串）也算不可用：它既不是库也不是条目，任何视图都查不到。
pub(crate) const SQL_SELECT_UNREACHABLE_ID_KINDS: &str = "
SELECT id, kind, parent_id FROM items
WHERE server_id = ?1
  AND (parent_id IS NULL
       OR parent_id NOT IN (SELECT id FROM items WHERE server_id = ?1))
";

/// 清理批次用的**连接级**临时表（其它连接看不见）。
///
/// - `prune_seeds`：这一批要删的条目 id（由调用方给出）；
/// - `prune_tree`：把它们在 `items` 里的后代一起展开后的 id 集合。
///
/// 为什么不用"一条递归 `DELETE`"：实测那样删除 3.3 万行要十分钟以上（`IN (子查询)`
/// 被当成相关性求值，退化成二次复杂度），而"先展开进临时表、再按 id 逐条删"是
/// 0.4 秒。
pub(crate) const SQL_CREATE_PRUNE_TABLES: &str = "
CREATE TEMP TABLE IF NOT EXISTS prune_seeds (id TEXT PRIMARY KEY);
CREATE TEMP TABLE IF NOT EXISTS prune_tree (id TEXT PRIMARY KEY);
";

/// 清空上一批种子与展开结果。
pub(crate) const SQL_CLEAR_PRUNE_SEEDS: &str = "DELETE FROM prune_seeds";

/// 清空上一批展开结果。
pub(crate) const SQL_CLEAR_PRUNE_TREE: &str = "DELETE FROM prune_tree";

/// 放入一条种子 id（重复放入无副作用）。
pub(crate) const SQL_INSERT_PRUNE_SEED: &str = "INSERT OR IGNORE INTO prune_seeds (id) VALUES (?1)";

/// 把种子及其全部后代（剧集 → 季 → 集）展开进 `prune_tree`。
///
/// `items.parent_id` 没有自引用外键（理由见 [`MIGRATION_V1`] 的注释），级联只能
/// 在这里显式递归。
///
/// **`CROSS JOIN` 是性能要求，不是写法偏好**：用普通的 `JOIN ... ON` 时 SQLite
/// 会把 `items` 当外层循环、只用 `(server_id)` 前缀扫索引，每行递归都把整表扫一遍
/// ——实测 3.3 万条要 9 分钟。`CROSS JOIN` 固定连接顺序（种子表在外层），内层走
/// `(server_id, parent_id)` 精确探针，同样的数据 50 毫秒。
pub(crate) const SQL_EXPAND_PRUNE_TREE: &str = "
WITH RECURSIVE subtree(id) AS (
    SELECT id FROM items WHERE server_id = ?1 AND id IN (SELECT id FROM prune_seeds)
    UNION
    SELECT child.id FROM subtree CROSS JOIN items child
        ON child.parent_id = subtree.id
    WHERE child.server_id = ?1
)
INSERT OR IGNORE INTO prune_tree (id) SELECT id FROM subtree
";

/// 展开后的待删 id（逐条按主键删除才是线性的）。
pub(crate) const SQL_SELECT_PRUNE_TREE: &str = "SELECT id FROM prune_tree";

/// 按主键删一条条目；`sources` 由外键级联。
pub(crate) const SQL_DELETE_ITEM_BY_ID: &str = "
DELETE FROM items WHERE server_id = ?1 AND id = ?2
";

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

/// v3：持久化服务端媒体库列表。
///
/// 以前媒体库只存在 `nova_app::Local` 的内存里（`set_libraries`），进程一退就没了：
/// 离线启动时标签只能靠"缓存里有条目就合成一个「全部」"兜底，真实库名（电影 /
/// 动画电影 / 剧集）看不到。列表本身是服务端结构的本地副本，条目才是缓存；但
/// 它决定顶部标签与库页的 `ParentId`，没有它离线浏览只能退化。
///
/// `kind` 存 `MediaKind` 的 serde 文本（与 `items.kind` 同一编码，见
/// `crate::codec::enum_to_text`）；`collection_type` 保留服务端原值（`movies` /
/// `tvshows`），`item_count` 服务端没给就是 NULL。
pub(crate) const MIGRATION_V3: &str = "
CREATE TABLE libraries (
    server_id       TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    id              TEXT NOT NULL,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    collection_type TEXT,
    item_count      INTEGER,
    PRIMARY KEY (server_id, id)
);
";

/// v4：媒体库保存**服务端顺序**。
///
/// v3 按 `name` 排序读回，而服务端的 `/Users/{id}/Views` 是另一种顺序：离线启动
/// （读回持久化列表）与在线刷新后的顶栏标签顺序不一致，用户刚形成的肌肉记忆
/// 会被一次后台刷新打乱（真机 R6-8）。`position` 是服务端返回顺序（0 起）。
///
/// 旧库用 `rowid` 回填：v3 的 `replace_libraries` 是“先删后插”，插入顺序就是
/// 上一次刷新拿到的服务端顺序，`rowid` 单调递增地记录了它（v3 的 INSERT 带
/// `ON CONFLICT DO UPDATE`，但前置 DELETE 已经清空同一服务器的行，不会走 UPDATE）。
/// 因此升级后旧库也能立即保持上一次刷新后的顺序，不用等下一次刷新才对齐。
///
/// 之后的顺序以显式 `position` 列为准：新写入由 `replace_libraries` 按切片序号
/// 赋值（见 `libraries.rs`），不再依赖 `rowid`（插入语句变化 / 分批写入时它不可靠）。
pub(crate) const MIGRATION_V4: &str = "
ALTER TABLE libraries ADD COLUMN position INTEGER NOT NULL DEFAULT 0;

UPDATE libraries SET position = (
    SELECT COUNT(*) FROM libraries AS earlier
    WHERE earlier.server_id = libraries.server_id
      AND earlier.rowid < libraries.rowid
);
";

// ---------------------------------------------------------------------------
// libraries
// ---------------------------------------------------------------------------

/// 覆盖一台服务器的媒体库列表（先删后插，调用方负责放进事务）。
pub(crate) const SQL_DELETE_LIBRARIES: &str = "DELETE FROM libraries WHERE server_id = ?1";

/// 写入一个媒体库（`position` = 服务端返回顺序，0 起）。
pub(crate) const SQL_INSERT_LIBRARY: &str = "
INSERT INTO libraries (server_id, id, name, kind, collection_type, item_count, position)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT(server_id, id) DO UPDATE SET
    name            = excluded.name,
    kind            = excluded.kind,
    collection_type = excluded.collection_type,
    item_count      = excluded.item_count,
    position        = excluded.position
";

/// 列出一台服务器的媒体库（按服务端返回顺序）。
pub(crate) const SQL_SELECT_LIBRARIES: &str = "
SELECT id, name, kind, collection_type, item_count FROM libraries
WHERE server_id = ?1
ORDER BY position, name COLLATE NOCASE, id
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
            SQL_SELECT_SEARCH,
            SQL_SELECT_IN_PROGRESS,
            SQL_COUNT_ITEMS,
            SQL_SELECT_CHILD_ID_KINDS,
            SQL_SELECT_UNREACHABLE_ID_KINDS,
            SQL_CREATE_PRUNE_TABLES,
            SQL_CLEAR_PRUNE_SEEDS,
            SQL_CLEAR_PRUNE_TREE,
            SQL_INSERT_PRUNE_SEED,
            SQL_EXPAND_PRUNE_TREE,
            SQL_SELECT_PRUNE_TREE,
            SQL_DELETE_ITEM_BY_ID,
            SQL_UPSERT_SOURCE,
            SQL_SELECT_SOURCES,
            SQL_COUNT_SOURCES,
            SQL_UPSERT_PREFERENCES,
            SQL_SELECT_PREFERENCES,
            SQL_DELETE_LIBRARIES,
            SQL_INSERT_LIBRARY,
            SQL_SELECT_LIBRARIES,
            SQL_UPSERT_SYNC_STATE,
            SQL_SELECT_SYNC_STATE,
            SQL_UPSERT_IMAGE,
            SQL_TOUCH_IMAGE,
            SQL_SELECT_IMAGE,
            SQL_SUM_IMAGE_BYTES,
            SQL_SELECT_IMAGES_BY_LRU,
            SQL_DELETE_IMAGE,
        ] {
            // `LIKE ... ESCAPE '\'` 里的单引号是**语法**（转义符字面量），不是值；
            // 守卫的本意是"别把标题 / URL / 令牌拼进 SQL"，这里先把它剔掉再检查。
            let sql = sql.replace("ESCAPE '\\'", "");
            assert!(!sql.contains('\''), "SQL 中出现了字符串字面量：{sql}");
            assert!(!sql.contains("format!"), "SQL 中出现了 format!：{sql}");
        }
    }
}
