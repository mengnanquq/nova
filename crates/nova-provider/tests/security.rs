// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 安全相关：地址校验、令牌不进日志/错误、`dyn` 兼容性。
//!
//! 这里的断言都用真实令牌字符串做"反向 grep"：只要格式化输出里出现它就算失败。

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{ITEM_ID, JELLYFIN_TOKEN, USER_ID};
use nova_core::{AuthState, ItemId, ServerId};
use nova_provider::{Flavor, MediaProvider, Provider, ProviderConfig, ProviderError, RetryPolicy};
use nova_secrets::{CredentialStore, MemoryStore, SecretError, SecretString};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn rejects_base_urls_that_are_not_plain_http() {
    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "ftp://nas.local",
        "https://",
        "http://:8096",
        "",
        "   ",
    ] {
        let config = ProviderConfig::new(ServerId::new("srv-1"), bad);
        let error = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
            .expect_err(&format!("{bad:?} 应当构造失败"));
        assert!(
            matches!(error, ProviderError::Unsupported(_)),
            "{bad:?} → {error:?}"
        );
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn rejects_base_urls_with_userinfo() {
    // `http://user:pass@host` 能被 `Url::parse` 接受，但明文密码会进入
    // `ServerInfo.base_url` / 日志 / 界面。宁可直接拒绝。
    for bad in [
        "http://alice:hunter2@192.168.1.10:8096",
        "https://alice:hunter2@media.example.com",
        "https://alice@media.example.com",
    ] {
        let config = ProviderConfig::new(ServerId::new("srv-1"), bad);
        let error = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
            .expect_err(&format!("{bad:?} 必须构造失败"));
        assert!(
            matches!(error, ProviderError::Unsupported(_)),
            "{bad:?} → {error:?}"
        );
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains("hunter2"),
            "错误里出现了口令：{rendered}"
        );
    }
}

#[test]
fn plain_http_is_accepted_but_flagged() {
    let config = ProviderConfig::new(ServerId::new("srv-1"), "http://192.168.1.10:8096");
    let provider = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
        .expect("明文地址是允许的（局域网自建）");
    assert!(provider.is_plaintext());
    assert!(provider.server_info().is_plaintext());

    let config = ProviderConfig::new(ServerId::new("srv-1"), "https://media.example.com");
    let provider =
        Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new())).expect("应当构造");
    assert!(!provider.is_plaintext());
    assert!(!provider.server_info().is_plaintext());
}

fn provider_debug() -> String {
    let config = ProviderConfig::new(ServerId::new("srv-1"), "https://media.example.com");
    let provider =
        Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new())).expect("应当构造");
    format!("{provider:?}")
}

#[test]
fn provider_is_usable_as_a_trait_object() {
    let config = ProviderConfig::new(ServerId::new("srv-1"), "https://media.example.com");
    let provider =
        Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new())).expect("应当构造");
    let boxed: Arc<dyn MediaProvider> = Arc::new(provider);
    assert_eq!(boxed.base_url(), "https://media.example.com");
    assert_eq!(boxed.kind(), nova_core::ServerKind::Jellyfin);
    assert_eq!(
        boxed.server_info().auth,
        AuthState::Unauthorized,
        "没有令牌时应当是未认证"
    );
    // 具体类型的 `Debug` 不泄漏凭证，只暴露键名。
    let concrete = provider_debug();
    assert!(concrete.contains("credential_key"));
    assert!(!concrete.contains("token:"));
}

/// 一个总是失败的凭证存储，用来覆盖 [`ProviderError::Credentials`]。
struct BrokenStore;

impl CredentialStore for BrokenStore {
    fn get(&self, _key: &str) -> Result<Option<SecretString>, SecretError> {
        Err(SecretError::Unavailable("钥匙串被锁住了".to_owned()))
    }

    fn set(&self, _key: &str, _value: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("钥匙串被锁住了".to_owned()))
    }

    fn delete(&self, _key: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("钥匙串被锁住了".to_owned()))
    }
}

