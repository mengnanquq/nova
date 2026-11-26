// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 后台刷新：`libraries()` → 每个库**分页拉全** → `upsert_items` → 清理过期条目。
//!
//! 刷新是"锦上添花"：失败只改变 [`Status`]，不抛给界面、不清缓存。界面在刷新前
//! 就已经用 SQLite 渲染过一次了（离线优先），刷新完成后由调用方重建视图。
//!
//! 三条边界值得写清楚：
//!
//! - **列表刷新是库归属的权威来源**：每个库按 `ParentId=<库>` 递归分页，拿回来的
//!   电影 / 剧集一律把父项写成这个库（[`nova_storage::library_items`]，与 CLI
//!   `items` 的缓存路径共用同一份规则），历史坏归属顺手被纠正。详情 / 搜索路径
//!   不参与这件事（见 `Local::cache_item_and_sources` 的 `keep_library_membership`）。
//! - **只有完整拉完才删**：一个库翻完最后一页后，才删除"归属这个库、本次列表里
//!   没有"的电影 / 剧集（服务端删掉的片子不该继续显示）；所有库都拉完后，再清
//!   一遍"父项不可用"的历史坏数据（见 [`prune_unreachable`]）。中途失败、写库
//!   失败、401 都不删——宁可多留一屏旧数据，也不能把一次网络抖动当成"服务端删库"。
//! - **不阻塞界面**：每页写完就落地（刷新期间界面读到的是越来越全的缓存），
//!   页与页之间 `yield_now` 让出调度；写库本身仍在 `spawn_blocking` 里，
//!   因为 `nova-storage` 是同步阻塞的（内部 `Mutex<Connection>`），在 tokio 工作
//!   线程上直接写会拖慢图片下载与其它请求。

use std::collections::HashSet;
use std::sync::Arc;

use nova_core::{ItemId, MediaItem, MediaKind};
use nova_provider::{ItemQuery, Library, MediaProvider, ProviderError};
use nova_storage::Database;
use nova_storage::library_items;

use crate::error::AppError;
use crate::local::Local;
use crate::status::Status;

/// 分页拉取的**页大小**。
///
/// 这不是"每个库取多少条"：刷新会一直翻页到服务端报告的 `Page.total` 为止
/// （体检 R5-1——以前每库只取第一页 200 条，800 多条的库在媒体库页只能看到
/// 碰巧落在第一页的那些）。200 与服务端默认页大小一致，单次响应不至于太大。
pub const ITEMS_PER_PAGE: u32 = 200;

/// 清理缓存时每个事务删多少行。
///
/// `Database` 内部是 `Mutex<Connection>`：一次删几万行会把界面线程的所有读
/// （库页 `children`、本地搜索、图片键查询）堵在 `Mutex::lock` 上，表现就是整窗
/// 卡死、调试通道无回包。分批后单次持锁只有"删 200 行"的量级（实测数毫秒），
/// 批与批之间释放锁并 `yield_now`，读请求最多等一批。
const DELETE_BATCH: usize = 200;

/// 媒体库的**直接子项**类型：只有它们在库里代表"一部片子"。
///
/// 季 / 集是播放层级（父项是剧集 / 季），不参与"库归属"的纠正与清理。
const LIBRARY_ITEM_KINDS: [MediaKind; 2] = [MediaKind::Movie, MediaKind::Series];

/// 一个库本次刷新的结果（日志与测试用它，界面不需要）。
#[derive(Clone, Debug)]
pub struct LibrarySync {
    /// 媒体库标识。
    pub library: ItemId,
    /// 显示名（日志用）。
    pub name: String,
    /// 服务端报告的条目总数（最后一页的 `Page.total`）。
    pub total: u32,
    /// 本次实际写入缓存的条目数。
    pub cached: u64,
    /// 因为服务端列表里已经没有而删掉的本地条目数（含递归删掉的季 / 集）。
    pub removed: u64,
    /// 是否完整拉完（只有完整才允许清理）。
    pub complete: bool,
}

/// 一次刷新的结果。
#[derive(Clone, Debug)]
pub struct RefreshReport {
    /// 刷新后的连接状态。
    pub status: Status,
    /// 服务端返回的媒体库（已写进 [`Local`]）。
    pub libraries: Vec<nova_provider::Library>,
    /// 刷新后本地缓存的条目总数。
    pub item_count: u64,
    /// 每个库的拉取结果（顺序与 `libraries` 一致）。
    pub synced: Vec<LibrarySync>,
}

