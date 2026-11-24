// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 端到端流程：登录 → 媒体库 → 条目 → 详情 → 资源 → 播放地址 → 进度上报。
//!
//! 全部跑在 wiremock 上，断言的是"客户端实际发出的请求长什么样"，
//! 而不只是"返回值解析对了"。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    EMBY_TOKEN, EMBY_USER_ID, ITEM_ID, JELLYFIN_TOKEN, SOURCE_ID, USER_ID, json_fixture, provider,
    provider_without_retry,
};
use nova_core::{AuthState, ItemId, MediaKind, PlayMethod, PlaybackProgress, SourceId, StreamKind};
use nova_provider::{Flavor, ImageKind, ImageRequest, ItemQuery, MediaProvider, SortBy};
use nova_secrets::{CredentialStore, MemoryStore, SecretString};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn jellyfin_flow_from_login_to_progress() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    // --- 登录 ---------------------------------------------------------------
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(common::auth_header("authorization", None))
        .and(body_partial_json(json!({
            "Username": "alice",
            "Pw": "correct horse battery staple"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("auth_jellyfin")))
        .expect(1)
        .mount(&server)
        .await;

    let info = provider
        .authenticate("alice", &SecretString::new("correct horse battery staple"))
        .await
        .expect("登录应当成功");
    assert_eq!(info.kind, nova_core::ServerKind::Jellyfin);
    assert_eq!(
        info.auth,
        AuthState::Authorized {
            user_name: "alice".to_owned(),
            credential_key: "jellyfin/srv-1/token".to_owned(),
        }
    );
    assert_eq!(info.user_id.as_deref(), Some(USER_ID));
    // 令牌进了注入的凭证存储，键名按约定。
    assert_eq!(
        store
            .get("jellyfin/srv-1/token")
            .expect("内存存储不会失败")
            .as_ref()
            .map(SecretString::expose),
        Some(JELLYFIN_TOKEN)
    );

    // --- 媒体库 -------------------------------------------------------------
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(1)
        .mount(&server)
        .await;

    let libraries = provider.libraries().await.expect("应当能列出媒体库");
    assert_eq!(libraries.len(), 2);
    assert_eq!(libraries[0].name, "电影");
    assert_eq!(libraries[0].kind, MediaKind::Movie);
    assert_eq!(libraries[1].kind, MediaKind::Series);

    // --- 条目列表（分页参数必须原样传给服务端）--------------------------------
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items")))
        .and(query_param("StartIndex", "40"))
        .and(query_param("Limit", "20"))
        .and(query_param("IncludeItemTypes", "Movie"))
        .and(query_param("Recursive", "true"))
        .and(query_param("SortBy", "PremiereDate"))
        .and(query_param("SortOrder", "Ascending"))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("items_movies")))
        .expect(1)
        .mount(&server)
        .await;

    let page = provider
        .items(
            ItemQuery::new()
                .with_kinds([MediaKind::Movie])
                .with_sort(SortBy::PremiereDate)
                .paged(40, 20),
        )
        .await
        .expect("应当能取到条目列表");
    assert_eq!(page.total, 4, "TotalRecordCount 要映射到 Page::total");
    assert_eq!(page.start, 40, "start 回显请求里的偏移");
    assert_eq!(page.limit, 20);
    assert_eq!(page.items.len(), 3, "BoxSet 这类未知类型应当被跳过");
    assert_eq!(page.items[0].title, "银翼杀手 2049");
    assert_eq!(
        page.items[0].images.poster_tag.as_deref(),
        Some("d41d8cd98f00b204e9800998ecf8427e"),
        "列表响应里的 ImageTags 要映射成版本标记"
    );
    assert!(
        !page.has_more(),
        "40 + 3 已经越过报告的总数 4，不该谎称还有下一页"
    );

    // --- 单个条目 -----------------------------------------------------------
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items/{ITEM_ID}")))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("item_movie")))
        .expect(1)
        .mount(&server)
        .await;

    let item = provider
        .item(&ItemId::new(ITEM_ID))
        .await
        .expect("应当能取到条目");
    assert_eq!(item.kind, MediaKind::Movie);
    assert_eq!(item.runtime, Some(Duration::from_secs(9816)));
    assert_eq!(item.user_data.play_count, 1);
    assert!(item.images.poster.is_some());
    assert_eq!(
        item.images.poster.as_deref(),
        Some(
            format!(
                "{}/Items/{ITEM_ID}/Images/Primary?tag=d41d8cd98f00b204e9800998ecf8427e",
                server.uri()
            )
            .as_str()
        ),
        "图片地址必须是基于基址拼出来的绝对地址"
    );

    // --- 可播放资源 ---------------------------------------------------------
    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/PlaybackInfo")))
        .and(query_param("UserId", USER_ID))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("playback_info")))
        .expect(1)
        .mount(&server)
        .await;

    let sources = provider
        .sources(&ItemId::new(ITEM_ID))
        .await
        .expect("应当能取到媒体源");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id.as_str(), SOURCE_ID);
    assert_eq!(
        sources[0].runtime,
        Some(Duration::from_secs(9816)),
        "时长只有 PlaybackInfo / 详情才带得到"
    );
    assert_eq!(sources[0].streams.len(), 5);
    assert_eq!(
        sources[0]
            .video_stream()
            .map(|stream| stream.codec.as_str()),
        Some("hevc")
    );
    assert_eq!(
        sources[0]
            .subtitle_streams()
            .filter(|stream| stream.kind == StreamKind::Subtitle)
            .count(),
        2
    );

    // --- 播放地址（不发请求，纯本地拼接）-------------------------------------
    let target = provider
        .stream_target(&ItemId::new(ITEM_ID), &SourceId::new(SOURCE_ID))
        .await
        .expect("应当能构造播放地址");
    let url = target.url();
    assert!(
        url.starts_with(&format!("{}/Videos/{ITEM_ID}/stream?", server.uri())),
        "地址不对：{url}"
    );
    assert!(url.contains("Static=true"));
    assert!(url.contains(&format!("MediaSourceId={SOURCE_ID}")));
    assert!(url.contains(&format!("api_key={JELLYFIN_TOKEN}")));
    assert!(target.is_plaintext(), "http:// 应当识别为明文");
    // Debug / Display 都必须抹掉查询串。
    assert!(!format!("{target:?}").contains(JELLYFIN_TOKEN));
    assert!(!target.to_string().contains(JELLYFIN_TOKEN));

    // --- 进度上报 -----------------------------------------------------------
    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .and(body_partial_json(json!({
            "ItemId": ITEM_ID,
            "MediaSourceId": SOURCE_ID,
            "PositionTicks": 4_908_000_000_i64,
            "IsPaused": false,
            "CanSeek": true,
            "PlayMethod": "DirectPlay"
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    provider
        .report_progress(&PlaybackProgress {
            item_id: ItemId::new(ITEM_ID),
            source_id: SourceId::new(SOURCE_ID),
            position: Duration::from_millis(490_800),
            duration: Some(Duration::from_secs(9816)),
            played: false,
            paused: false,
            method: PlayMethod::DirectPlay,
        })
        .await
        .expect("进度上报应当成功");

    // --- 图片 ---------------------------------------------------------------
    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/Images/Primary")))
        .and(query_param("maxWidth", "600"))
        .and(query_param("quality", "90"))
        .and(query_param("tag", "d41d8cd98f00b204e9800998ecf8427e"))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-png-bytes".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let bytes = provider
        .image(
            ImageRequest::new(ItemId::new(ITEM_ID), ImageKind::Poster)
                .with_width(600)
                .with_tag("d41d8cd98f00b204e9800998ecf8427e"),
        )
        .await
        .expect("应当能下载图片");
    assert_eq!(bytes, b"fake-png-bytes");

    // --- 令牌不进 Debug -----------------------------------------------------
    let debug = format!("{provider:?}");
    assert!(!debug.contains(JELLYFIN_TOKEN), "Debug 泄漏令牌：{debug}");
    assert!(debug.contains("flavor: Jellyfin"));
    assert!(debug.contains("jellyfin/srv-1/token"));
}

#[tokio::test]
async fn sub_items_and_played_progress_stop_the_session() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    // 季 → 集
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items")))
        .and(query_param("ParentId", ITEM_ID))
        .and(query_param("Recursive", "false"))
        .and(query_param("SortBy", "ParentIndexNumber,IndexNumber"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [json_fixture("item_episode")],
            "TotalRecordCount": 1,
            "StartIndex": 0
        })))
        .expect(1)
        .mount(&server)
        .await;

    let children = provider
        .children(&ItemId::new(ITEM_ID))
        .await
        .expect("应当能列出子条目");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].kind, MediaKind::Episode);
    assert_eq!(children[0].index_number, Some(1));
    assert_eq!(
        children[0].user_data.resume_position,
        Duration::from_secs(754)
    );

    // 进度与"结束会话"是两条独立路径：
    // `report_progress` 只报位置（哪怕 played=true 也不兼职停播），`report_stopped` 才结束会话。
    // 旧实现让 report_progress 在 played=true 时顺手发 Stopped，结果是"正常退出"永远不发停播，
    // 服务端续播点恒为 0（实测），所以这里显式断言新契约。
    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Stopped"))
        .and(body_partial_json(json!({
            "ItemId": ITEM_ID,
            "PositionTicks": 98_160_000_000_i64
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let progress = PlaybackProgress {
        item_id: ItemId::new(ITEM_ID),
        source_id: SourceId::new(SOURCE_ID),
        position: Duration::from_secs(9816),
        duration: Some(Duration::from_secs(9816)),
        played: true,
        paused: false,
        method: PlayMethod::DirectPlay,
    };

    provider
        .report_progress(&progress)
        .await
        .expect("进度上报应当成功");
    provider
        .report_stopped(&progress)
        .await
        .expect("结束会话上报应当成功");
}

#[tokio::test]
async fn sources_fall_back_to_item_detail_when_playback_info_is_empty() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/PlaybackInfo")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "MediaSources": [] })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items/{ITEM_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("item_movie")))
        .expect(1)
        .mount(&server)
        .await;

    let sources = provider
        .sources(&ItemId::new(ITEM_ID))
        .await
        .expect("应当回退到条目详情里的媒体源");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id.as_str(), SOURCE_ID);
}

#[tokio::test]
async fn emby_uses_its_own_header_and_password_field() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Emby, Arc::clone(&store));

    // 首选 `Pw`（Emby 4.10 只认它，实测发 `Password` 会 401）。
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(common::auth_header("x-emby-authorization", None))
        .and(body_partial_json(json!({
            "Username": "bob",
            "Pw": "hunter2"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("auth_emby")))
        .expect(1)
        .mount(&server)
        .await;

    let info = provider
        .authenticate("bob", &SecretString::new("hunter2"))
        .await
        .expect("Emby 登录应当成功");
    assert_eq!(info.kind, nova_core::ServerKind::Emby);
    assert_eq!(
        store
            .get("emby/srv-1/token")
            .expect("内存存储不会失败")
            .as_ref()
            .map(SecretString::expose),
        Some(EMBY_TOKEN)
    );

    // Emby 的请求头名字与 Jellyfin 不同，用户标识也来自它自己的响应。
    Mock::given(method("GET"))
        .and(path(format!("/Users/{EMBY_USER_ID}/Views")))
        .and(common::auth_header(
            "x-emby-authorization",
            Some(EMBY_TOKEN),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        provider.libraries().await.expect("应当能列出媒体库").len(),
        2
    );
}

#[tokio::test]
async fn emby_token_can_come_from_the_legacy_header() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider_without_retry(&server, Flavor::Emby, Arc::clone(&store));

    // 旧版 Emby 的响应体里没有 AccessToken，只在响应头里给。
    let mut body = json_fixture("auth_emby");
    body.as_object_mut()
        .expect("fixture 应当是对象")
        .remove("AccessToken");
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Emby-Token", "legacy-header-token")
                .set_body_json(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    provider
        .authenticate("bob", &SecretString::new("hunter2"))
        .await
        .expect("应当从响应头里取到令牌");
    assert_eq!(
        store
            .get("emby/srv-1/token")
            .expect("内存存储不会失败")
            .as_ref()
            .map(SecretString::expose),
        Some("legacy-header-token")
    );
}

#[tokio::test]
async fn playback_info_bad_request_means_the_item_has_no_sources() {
    // 真实服务端行为：容器条目（Folder / 剧集 / 季）的 `PlaybackInfo` 回 400，
    // 而不是 200 + 空数组。这是"没有可播放文件"，不是错误。
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/PlaybackInfo")))
        .and(query_param("UserId", USER_ID))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "Message": "无法为文件夹获取播放信息" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let sources = provider
        .sources(&ItemId::new(ITEM_ID))
        .await
        .expect("400 是“没有媒体源”，不是错误");
    assert!(sources.is_empty(), "实际：{sources:?}");
}

#[tokio::test]
async fn playback_info_not_found_means_the_item_has_no_sources() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/PlaybackInfo")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    assert!(
        provider
            .sources(&ItemId::new(ITEM_ID))
            .await
            .expect("404 同样只是“没有媒体源”")
            .is_empty()
    );
}

