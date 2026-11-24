// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 集成测试共用的辅助代码。
//!
//! 注意目录名是 `common/`（子目录而不是 `common.rs`），Cargo 不会把它当成独立的测试目标。

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use nova_core::{ItemId, Language, ServerId, SourceId};
use nova_provider::{Flavor, Provider, ProviderConfig, RetryPolicy};
use nova_secrets::{CredentialStore, MemoryStore};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Jellyfin fixture 里的访问令牌。
pub const JELLYFIN_TOKEN: &str = "8f3a1c9d7e5b2a46c0d9e8f7a6b5c4d3";
/// Emby fixture 里的访问令牌。
pub const EMBY_TOKEN: &str = "d4c3b2a1908f7e6d5c4b3a29180f7e6d";
/// Jellyfin fixture 里认证后的用户标识。
pub const USER_ID: &str = "9c8b7a6d5e4f4a3b2c1d0e9f8a7b6c5d";
/// Emby fixture 里认证后的用户标识。
pub const EMBY_USER_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
/// 电影条目 id。
pub const ITEM_ID: &str = "3b6f1d2c8a9e4f5b8c7d1e2f3a4b5c6d";
/// 媒体源 id。
pub const SOURCE_ID: &str = "7d8e9f0a1b2c3d4e5f60718293a4b5c6";

/// 读取 fixture 原文。
pub fn fixture(name: &str) -> String {
    match name {
        "auth_jellyfin" => include_str!("../fixtures/auth_jellyfin.json").to_owned(),
        "auth_emby" => include_str!("../fixtures/auth_emby.json").to_owned(),
        "views" => include_str!("../fixtures/views_jellyfin.json").to_owned(),
        "items_movies" => include_str!("../fixtures/items_movies.json").to_owned(),
        "item_movie" => include_str!("../fixtures/item_movie.json").to_owned(),
        "item_episode" => include_str!("../fixtures/item_episode.json").to_owned(),
        "playback_info" => include_str!("../fixtures/playback_info.json").to_owned(),
        other => panic!("未知 fixture：{other}"),
    }
}

/// 读取 fixture 成 JSON 值。
pub fn json_fixture(name: &str) -> serde_json::Value {
    serde_json::from_str(&fixture(name)).expect("fixture 应当是合法 JSON")
}

/// 认证头匹配器。
///
/// 不能直接用 wiremock 的 `header()`：它会把收到的值按 `,` 切开再逐段比较，
/// 而 `MediaBrowser Client="Nova", Device="…"` 这种格式天然带逗号，
/// 于是永远匹配不上。这里比较的是整条头的原始值。
pub struct AuthHeader {
    name: &'static str,
    expected: String,
}

impl AuthHeader {
    /// 构造匹配器。
    pub fn new(name: &'static str, token: Option<&str>) -> Self {
        Self {
            name,
            expected: media_browser_header(token),
        }
    }
}

impl wiremock::Match for AuthHeader {
    fn matches(&self, request: &wiremock::Request) -> bool {
        request
            .headers
            .get(self.name)
            .and_then(|value| value.to_str().ok())
            == Some(self.expected.as_str())
    }
}

/// 认证头匹配器（便捷函数）。
pub fn auth_header(name: &'static str, token: Option<&str>) -> AuthHeader {
    AuthHeader::new(name, token)
}

/// 认证头里应当出现的固定部分（不含令牌）。
pub fn media_browser_header(token: Option<&str>) -> String {
    let base = format!(
        "MediaBrowser Client=\"{}\", Device=\"Nova\", DeviceId=\"nova-srv-1\", Version=\"{}\"",
        nova_provider::CLIENT_NAME,
        nova_provider::CLIENT_VERSION,
    );
    match token {
        Some(token) => format!("{base}, Token=\"{token}\""),
        None => base,
    }
}

/// 构造一个指向 wiremock 服务器的 provider。
pub fn provider(server: &MockServer, flavor: Flavor, store: Arc<MemoryStore>) -> Provider {
    let config = ProviderConfig::new(ServerId::new("srv-1"), server.uri())
        .with_display_name("测试服务器")
        .with_metadata_language(Language::new("zh-CN"));
    Provider::new(flavor, config, store as Arc<dyn CredentialStore>)
        .expect("wiremock 地址合法")
        .with_retry_policy(
            RetryPolicy::new(3).with_delays(Duration::from_millis(1), Duration::from_millis(2)),
        )
}

/// 不重试的 provider（断言"没有重试"时用）。
pub fn provider_without_retry(
    server: &MockServer,
    flavor: Flavor,
    store: Arc<MemoryStore>,
) -> Provider {
    provider(server, flavor, store).with_retry_policy(RetryPolicy::none())
}

/// 挂载登录接口。
pub async fn mount_login(server: &MockServer, flavor: Flavor) {
    let (body_field, fixture_name) = match flavor {
        Flavor::Jellyfin => ("Pw", "auth_jellyfin"),
        Flavor::Emby => ("Password", "auth_emby"),
    };
    let mut body = serde_json::json!({ "Username": "tester" });
    body[body_field] = serde_json::Value::String("correct horse".to_owned());

    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .and(auth_header(flavor.auth_header_name(), None))
        .and(wiremock::matchers::body_partial_json(body))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture(fixture_name)))
        .expect(1)
        .mount(server)
        .await;
}

/// 媒体库列表接口。
pub async fn mount_views(server: &MockServer, token: &str, flavor: Flavor) {
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .and(auth_header(flavor.auth_header_name(), Some(token)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(1)
        .mount(server)
        .await;
}

/// 条目 id 便捷构造。
pub fn item_id() -> ItemId {
    ItemId::new(ITEM_ID)
}

/// 媒体源 id 便捷构造。
pub fn source_id() -> SourceId {
    SourceId::new(SOURCE_ID)
}