/// 执行一次刷新。
///
/// 前提是 `status` 已连接；否则原样返回（离线启动不应该发任何请求）。
pub async fn refresh(local: Arc<Local>, status: Status) -> RefreshReport {
    if !status.is_connected() {
        return RefreshReport {
            status,
            libraries: local.libraries(),
            item_count: local_item_count(&local).await,
            synced: Vec::new(),
        };
    }
    let Some(provider) = local.provider() else {
        return RefreshReport {
            status: Status::RefreshFailed {
                message: "没有可用的服务端会话".to_owned(),
            },
            libraries: Vec::new(),
            item_count: local_item_count(&local).await,
            synced: Vec::new(),
        };
    };
    let provider = provider.as_ref() as &dyn MediaProvider;

    let libraries = match provider.libraries().await {
        Ok(libraries) => libraries,
        Err(error) => {
            return RefreshReport {
                status: failure_status(&local, &error),
                libraries: local.libraries(),
                item_count: local_item_count(&local).await,
                synced: Vec::new(),
            };
        }
    };
    local.set_libraries(libraries.clone());
    if let Err(error) = local.persist_info(provider) {
        tracing::warn!(%error, "写回服务器信息失败");
    }

    let mut synced = Vec::with_capacity(libraries.len());
    for library in &libraries {
        match sync_library(&local, provider, library).await {
            Ok(result) => synced.push(result),
            Err(error) => {
                // `sync_library` 只把 401 往上抛（其余错误在库内消化，保留已拉到的页）。
                // 令牌失效后后面的库只会继续 401，直接停下；已写入的缓存不清。
                if matches!(error, ProviderError::Unauthorized) {
                    local.mark_expired();
                }
                tracing::warn!(library = library.name, %error, "拉取媒体库条目中断");
                return RefreshReport {
                    status: local.status(),
                    libraries,
                    item_count: local_item_count(&local).await,
                    synced,
                };
            }
        }
    }

    // 所有库都完整拉完之后，才清理"父项不可用"的死缓存（坏归属的电影 / 剧集 +
    // 层级断掉的季 / 集）：只要有一个库没拉全，就无从判断那些悬空条目是不是它
    // 还在用的。
    if !libraries.is_empty() && synced.iter().all(|result| result.complete) {
        prune_unreachable(&local, &libraries).await;
    }

    let info = provider.server_info();
    RefreshReport {
        status: Status::Connected {
            server: info.name,
            base_url: info.base_url,
        },
        libraries,
        item_count: local_item_count(&local).await,
        synced,
    }
}

/// 把一个库分页拉到底（每页 [`ITEMS_PER_PAGE`] 条），并在完整拉完后清理过期条目。
///
/// 只有 401 会向上返回错误；其它失败（网络、5xx、写库）都当作"这个库没拉全"：
/// 已经写入的页保留，缓存一条不删。
async fn sync_library(
    local: &Arc<Local>,
    provider: &dyn MediaProvider,
    library: &Library,
) -> Result<LibrarySync, ProviderError> {
    let started = std::time::Instant::now();
    let mut start: u32 = 0;
    let mut total: u32 = 0;
    let mut cached: u64 = 0;
    let mut kept: HashSet<ItemId> = HashSet::new();
    let mut complete = false;

    loop {
        // 只问这个库自己那一层：电影库要电影、剧集库要剧集。
        //
        // 不加 `with_kinds` 的话 `Recursive=true` 会把季与集一起带回来，而列表响应
        // 不带 `ParentId`（Jellyfin / Emby 实测如此），provider 只给电影/剧集回填
        // 库归属，季与集就成了 `parent_id = NULL` 的孤儿——它们会挤进"顶层条目"
        // 列表（首页「最近添加」、离线「全部」标签），一个剧集库能带出几万行。
        // 季/集是播放层级：打开剧集详情时由 `children()` 按需拉取（父项是剧集 / 季）。
        let query = ItemQuery::in_parent(library.id.clone())
            .with_kinds([library.kind])
            .paged(start, ITEMS_PER_PAGE);
        let page = match provider.items(query).await {
            Ok(page) => page,
            Err(ProviderError::Unauthorized) => return Err(ProviderError::Unauthorized),
            Err(error) => {
                // 单个库失败不影响其它库：界面继续显示已缓存的部分（也不清理）。
                tracing::warn!(library = library.name, start, %error, "拉取媒体库条目失败");
                break;
            }
        };
        total = page.total;
        let count = u32::try_from(page.items.len()).unwrap_or(u32::MAX);
        if count == 0 {
            // 服务端声称还有更多却给了空页：当作"没拉全"（不允许清理，也别死循环）。
            complete = start >= total;
            break;
        }

        let items = library_items(page.items, &library.id);
        for item in &items {
            if matches!(item.kind, MediaKind::Movie | MediaKind::Series) {
                kept.insert(item.id.clone());
            }
        }
        cached = cached.saturating_add(u64::from(count));
        if let Err(error) = cache_items(local, items).await {
            // 写库失败同样按"没拉全"处理：这一页的 id 虽然进了 `kept`，但缓存里可能
            // 缺行，继续清理的风险大于收益。
            tracing::warn!(library = library.name, start, %error, "条目写入本地缓存失败");
            break;
        }

        let next = start.saturating_add(count);
        if next >= total {
            complete = true;
            break;
        }
        if next <= start {
            // 服务端没按 `StartIndex` 前进（异常响应）：停下，别死循环。
            tracing::warn!(
                library = library.name,
                start,
                next,
                "分页没有前进，停止翻页"
            );
            break;
        }
        start = next;
        // 大库十几页：每页之间让出一次调度，界面重建与图片下载不排在写库后面。
        tokio::task::yield_now().await;
    }

    let removed = if complete {
        prune_library(local, library, &kept).await
    } else {
        0
    };

    let result = LibrarySync {
        library: library.id.clone(),
        name: library.name.clone(),
        total,
        cached,
        removed,
        complete,
    };
    tracing::info!(
        library = result.name,
        total = result.total,
        cached = result.cached,
        removed = result.removed,
        complete = result.complete,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "媒体库刷新完成"
    );
    Ok(result)
}