#[test]
fn broken_credential_store_is_reported_separately_from_server_errors() {
    let config = ProviderConfig::new(ServerId::new("srv-1"), "https://media.example.com");
    let provider = Provider::new(Flavor::Jellyfin, config, Arc::new(BrokenStore))
        .expect("读不到凭证不该让构造失败");
    // 构造时读失败 → 按未认证处理。
    assert_eq!(provider.server_info().auth, AuthState::Unauthorized);
    assert_eq!(provider.credential_key(), "jellyfin/srv-1/token");
}

#[tokio::test]
async fn storing_the_token_failure_is_a_credentials_error() {
    let server = MockServer::start().await;

    let config = ProviderConfig::new(ServerId::new("srv-1"), server.uri());
    let provider =
        Provider::new(Flavor::Jellyfin, config, Arc::new(BrokenStore)).expect("应当构造");
    common::mount_login(&server, Flavor::Jellyfin).await;

    let error = provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect_err("写不进凭证存储必须报错");
    assert!(
        matches!(error, ProviderError::Credentials { .. }),
        "本机钥匙串故障不能被当成服务端错误：{error:?}"
    );
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(JELLYFIN_TOKEN));
    // 失败时不留部分状态。
    assert_eq!(provider.server_info().auth, AuthState::Unauthorized);
}

#[tokio::test]
async fn errors_never_leak_the_token_or_the_query_string() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = common::provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");
    assert_eq!(
        store
            .get("jellyfin/srv-1/token")
            .expect("内存存储不会失败")
            .as_ref()
            .map(SecretString::expose),
        Some(JELLYFIN_TOKEN)
    );

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items")))
        .respond_with(ResponseTemplate::new(502))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items/{ITEM_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>坏响应</html>"))
        .mount(&server)
        .await;

    let mut errors: Vec<ProviderError> = Vec::new();
    errors.push(provider.libraries().await.expect_err("401"));
    errors.push(
        provider
            .items(nova_provider::ItemQuery::new().paged(0, 50))
            .await
            .expect_err("502"),
    );
    errors.push(
        provider
            .item(&ItemId::new(ITEM_ID))
            .await
            .expect_err("坏 JSON"),
    );

    // 地址不合规
    let mut config = ProviderConfig::new(ServerId::new("srv-1"), "file:///etc/passwd");
    config.base_url = "file:///etc/passwd".to_owned();
    errors.push(
        Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
            .expect_err("非法地址"),
    );

    // 连不上（端口借用后立刻关闭，重试关掉以免拖慢）
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("应当能绑定端口");
    let port = listener.local_addr().expect("应当能取到地址").port();
    drop(listener);
    let dead_store = Arc::new(MemoryStore::new());
    dead_store
        .set("jellyfin/srv-1/token", &SecretString::new(JELLYFIN_TOKEN))
        .expect("内存存储不会失败");
    let config = ProviderConfig::new(ServerId::new("srv-1"), format!("http://127.0.0.1:{port}"))
        .with_display_name("测试");
    let dead = Provider::new(
        Flavor::Jellyfin,
        config,
        dead_store as Arc<dyn CredentialStore>,
    )
    .expect("应当构造")
    .with_retry_policy(RetryPolicy::none())
    .with_request_timeout(Duration::from_millis(200));
    // 令牌在手但连不上：这条错误来自网络层，最容易捎带出完整地址。
    errors.push(
        dead.image(nova_provider::ImageRequest::new(
            ItemId::new(ITEM_ID),
            nova_provider::ImageKind::Poster,
        ))
        .await
        .expect_err("应当连不上"),
    );
    assert!(
        matches!(errors.last(), Some(ProviderError::Network(_))),
        "期望网络错误，实际：{:?}",
        errors.last()
    );

    for error in &errors {
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains(JELLYFIN_TOKEN),
            "错误里出现了令牌：{rendered}"
        );
        assert!(
            !rendered.contains("api_key"),
            "错误里出现了 api_key：{rendered}"
        );
        assert!(
            !rendered.contains("StartIndex"),
            "错误里带上了请求查询串：{rendered}"
        );
    }
}

#[tokio::test]
async fn logout_removes_the_token_from_the_store() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = common::provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");
    assert_eq!(store.len(), 1);

    provider.logout().await.expect("登出应当成功");
    assert!(store.is_empty(), "令牌必须从凭证存储里删掉");
    assert_eq!(provider.server_info().auth, AuthState::Unauthorized);

    // 幂等：再登出一次也成功。
    provider.logout().await.expect("重复登出应当同样成功");
}

