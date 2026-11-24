// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 后台刷新：`libraries()` → 每个库 `items()`（前 N 条）→ `upsert_items`。
//!
//! 刷新是"锦上添花"：失败只改变 [`Status`]，不抛给界面、不清缓存。界面在刷新前
//! 就已经用 SQLite 渲染过一次了（离线优先），刷新完成后由调用方重建视图。
//!
//! 数据库写入放在 `spawn_blocking` 里：`nova-storage` 是同步阻塞的（内部
//! `Mutex<Connection>`），在 tokio 工作线程上直接写会拖慢图片下载与其它请求。

use std::sync::Arc;

use nova_provider::{ItemQuery, MediaProvider, ProviderError};
use nova_storage::Database;

use crate::error::AppError;
use crate::local::Local;
use crate::status::Status;

/// 每个媒体库首次刷新取多少条。
///
/// 200 条足够铺满首页与几屏网格；整库同步留给后续阶段的增量同步。
pub const ITEMS_PER_LIBRARY: u32 = 200;

/// 一次刷新的结果。
#[derive(Clone, Debug)]
pub struct RefreshReport {
    /// 刷新后的连接状态。
    pub status: Status,
    /// 服务端返回的媒体库（已写进 [`Local`]）。
    pub libraries: Vec<nova_provider::Library>,
    /// 刷新后本地缓存的条目总数。
    pub item_count: u64,
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
        };
    }
    let Some(provider) = local.provider() else {
        return RefreshReport {
            status: Status::RefreshFailed {
                message: "没有可用的服务端会话".to_owned(),
            },
            libraries: Vec::new(),
            item_count: local_item_count(&local).await,
        };
    };

    let libraries = match provider.libraries().await {
        Ok(libraries) => libraries,
        Err(error) => {
            return RefreshReport {
                status: failure_status(&local, &error),
                libraries: local.libraries(),
                item_count: local_item_count(&local).await,
            };
        }
    };
    local.set_libraries(libraries.clone());
    if let Err(error) = local.persist_info(&provider) {
        tracing::warn!(%error, "写回服务器信息失败");
    }

    for library in &libraries {
        let query = ItemQuery::in_parent(library.id.clone()).paged(0, ITEMS_PER_LIBRARY);
        match provider.items(query).await {
            Ok(page) => {
                if let Err(error) = cache_items(&local, page.items).await {
                    tracing::warn!(library = library.name, %error, "条目写入本地缓存失败");
                }
            }
            Err(ProviderError::Unauthorized) => {
                local.mark_expired();
                return RefreshReport {
                    status: local.status(),
                    libraries,
                    item_count: local_item_count(&local).await,
                };
            }
            Err(error) => {
                // 单个库失败不影响其它库：界面继续显示已缓存的部分。
                tracing::warn!(library = library.name, %error, "拉取媒体库条目失败");
            }
        }
    }

    let info = provider.server_info();
    RefreshReport {
        status: Status::Connected {
            server: info.name,
            base_url: info.base_url,
        },
        libraries,
        item_count: local_item_count(&local).await,
    }
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

async fn cache_items(local: &Arc<Local>, items: Vec<nova_core::MediaItem>) -> Result<(), AppError> {
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
        assert_eq!(report.item_count, 1, "离线也要能数到缓存里的条目");
        assert_eq!(local.home(None).expect("构建首页").recently_added.len(), 1);
        let _ = StoredServer {
            info: server_info(),
            created_at: 1,
            last_seen_at: 1,
        };
    }
}