/// 库完整拉取后，删除"归属这个库、但服务端列表里已经没有"的电影 / 剧集。
///
/// 剧集被删时它的季与集一起删（[`Database::delete_item_trees`] 递归），否则那些
/// 集会作为孤儿行继续出现在「继续观看」里。`sources` 由外键级联；
/// `mylist`（用户收藏）没有外键，明确不动——条目回来后收藏还在。
///
/// 只在完整拉取后调用：`stale` 是"本地有这个库的归属、服务端这次没给"的差集，
/// 拿一个只拉了半截的列表去算，会把整屏片子当成"已删除"。
async fn prune_library(local: &Arc<Local>, library: &Library, kept: &HashSet<ItemId>) -> u64 {
    let Some(server) = local.server() else {
        return 0;
    };
    let db = Arc::clone(&local.db);
    let server_id = server.info.id;
    let parent = library.id.clone();
    let kept = kept.clone();
    let task =
        tokio::task::spawn_blocking(move || -> Result<Vec<ItemId>, nova_storage::StorageError> {
            let existing = db.child_ids(&server_id, &parent, &LIBRARY_ITEM_KINDS)?;
            Ok(existing
                .into_iter()
                .filter(|id| !kept.contains(id))
                .collect())
        });
    let stale = match task.await {
        Ok(Ok(stale)) => stale,
        Ok(Err(error)) => {
            tracing::warn!(%error, "统计过期缓存条目失败");
            return 0;
        }
        Err(error) => {
            tracing::warn!(%error, "统计过期缓存任务失败");
            return 0;
        }
    };
    if stale.is_empty() {
        return 0;
    }
    tracing::info!(
        library = library.id.as_str(),
        count = stale.len(),
        "清理服务端已删除的缓存条目"
    );
    delete_in_batches(local, &stale, "清理服务端已删除的缓存条目").await
}