#[tokio::test]
async fn playback_info_forbidden_still_fails() {
    // 403 是“你没权限”，不是“这个条目没有源”：必须继续报错。
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/PlaybackInfo")))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider
        .sources(&ItemId::new(ITEM_ID))
        .await
        .expect_err("403 必须报错");
    assert!(
        matches!(error, nova_provider::ProviderError::Unauthorized),
        "实际错误：{error:?}"
    );
}

#[tokio::test]
async fn progress_report_carries_pause_state_and_play_method() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    // 暂停中 + 转码。
    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .and(body_partial_json(json!({
            "ItemId": ITEM_ID,
            "PositionTicks": 60_000_000_000_i64,
            "IsPaused": true,
            "PlayMethod": "Transcode"
        })))
        .respond_with(ResponseTemplate::new(204))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    provider
        .report_progress(&PlaybackProgress {
            item_id: ItemId::new(ITEM_ID),
            source_id: SourceId::new(SOURCE_ID),
            position: Duration::from_secs(6000),
            duration: Some(Duration::from_secs(9816)),
            played: false,
            paused: true,
            method: PlayMethod::Transcode,
        })
        .await
        .expect("进度上报应当成功");

    // 播放中 + 直接流。
    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .and(body_partial_json(json!({
            "IsPaused": false,
            "PlayMethod": "DirectStream"
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    provider
        .report_progress(&PlaybackProgress {
            item_id: ItemId::new(ITEM_ID),
            source_id: SourceId::new(SOURCE_ID),
            position: Duration::from_secs(6100),
            duration: Some(Duration::from_secs(9816)),
            played: false,
            paused: false,
            method: PlayMethod::DirectStream,
        })
        .await
        .expect("进度上报应当成功");
}

/// Emby 各版本的密码字段名不统一：4.10 只认 `Pw`，更早的只认 `Password`。
/// 首选 `Pw` 被 401 时应当换字段名重试一次（实测这条路径是让真实 Emby 能登录的关键）。
#[tokio::test]
async fn emby_retries_with_the_legacy_password_field_after_401() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Emby, Arc::clone(&store));

    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(body_partial_json(json!({ "Pw": "hunter2" })))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(body_partial_json(json!({ "Password": "hunter2" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("auth_emby")))
        .expect(1)
        .mount(&server)
        .await;

    provider
        .authenticate("bob", &SecretString::new("hunter2"))
        .await
        .expect("旧版 Emby 用 Password 重试后应当成功");
}

/// 列表响应不带 `ParentId`（Jellyfin 12 / Emby 4.10 实测如此）。
/// 递归列表里只有"库的直接子项类型"（电影/剧集）回填请求的 parent，
/// 季/集不能回填——它们的父项各不相同，统一写同一个父会让缓存按库检索出错。
#[tokio::test]
async fn recursive_list_backfills_parent_only_for_top_level_kinds() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items")))
        .and(query_param("ParentId", "library-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [
                { "Id": "m1", "Name": "电影", "Type": "Movie" },
                { "Id": "s1", "Name": "剧集", "Type": "Series" },
                { "Id": "e1", "Name": "第 1 集", "Type": "Episode" }
            ],
            "TotalRecordCount": 3
        })))
        .mount(&server)
        .await;

    let page = provider
        .items(ItemQuery {
            parent: Some(ItemId::new("library-1")),
            ..ItemQuery::default()
        })
        .await
        .expect("列条目应当成功");

    let by_id = |id: &str| {
        page.items
            .iter()
            .find(|item| item.id.as_str() == id)
            .unwrap_or_else(|| panic!("缺少条目 {id}"))
    };
    assert_eq!(
        by_id("m1").parent_id.as_ref().map(|id| id.as_str()),
        Some("library-1"),
        "电影是库的直接子项，应当回填"
    );
    assert_eq!(
        by_id("s1").parent_id.as_ref().map(|id| id.as_str()),
        Some("library-1"),
        "剧集是库的直接子项，应当回填"
    );
    assert_eq!(by_id("e1").parent_id, None, "集的父项不是媒体库，不能回填");
}