#[tokio::test]
async fn stream_target_keeps_the_token_out_of_debug_output() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = common::provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    let target = provider
        .stream_target(&ItemId::new(ITEM_ID), &nova_core::SourceId::new("src-1"))
        .await
        .expect("应当能构造播放地址");
    assert!(target.url().contains(JELLYFIN_TOKEN));
    assert!(!format!("{target:?}").contains(JELLYFIN_TOKEN));
    assert!(!target.to_string().contains(JELLYFIN_TOKEN));
    assert!(format!("{target:?}").contains("?<redacted>"));
}

#[tokio::test]
async fn item_ids_from_the_server_cannot_escape_the_path_prefix() {
    // 恶意服务端把 `Id` 填成 `..`：拼出来的地址会多出一层，可能被反向代理
    // 规范化到上层路径（例如 `/Items/../Images/...`）。
    let server = MockServer::start().await;
    let provider = common::provider(&server, Flavor::Jellyfin, Arc::new(MemoryStore::new()));
    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items/{ITEM_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Id": "..",
            "Name": "恶意条目",
            "Type": "Movie",
            "ImageTags": { "Primary": "tag" }
        })))
        .mount(&server)
        .await;

    let error = provider
        .item(&ItemId::new(ITEM_ID))
        .await
        .expect_err("`..` 条目 id 必须被拒绝");
    assert!(
        matches!(error, ProviderError::Unsupported(_)),
        "实际错误：{error:?}"
    );
    assert!(
        !error.to_string().contains("../"),
        "错误信息里不该出现拼出来的路径：{error}"
    );
}

/// 收集本线程 `tracing` 事件的最小 Subscriber。
///
/// 只为断言"服务端文本不进日志"而存在：所有字段（含 `message`）都被拼成文本存下来。
struct CaptureSubscriber(Arc<Mutex<Vec<String>>>);

#[derive(Default)]
struct FieldCollector(String);

impl tracing::field::Visit for FieldCollector {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(value);
        self.0.push(';');
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(&format!("{value:?}"));
        self.0.push(';');
    }
}

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut collector = FieldCollector::default();
        event.record(&mut collector);
        if let Ok(mut log) = self.0.lock() {
            log.push(collector.0);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[tokio::test]
async fn server_error_text_never_reaches_the_log_or_the_user_message() {
    // 服务端可能把请求地址回显在 Message 里（带 api_key）。日志里只允许出现
    // 状态码与长度；给用户看的文本要把查询串抹掉。
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _guard = tracing::subscriber::set_default(CaptureSubscriber(Arc::clone(&captured)));

    let server = MockServer::start().await;
    let provider = common::provider(&server, Flavor::Jellyfin, Arc::new(MemoryStore::new()));
    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    let leaky = format!("GET {}/Items?api_key={JELLYFIN_TOKEN} 失败", server.uri());
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({ "Message": leaky })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items")))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "Message": leaky })))
        .mount(&server)
        .await;

    let server_error = provider.libraries().await.expect_err("503");
    assert!(matches!(server_error, ProviderError::Http { status: 503 }));
    let client_error = provider
        .items(nova_provider::ItemQuery::new().paged(0, 10))
        .await
        .expect_err("400");

    let log = captured.lock().expect("互斥量不会中毒").join("\n");
    assert!(!log.is_empty(), "应当至少有日志事件被捕获");
    assert!(!log.contains(JELLYFIN_TOKEN), "日志里出现了令牌：{log}");
    assert!(!log.contains("api_key"), "日志里出现了查询串：{log}");
    assert!(
        !log.contains(&server.uri()),
        "日志里出现了服务端回显的地址：{log}"
    );
    assert!(log.contains("503"), "日志应当保留状态码：{log}");
    assert!(log.contains("message_len"), "日志应当只记长度：{log}");

    for error in [&server_error, &client_error] {
        let rendered = format!("{error} {error:?}");
        assert!(
            !rendered.contains(JELLYFIN_TOKEN),
            "错误里出现了令牌：{rendered}"
        );
        assert!(
            !rendered.contains("api_key"),
            "错误里出现了查询串：{rendered}"
        );
    }
}