/// 完整刷新所有库之后，清理"父项不可用"的历史坏数据。
///
/// `libraries` 来自本次 `libraries()` 的完整响应：库 id 是合法的父项，但库本身不在
/// `items` 表里，所以"父项不在本地条目里"并不等于悬空。
///
/// 两类行，判定规则见 [`Database::unreachable_item_ids`]：
///
/// 1. **电影 / 剧集**：父项既不是库也不是本地条目。这是 R4-3 之前的坏数据
///    （详情 / 搜索把物理父项写进缓存）或更早的父项删除痕迹。完整拉取后仍悬空的，
///    说明服务端任何电影 / 剧集库的完整列表里都没有它——要么已删，要么属于这台
///    服务器视图里没有的库。两种都不该继续占缓存，且它们在任何页面都显示不出来
///    （库页按 `ParentId=<库>` 查、顶层查 `parent_id IS NULL`），只虚增条目数。
/// 2. **季 / 集**：父项为空或已不存在。它们不可能被 `children(剧集)` 找到，却会
///    漏进顶层列表（首页「最近添加」/ 离线「全部」）冒充卡片。旧版每库只拉 200
///    条的列表路径写下的就是这种行（列表响应不带 `ParentId`）；现在刷新只拉库的
///    直接子项，这类行不会再产生。删掉不丢服务端数据：打开剧集详情时
///    `children()` 会把季 / 集连同进度一起拉回来。
///
/// `items` 本来就是可重建缓存：条目重新出现在列表里就会重新入库。只在**所有**库都
/// 完整拉完时调用（调用点保证）：少一个库就分不清"已删除"和"在没拉到的那库里"。
async fn prune_unreachable(local: &Arc<Local>, libraries: &[Library]) -> u64 {
    let Some(server) = local.server() else {
        return 0;
    };
    let db = Arc::clone(&local.db);
    let server_id = server.info.id;
    // 库不在 `items` 表里，必须作为"合法父项"传下去，否则会把整个库的条目当成悬空。
    let known: Vec<ItemId> = libraries.iter().map(|library| library.id.clone()).collect();
    let task =
        tokio::task::spawn_blocking(move || -> Result<Vec<ItemId>, nova_storage::StorageError> {
            db.unreachable_item_ids(&server_id, &known)
        });
    let unreachable = match task.await {
        Ok(Ok(ids)) => ids,
        Ok(Err(error)) => {
            tracing::warn!(%error, "统计不可达缓存条目失败");
            return 0;
        }
        Err(error) => {
            tracing::warn!(%error, "统计不可达缓存任务失败");
            return 0;
        }
    };
    if unreachable.is_empty() {
        return 0;
    }
    tracing::info!(count = unreachable.len(), "清理父项不可用的缓存条目");
    delete_in_batches(local, &unreachable, "清理父项不可用的缓存条目").await
}

/// 分批删除一批条目及其全部后代，**每批一个短事务**。
///
/// 展开后代（递归 CTE，几十毫秒）只写连接级临时表；真正删除按 [`DELETE_BATCH`]
/// 分批，批与批之间 [`tokio::task::yield_now`] 并重新取锁——`Database` 是
/// `Mutex<Connection>`，一次删几万行会让界面线程的读全部堵在锁上。
///
/// 返回实际删除的行数；某一批失败就停下并返回已删的数量（缓存可重建，
/// 下一次刷新会接着清理）。
async fn delete_in_batches(local: &Arc<Local>, ids: &[ItemId], what: &'static str) -> u64 {
    let Some(server) = local.server() else {
        return 0;
    };
    let db = Arc::clone(&local.db);
    let server_id = server.info.id;
    let tree = {
        let db = Arc::clone(&db);
        let server_id = server_id.clone();
        let ids = ids.to_vec();
        match tokio::task::spawn_blocking(move || db.item_tree_ids(&server_id, &ids)).await {
            Ok(Ok(tree)) => tree,
            Ok(Err(error)) => {
                tracing::warn!(%error, what, "展开待删条目失败");
                return 0;
            }
            Err(error) => {
                tracing::warn!(%error, what, "展开待删条目任务失败");
                return 0;
            }
        }
    };

    let started = std::time::Instant::now();
    let mut removed: u64 = 0;
    let mut batches = 0_u64;
    for chunk in tree.chunks(DELETE_BATCH) {
        let db = Arc::clone(&db);
        let server_id = server_id.clone();
        let batch = chunk.to_vec();
        // 每批一次独立的 `spawn_blocking`：函数返回时锁已释放。
        match tokio::task::spawn_blocking(move || db.delete_items(&server_id, &batch)).await {
            Ok(Ok(count)) => removed = removed.saturating_add(count),
            Ok(Err(error)) => {
                tracing::warn!(%error, what, removed, "删除缓存条目失败，停止本批清理");
                break;
            }
            Err(error) => {
                tracing::warn!(%error, what, removed, "删除缓存条目任务失败，停止本批清理");
                break;
            }
        }
        batches = batches.saturating_add(1);
        tokio::task::yield_now().await;
    }
    tracing::info!(
        removed,
        batches,
        elapsed_ms = started.elapsed().as_millis() as u64,
        what,
        "缓存清理完成"
    );
    removed
}

fn failure_status(local: &Local, error: &ProviderError) -> Status {
    if matches!(error, ProviderError::Unauthorized) {
        local.mark_expired();
        return local.status();
    }
    Status::RefreshFailed {
        message: error.to_string(),
    }
}

/// 写一页条目（调用方已用 [`nova_storage::library_items`] 纠正过库归属）。
async fn cache_items(local: &Arc<Local>, items: Vec<MediaItem>) -> Result<(), AppError> {
    if items.is_empty() {
        return Ok(());
    }
    let local = Arc::clone(local);
    tokio::task::spawn_blocking(move || local.cache_items(&items))
        .await
        .map_err(|error| AppError::Config(format!("条目缓存任务失败：{error}")))?
}

