// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova items` 写缓存的卫生规则（wiremock，不碰真实服务器）。
//!
//! 修复前的现象：不带 `--limit` 时把服务端返回的**整页 / 整库**（Jellyfin 列表响应
//! 不带 `ParentId`，季 / 集因此成了 `parent_id = NULL` 的孤儿）原样写进 nova.db，
//! 真实数据目录被撑到 4 万行，而且永远不清理。
//!
//! 现在与桌面端刷新共用同一条归属规则（[`nova_storage::library_items`]）：
//! 只写电影 / 剧集，父项写成 `--library` 指定的库；`--limit` 默认 200（与刷新
//! 每页一致），不再一次把整库拉进缓存。

use std::sync::Arc;

use nova_cli::{App, CredentialMode, Env, run_in};
use nova_core::{AuthState, ItemId, Language, ServerId, ServerInfo, ServerKind};
use nova_secrets::{CredentialStore, SecretString};
use nova_storage::Database;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SERVER_ID: &str = "srv-cli-cache";
const USER_ID: &str = "user-cli";
const LIBRARY_ID: &str = "lib-movies";
const CREDENTIAL_KEY: &str = "jellyfin/srv-cli-cache/token";
const DEFAULT_LIMIT: &str = "200";

fn env(dir: &std::path::Path) -> Env {
    Env {
        data_dir: dir.to_path_buf(),
        credentials: CredentialMode::Memory,
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn server_info(base_url: &str) -> ServerInfo {
    ServerInfo {
        id: ServerId::new(SERVER_ID),
        kind: ServerKind::Jellyfin,
        name: "测试服务器".to_owned(),
        base_url: base_url.to_owned(),
        version: None,
        user_id: Some(USER_ID.to_owned()),
        auth: AuthState::Authorized {
            user_name: "nova".to_owned(),
            credential_key: CREDENTIAL_KEY.to_owned(),
        },
        metadata_language: Language::undetermined(),
    }
}

/// 服务端一页里混着电影、剧集、季与集；电影带**物理** ParentId（子文件夹）。
fn page() -> serde_json::Value {
    json!({
        "Items": [
            { "Id": "m1", "Name": "沙丘", "Type": "Movie", "ParentId": "folder-physical" },
            { "Id": "s1", "Name": "沙丘：预言", "Type": "Series" },
            { "Id": "se1", "Name": "第 1 季", "Type": "Season", "ParentId": "s1" },
            { "Id": "e1", "Name": "第 1 集", "Type": "Episode", "ParentId": "se1" }
        ],
        "TotalRecordCount": 4,
        "StartIndex": 0
    })
}

async fn mount_items(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items")))
        .and(query_param("ParentId", LIBRARY_ID))
        // 默认 `--limit`：与桌面端刷新每页一致。修复前这里不带 Limit，mock 不会命中。
        .and(query_param("Limit", DEFAULT_LIMIT))
        .respond_with(ResponseTemplate::new(200).set_body_json(page()))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn items_caches_only_movies_and_series_under_the_library() {
    let server = MockServer::start().await;
    mount_items(&server).await;

    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path());
    {
        let app = App::open(&env).expect("打开状态目录");
        app.database()
            .upsert_server(&server_info(&server.uri()), 1)
            .expect("写服务器");
        let store: Arc<dyn CredentialStore> = app.credential_store().clone();
        store
            .set(CREDENTIAL_KEY, &SecretString::new("token-cli"))
            .expect("写令牌");
    }

    // 不带 `--limit`：默认 200，且不再把整库写进缓存。
    let text = run_in(&env, argv(&["items", "--library", LIBRARY_ID, "--json"]))
        .await
        .expect("items 应当成功");
    server.verify().await;
    let output: serde_json::Value = serde_json::from_str(&text).expect("输出应当是 JSON");
    assert_eq!(
        output["items"].as_array().map(Vec::len),
        Some(4),
        "命令行输出仍然是服务端这一页（季 / 集也能看到）：{output}"
    );

    // 缓存里只能有电影 / 剧集，父项是媒体库（与桌面端刷新一致）。
    let db = Database::open(dir.path().join("nova.db")).expect("读缓存库");
    let server_id = ServerId::new(SERVER_ID);
    assert_eq!(db.item_count(&server_id).expect("条目数"), 2);
    let cached = db
        .children(&server_id, Some(&ItemId::new(LIBRARY_ID)))
        .expect("按库读缓存");
    let mut ids: Vec<&str> = cached.iter().map(|item| item.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["m1", "s1"], "只写电影 / 剧集，季 / 集不入库");
    assert!(
        cached
            .iter()
            .all(|item| item.parent_id.as_ref().map(ItemId::as_str) == Some(LIBRARY_ID)),
        "父项必须纠正成库（服务端的物理 ParentId 不算库归属）"
    );
    assert!(
        db.item(&server_id, &ItemId::new("se1"))
            .expect("读季")
            .is_none(),
        "季是播放层级，不能作为孤儿行进缓存"
    );
    assert!(
        db.item(&server_id, &ItemId::new("e1"))
            .expect("读集")
            .is_none(),
        "集是播放层级，不能作为孤儿行进缓存"
    );

    // `--cached` 仍然可用，并且只列电影 / 剧集。
    let cached_text = run_in(
        &env,
        argv(&["items", "--library", LIBRARY_ID, "--cached", "--json"]),
    )
    .await
    .expect("items --cached 应当成功");
    let cached: serde_json::Value = serde_json::from_str(&cached_text).expect("输出应当是 JSON");
    assert_eq!(cached["cached_total"], 2);
    assert_eq!(cached["items"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn items_help_documents_the_default_limit_and_cache_rule() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path());
    let help = run_in(&env, argv(&["help", "items"]))
        .await
        .expect("帮助应当成功");
    assert!(
        help.contains("默认 200"),
        "帮助要说明 --limit 的默认值：{help}"
    );
    assert!(
        help.contains("电影 / 剧集"),
        "帮助要说明缓存写入规则：{help}"
    );
}
