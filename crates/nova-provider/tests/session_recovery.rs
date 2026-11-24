// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 会话恢复与公开服务器信息。
//!
//! 真实联调暴露的硬伤：`Provider::new` 只恢复令牌，`user_id` 只有 `authenticate()`
//! 成功后才写入，于是"进程一登录、进程二读库"必然 `Unauthorized`（多数端点要拼
//! `/Users/{user_id}/…`）。这里断言两条恢复路径：调用方直接传 `user_id`，
//! 或者留空由本层用 `/Users/Me` 惰性解析一次。

mod common;

use std::sync::Arc;

use common::{JELLYFIN_TOKEN, USER_ID, json_fixture, provider, provider_without_retry};
use nova_core::{ServerId, ServerKind};
use nova_provider::{Flavor, MediaProvider, Provider, ProviderConfig, ProviderError};
use nova_secrets::{CredentialStore, MemoryStore, SecretString};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 只从凭证存储恢复令牌的 provider（`user_id` 未知）。
fn restored_provider(server: &MockServer) -> (Provider, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::new());
    store
        .set("jellyfin/srv-1/token", &SecretString::new(JELLYFIN_TOKEN))
        .expect("内存存储不会失败");
    let provider = provider(server, Flavor::Jellyfin, Arc::clone(&store));
    (provider, store)
}

/// 挂载 `/Users/Me`。
async fn mount_users_me(server: &MockServer, expect: u64) {
    Mock::given(method("GET"))
        .and(path("/Users/Me"))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "Id": USER_ID, "Name": "alice" })),
        )
        .expect(expect)
        .mount(server)
        .await;
}

async fn count(server: &MockServer, suffix: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("wiremock 默认记录请求")
        .iter()
        .filter(|request| request.url.path().ends_with(suffix))
        .count()
}

#[tokio::test]
async fn stored_token_without_user_id_resolves_it_once_from_users_me() {
    let server = MockServer::start().await;
    let (provider, _store) = restored_provider(&server);
    assert_eq!(
        provider.server_info().user_id,
        None,
        "构造时还没有用户标识，但这不该报错"
    );

    mount_users_me(&server, 1).await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .and(common::auth_header("authorization", Some(JELLYFIN_TOKEN)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(2)
        .mount(&server)
        .await;

    // 并发两次：解析必须是单次的（否则第二个请求会去抢同一个 `/Users/Me`）。
    let (first, second) = tokio::join!(provider.libraries(), provider.libraries());
    assert_eq!(first.expect("第一个并发调用应当成功").len(), 2);
    assert_eq!(second.expect("第二个并发调用应当成功").len(), 2);

    assert_eq!(count(&server, "/Users/Me").await, 1, "并发下只能解析一次");
    assert_eq!(
        provider.server_info().user_id.as_deref(),
        Some(USER_ID),
        "解析结果要写回状态，后续请求直接用"
    );
}

#[tokio::test]
async fn explicit_user_id_never_requests_users_me() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    store
        .set("jellyfin/srv-1/token", &SecretString::new(JELLYFIN_TOKEN))
        .expect("内存存储不会失败");
    let config = ProviderConfig::new(ServerId::new("srv-1"), server.uri())
        .with_user_id(USER_ID)
        .with_display_name("测试服务器");
    let provider = Provider::new(
        Flavor::Jellyfin,
        config,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
    )
    .expect("wiremock 地址合法");

    assert_eq!(provider.server_info().user_id.as_deref(), Some(USER_ID));

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(1)
        .mount(&server)
        .await;

    assert_eq!(
        provider.libraries().await.expect("应当能列出媒体库").len(),
        2
    );
    assert_eq!(
        count(&server, "/Users/Me").await,
        0,
        "调用方已经给了 user_id，不该再去问服务端"
    );
}

#[tokio::test]
async fn failed_lazy_resolution_is_unauthorized() {
    let server = MockServer::start().await;
    let (provider, _store) = restored_provider(&server);

    // 令牌被服务端吊销：解析用户标识这一步就会失败，视图接口根本不会被请求。
    Mock::given(method("GET"))
        .and(path("/Users/Me"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当报未认证");
    assert!(matches!(error, ProviderError::Unauthorized), "{error:?}");
    assert_eq!(count(&server, "/Views").await, 0);
}

#[tokio::test]
async fn public_info_reports_kind_name_and_version() {
    // 产品名两种取值各跑一遍：Jellyfin 与 Emby。
    for (product, expected) in [
        ("Jellyfin Server", ServerKind::Jellyfin),
        ("Emby Server", ServerKind::Emby),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/System/Info/Public"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ServerName": "客厅的服务器",
                "Version": "10.10.3",
                "ProductName": product
            })))
            .expect(1)
            .mount(&server)
            .await;

        // 未登录（没有令牌）也必须能拿到公开信息。
        let provider = provider(&server, Flavor::Emby, Arc::new(MemoryStore::new()));
        let info = provider.public_info().await.expect("公开接口不需要令牌");

        assert_eq!(info.kind, expected, "ProductName = {product}");
        assert_eq!(info.name, "客厅的服务器");
        assert_eq!(info.version.as_deref(), Some("10.10.3"));
        // 窄结构体不含会话字段是**有意**的：公开端点拿不到 auth/user_id，
        // 以前返回完整 `ServerInfo` 时一旦被整体写回本地记录，就会把已登录状态冲掉。
        // 需要本机快照时用 `server_info()`。
        assert_eq!(
            provider.server_info().id,
            ServerId::new("srv-1"),
            "本机分配的标识不受公开信息影响"
        );
    }
}

#[tokio::test]
async fn unknown_product_name_falls_back_to_the_configured_flavor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ServerName": "第三方分支",
            "Version": "1.0.0",
            "ProductName": "Something Else"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider(&server, Flavor::Jellyfin, Arc::new(MemoryStore::new()));
    let info = provider.public_info().await.expect("应当能取到公开信息");
    assert_eq!(info.kind, ServerKind::Jellyfin);
}

#[tokio::test]
async fn authenticate_requests_the_public_endpoint_exactly_once() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ServerName": "家庭影院",
            "Version": "10.10.3",
            "ProductName": "Jellyfin Server"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(1)
        .mount(&server)
        .await;

    let info = provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");
    assert_eq!(info.version.as_deref(), Some("10.10.3"), "版本要顺带填上");
    assert_eq!(info.name, "家庭影院", "登录后服务器名来自服务端");
    assert_eq!(info.user_id.as_deref(), Some(USER_ID));

    // 之后的请求复用状态，不再打公开接口。
    assert_eq!(provider.libraries().await.expect("应当成功").len(), 2);
    assert_eq!(count(&server, "/System/Info/Public").await, 1);
}

#[tokio::test]
async fn public_endpoint_failure_does_not_fail_authentication() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    // 关掉重试：500 是可重试的，开着重试就测不出“只打一次”了。
    let provider = provider_without_retry(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let info = provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("公开接口挂了也不该让登录失败");
    assert_eq!(info.version, None);
    assert_eq!(info.name, "测试服务器", "保留本地显示名");
    assert!(info.auth.is_authorized());
    assert_eq!(
        store
            .get("jellyfin/srv-1/token")
            .expect("内存存储不会失败")
            .as_ref()
            .map(SecretString::expose),
        Some(JELLYFIN_TOKEN)
    );
}