/// 本地缓存条目数（放到阻塞池里查，失败按 0 处理）。
pub async fn local_item_count(local: &Arc<Local>) -> u64 {
    let Some(server) = local.server() else {
        return 0;
    };
    let db = Arc::clone(&local.db);
    let server_id = server.info.id.clone();
    tokio::task::spawn_blocking(move || Database::item_count(&db, &server_id).unwrap_or(0))
        .await
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use nova_core::{
        AuthState, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo, ServerKind,
        UserData,
    };
    use nova_storage::StoredServer;

    use super::*;
    use crate::env::{CredentialMode, Env};

    fn server_info() -> ServerInfo {
        ServerInfo {
            id: ServerId::new("srv-a"),
            kind: ServerKind::Jellyfin,
            name: "测试".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            version: None,
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: Language::undetermined(),
        }
    }

    #[tokio::test]
    async fn offline_refresh_is_a_no_op() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开状态目录"));
        let report = refresh(Arc::clone(&local), Status::NoServer).await;
        assert!(matches!(report.status, Status::NoServer));
        assert_eq!(report.item_count, 0);
        assert!(report.libraries.is_empty());
    }

    #[tokio::test]
    async fn expired_refresh_marks_the_server_and_keeps_cache() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server_info(), 1).expect("写服务器");
            let item = MediaItem {
                id: ItemId::new("m1"),
                kind: MediaKind::Movie,
                title: "缓存电影".to_owned(),
                sort_title: "缓存电影".to_owned(),
                original_title: None,
                year: None,
                overview: None,
                genres: Vec::new(),
                runtime: None,
                parent_id: None,
                index_number: None,
                index_number_end: None,
                child_count: None,
                images: nova_core::ImageSet::default(),
                user_data: UserData::default(),
            };
            db.upsert_items(&ServerId::new("srv-a"), &[item], 1)
                .expect("写条目");
        }
        let local = Arc::new(Local::open(&env, None).expect("打开状态目录"));
        let report = refresh(
            Arc::clone(&local),
            Status::Expired {
                url: "http://127.0.0.1:1".to_owned(),
            },
        )
        .await;
        assert!(matches!(report.status, Status::Expired { .. }));
        assert!(
            report.status.banner_data().is_some(),
            "过期状态刷新时原样返回（离线不发请求），横幅必须留着等用户重新登录"
        );
        assert_eq!(report.item_count, 1, "离线也要能数到缓存里的条目");
        assert_eq!(local.home(None).expect("构建首页").recently_added.len(), 1);
        let _ = StoredServer {
            info: server_info(),
            created_at: 1,
            last_seen_at: 1,
        };
    }
}

/// 分页拉全与"服务端已删除"清理的回归测试（wiremock）。
///
/// 修复前的现象（体检 R5-1）：每个库只发一次 `paged(0, 200)`，服务端 800+ 条的
/// 库在媒体库页只能看到碰巧落在第一页的条目；历史缓存里"归属被写坏"的条目也
/// 永远不会被纠正或清掉。这里的断言全部落在**客户端实际发出的请求**与
/// **本地缓存的内容**上，而不是内部计数。
#[cfg(test)]
mod paging_tests {
    use nova_core::{
        AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, MediaSource, ServerId,
        ServerInfo, ServerKind, SourceId, UserData,
    };
    use nova_provider::{Flavor, Provider, ProviderConfig, RetryPolicy};
    use nova_secrets::{CredentialStore, MemoryStore, SecretString};
    use nova_storage::Database;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::env::{CredentialMode, Env};

    const SERVER_ID: &str = "srv-a";
    const USER_ID: &str = "user-paging";
    const TOKEN: &str = "token-paging";
    const LIBRARY_ID: &str = "lib-movies";
    /// 每页条数（服务端接口的分页粒度，不是"每库只取这么多"）。
    const PAGE: u32 = 200;
    /// 服务端大库的真实条目数：故意不是 200 的整数倍。
    const TOTAL: u32 = 450;

    /// 与 `tests` 模块里的同名辅助独立：那里是私有的，不能跨模块复用。
    fn server_info() -> ServerInfo {
        ServerInfo {
            id: ServerId::new(SERVER_ID),
            kind: ServerKind::Jellyfin,
            name: "测试".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            version: None,
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: Language::undetermined(),
        }
    }

    fn media_item(id: &str, kind: MediaKind, parent: Option<&str>) -> MediaItem {
        MediaItem {
            id: ItemId::new(id),
            kind,
            title: format!("片 {id}"),
            sort_title: format!("片 {id}"),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime: None,
            parent_id: parent.map(ItemId::new),
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet::default(),
            user_data: UserData::default(),
        }
    }

    fn media_source(item_id: &str) -> MediaSource {
        MediaSource {
            id: SourceId::new(format!("src-{item_id}")),
            item_id: ItemId::new(item_id),
            name: None,
            container: None,
            size: None,
            bitrate: None,
            runtime: None,
            streams: Vec::new(),
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        local: Arc<Local>,
        server: MockServer,
        server_id: ServerId,
    }

    impl Harness {
        async fn new() -> Self {
            let server = MockServer::start().await;
            let dir = tempfile::tempdir().expect("临时目录");
            let db_path = dir.path().join("nova.db");
            {
                let db = Database::open(&db_path).expect("建库");
                db.upsert_server(&server_info(), 1).expect("写服务器");
            }
            let env = Env {
                data_dir: dir.path().to_path_buf(),
                credentials: CredentialMode::Memory,
            };
            let mut local = Local::open(&env, None).expect("打开状态目录");
            // provider 与 `Local` 共用同一份凭证存储：`Provider::new` 构造时从这里读出令牌，
            // 不用真的走一次登录接口。
            let store: Arc<dyn CredentialStore> = {
                let store = MemoryStore::new();
                store
                    .set(
                        &format!("jellyfin/{SERVER_ID}/token"),
                        &SecretString::new(TOKEN),
                    )
                    .expect("写令牌");
                Arc::new(store)
            };
            let provider = Provider::new(
                Flavor::Jellyfin,
                ProviderConfig::new(ServerId::new(SERVER_ID), server.uri()).with_user_id(USER_ID),
                Arc::clone(&store),
            )
            .expect("wiremock 地址合法")
            // 断言"请求次数"时不能让重试把一次失败变成两次。
            .with_retry_policy(RetryPolicy::none());
            local.credentials = store;
            *local.provider.lock().expect("provider 锁") = Some(Arc::new(provider));
            local.set_status(Status::Connected {
                server: "测试服务器".to_owned(),
                base_url: server.uri(),
            });
            Self {
                _dir: dir,
                local: Arc::new(local),
                server,
                server_id: ServerId::new(SERVER_ID),
            }
        }

        async fn refresh(&self) -> RefreshReport {
            refresh(Arc::clone(&self.local), self.local.status()).await
        }

        fn seed(&self, items: &[MediaItem]) {
            self.local
                .db
                .upsert_items(&self.server_id, items, 1)
                .expect("写条目");
        }

        fn cached(&self, id: &str) -> Option<MediaItem> {
            self.local
                .db
                .item(&self.server_id, &ItemId::new(id))
                .expect("读条目")
        }

        fn cached_in_library(&self) -> Vec<MediaItem> {
            self.local
                .db
                .children(&self.server_id, Some(&ItemId::new(LIBRARY_ID)))
                .expect("按库读缓存")
        }

        async fn verify(&self) {
            // 未满足的 `expect(n)` 会在这里 panic，附带每条 mock 的命中次数。
            self.server.verify().await;
        }
    }

    async fn mount_views(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(format!("/Users/{USER_ID}/Views")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": [{
                    "Id": LIBRARY_ID,
                    "Name": "动画电影",
                    "Type": "CollectionFolder",
                    "CollectionType": "movies",
                }],
                "TotalRecordCount": 1,
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    fn movie_json(id: &str) -> Value {
        json!({ "Id": id, "Name": format!("片 {id}"), "Type": "Movie" })
    }

    /// 服务端在**子文件夹**里的条目：列表响应会带真实 `ParentId`（物理文件夹），
    /// 那不是库归属。
    fn movie_in_folder_json(id: &str) -> Value {
        let mut value = movie_json(id);
        value["ParentId"] = json!("folder-physical");
        value
    }

    fn plain_page(start: u32, count: u32) -> Vec<Value> {
        (start..start + count)
            .map(|index| movie_json(&format!("m{index:03}")))
            .collect()
    }

    async fn mount_page(server: &MockServer, start: u32, items: &[Value], total: u32) {
        Mock::given(method("GET"))
            .and(path(format!("/Users/{USER_ID}/Items")))
            .and(query_param("ParentId", LIBRARY_ID))
            // 只问库自己的那一层（电影库 → 电影），别把季 / 集当库的直接子项拉回来。
            .and(query_param("IncludeItemTypes", "Movie"))
            .and(query_param("StartIndex", start.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Items": items,
                "TotalRecordCount": total,
                "StartIndex": start,
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_page_failure(server: &MockServer, start: u32, status: u16) {
        Mock::given(method("GET"))
            .and(path(format!("/Users/{USER_ID}/Items")))
            .and(query_param("ParentId", LIBRARY_ID))
            .and(query_param("IncludeItemTypes", "Movie"))
            .and(query_param("StartIndex", start.to_string()))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(server)
            .await;
    }

    /// 三个完整分页的 mock（0 / 200 / 400）。
    async fn mount_full_library(server: &MockServer) {
        mount_views(server).await;
        mount_page(server, 0, &plain_page(0, PAGE), TOTAL).await;
        mount_page(server, PAGE, &plain_page(PAGE, PAGE), TOTAL).await;
        mount_page(server, 400, &plain_page(400, TOTAL - 400), TOTAL).await;
    }

    /// 修复前：只请求 `StartIndex=0`（缓存 200 条），后两页的 mock 从未命中。
    #[tokio::test]
    async fn a_big_library_is_paged_into_the_cache_completely() {
        let harness = Harness::new().await;
        mount_full_library(&harness.server).await;

        let report = harness.refresh().await;
        harness.verify().await;

        assert!(report.status.is_connected(), "刷新应当成功");
        // 报告里的每库统计也要对得上（日志/真机核对都读它）。
        assert_eq!(report.synced.len(), 1);
        let sync = &report.synced[0];
        assert_eq!(sync.total, TOTAL, "服务端报告的条目总数");
        assert_eq!(sync.cached, u64::from(TOTAL), "三页都应该写进缓存");
        assert_eq!(sync.removed, 0, "没有过期条目要删");
        assert!(sync.complete, "三页都拉到了");
        let cached = harness.cached_in_library();
        assert_eq!(cached.len() as u32, TOTAL, "整库 450 条都要进缓存");
        // 最后一页的条目也在，才算真的翻到底（不是只多拉了一页）。
        assert!(harness.cached("m449").is_some(), "第三页的末条要入缓存");
        // 库归属由列表刷新写下（服务端列表响应不带 ParentId 时由 provider 回填）。
        assert!(
            cached
                .iter()
                .all(|item| item.parent_id.as_ref().map(ItemId::as_str) == Some(LIBRARY_ID)),
            "库页按 ParentId 查缓存，归属必须是这个库"
        );
    }

    /// 修复前：第二页 500 时不会删（恰好也"通过"），但要求"已拉到的页保留 +
    /// 未拉到的页不出现"；配合上一条测试证明"完整拉取"是删除的前提。
    #[tokio::test]
    async fn a_failed_page_keeps_the_cached_items() {
        let harness = Harness::new().await;
        harness.seed(&[media_item("keep-me", MediaKind::Movie, Some(LIBRARY_ID))]);
        mount_views(&harness.server).await;
        mount_page(&harness.server, 0, &plain_page(0, PAGE), TOTAL).await;
        mount_page_failure(&harness.server, PAGE, 500).await;

        let report = harness.refresh().await;
        harness.verify().await;

        assert!(report.status.is_connected(), "单个库失败不影响连接状态");
        assert!(!report.synced[0].complete, "第二页失败 → 这个库没拉全");
        assert_eq!(report.synced[0].removed, 0, "没拉全就不许清理");
        assert!(
            harness.cached("keep-me").is_some(),
            "中途失败不能删本地缓存"
        );
        assert!(harness.cached("m000").is_some(), "失败前拉到的页要保留");
        assert!(harness.cached("m449").is_none(), "没拉到的页不该凭空出现");
    }

    /// 修复前：服务端已删除的条目永远留在缓存里（`gone` 会一直显示）。
    #[tokio::test]
    async fn items_deleted_on_the_server_are_removed_after_a_complete_fetch() {
        let harness = Harness::new().await;
        harness.seed(&[
            media_item("gone", MediaKind::Movie, Some(LIBRARY_ID)),
            media_item("gone-season", MediaKind::Season, Some("gone")),
            media_item("gone-episode", MediaKind::Episode, Some("gone-season")),
        ]);
        harness
            .local
            .db
            .upsert_sources(&harness.server_id, &[media_source("gone")])
            .expect("写资源");
        harness
            .local
            .db
            .add_to_mylist(&harness.server_id, &ItemId::new("gone"))
            .expect("收藏");
        mount_full_library(&harness.server).await;

        let report = harness.refresh().await;
        harness.verify().await;

        assert_eq!(
            report.synced[0].removed, 3,
            "删掉 1 部电影 + 它的 1 季 + 1 集"
        );
        assert!(
            harness.cached("gone").is_none(),
            "服务端已删除的条目要从缓存清理"
        );
        assert!(
            harness.cached("gone-season").is_none(),
            "季跟着剧集一起清理，否则留下指向不存在剧集的孤儿行"
        );
        assert!(
            harness.cached("gone-episode").is_none(),
            "集也跟着清理，否则它会一直出现在「继续观看」里"
        );
        assert!(
            harness
                .local
                .db
                .sources(&harness.server_id, &ItemId::new("gone"))
                .expect("读资源")
                .is_empty(),
            "播放资源按外键级联删除"
        );
        // 「我的列表」是用户数据（`mylist` 表没有外键）：条目回来后收藏还在。
        assert!(
            harness
                .local
                .db
                .in_mylist(&harness.server_id, &ItemId::new("gone"))
                .expect("读收藏"),
            "清理缓存不该动用户收藏"
        );
    }

    /// 修复前：`m000` 的归属停留在物理文件夹上（服务端列表给了 `ParentId` 就不回填），
    /// `ghost` 这种"父项既不是库也不是本地条目"的死缓存也不会被清理。
    #[tokio::test]
    async fn a_broken_library_membership_is_repaired_by_the_full_listing() {
        let harness = Harness::new().await;
        harness.seed(&[
            media_item("m000", MediaKind::Movie, Some("folder-physical")),
            media_item("ghost", MediaKind::Movie, Some("folder-gone")),
        ]);
        mount_views(&harness.server).await;
        let mut page = plain_page(0, PAGE);
        page[0] = movie_in_folder_json("m000");
        mount_page(&harness.server, 0, &page, TOTAL).await;
        mount_page(&harness.server, PAGE, &plain_page(PAGE, PAGE), TOTAL).await;
        mount_page(&harness.server, 400, &plain_page(400, TOTAL - 400), TOTAL).await;

        harness.refresh().await;
        harness.verify().await;

        let repaired = harness.cached("m000").expect("m000 还在缓存里");
        assert!(
            repaired.parent_id.as_ref().map(ItemId::as_str) == Some(LIBRARY_ID),
            "列表刷新是库归属的权威来源：物理 ParentId 要被纠正成库"
        );
        assert!(
            harness.cached("ghost").is_none(),
            "完整拉取后仍指向不存在父项的条目是死缓存，清理掉"
        );
    }

    /// 层级断掉的季 / 集是旧版列表路径留下的垃圾（父项为空 / 悬空）：它们漏进
    /// 顶层列表冒充卡片，完整拉取后一并清掉；顶层电影（父项 NULL）是合法状态。
    #[tokio::test]
    async fn hierarchy_rows_without_a_parent_are_cleaned_up() {
        let harness = Harness::new().await;
        harness.seed(&[
            media_item("stray-season", MediaKind::Season, None),
            media_item("stray-episode", MediaKind::Episode, None),
            media_item("orphan-episode", MediaKind::Episode, Some("no-such-season")),
            media_item("top-movie", MediaKind::Movie, None),
        ]);
        mount_full_library(&harness.server).await;

        harness.refresh().await;
        harness.verify().await;

        assert!(
            harness.cached("stray-season").is_none(),
            "没有父项的季在层级里根本找不到"
        );
        assert!(
            harness.cached("stray-episode").is_none(),
            "没有父项的集会漏进首页「最近添加」"
        );
        assert!(
            harness.cached("orphan-episode").is_none(),
            "父项已不存在的集同样是死缓存"
        );
        assert!(
            harness.cached("top-movie").is_some(),
            "顶层条目（父项 NULL 的电影）是合法状态，不能误删"
        );
    }

    /// 中途 401：沿用 `mark_expired`，不删缓存（否则一次令牌过期就把库清空）。
    #[tokio::test]
    async fn unauthorized_mid_pagination_marks_the_server_expired_without_deleting() {
        let harness = Harness::new().await;
        harness.seed(&[media_item("keep-me", MediaKind::Movie, Some(LIBRARY_ID))]);
        mount_views(&harness.server).await;
        mount_page(&harness.server, 0, &plain_page(0, PAGE), TOTAL).await;
        mount_page_failure(&harness.server, PAGE, 401).await;

        let report = harness.refresh().await;
        harness.verify().await;

        assert!(
            matches!(report.status, Status::Expired { .. }),
            "401 走 mark_expired，与原来一致"
        );
        assert!(
            harness.cached("keep-me").is_some(),
            "令牌过期不清缓存，重新登录后继续用"
        );
    }
}
