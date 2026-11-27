// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 重试与错误映射：用 wiremock 断言**实际发出的请求次数**。
//!
//! 每个用例都显式统计 `received_requests()`，因为"返回了对的结果"不代表
//! "没有多余请求"——重试写错最常见的表现就是悄悄多发几次。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{ITEM_ID, JELLYFIN_TOKEN, USER_ID, json_fixture};
use nova_core::{AuthState, ItemId, PlayMethod, PlaybackProgress, ServerId, SourceId};
use nova_provider::{Flavor, MediaProvider, Provider, ProviderConfig, ProviderError, RetryPolicy};
use nova_secrets::{CredentialStore, MemoryStore, SecretString};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 构造 provider（尚未认证）。
fn build(server: &MockServer, retry: RetryPolicy) -> (Provider, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::new());
    let config =
        ProviderConfig::new(ServerId::new("srv-1"), server.uri()).with_display_name("测试服务器");
    let provider = Provider::new(
        Flavor::Jellyfin,
        config,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
    )
    .expect("wiremock 地址合法")
    .with_retry_policy(retry);
    (provider, store)
}

/// 登录后的 provider。
async fn authorized(server: &MockServer, retry: RetryPolicy) -> Provider {
    let (provider, _store) = build(server, retry);
    common::mount_login(server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");
    provider
}

fn fast_retry(attempts: u32) -> RetryPolicy {
    RetryPolicy::new(attempts).with_delays(Duration::from_millis(1), Duration::from_millis(2))
}

async fn count_requests(server: &MockServer, suffix: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("wiremock 默认记录请求")
        .iter()
        .filter(|request| request.url.path().ends_with(suffix))
        .count()
}

#[tokio::test]
async fn retries_5xx_on_get_until_it_succeeds() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    // 头两次 500，第三次成功。
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json_fixture("views")))
        .expect(1)
        .mount(&server)
        .await;

    let libraries = provider.libraries().await.expect("第三次应当成功");
    assert_eq!(libraries.len(), 2);
    assert_eq!(
        count_requests(&server, "/Views").await,
        3,
        "应当恰好发 3 次请求"
    );
}

#[tokio::test]
async fn gives_up_after_the_configured_attempts() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当失败");
    assert!(
        matches!(error, ProviderError::Http { status: 503 }),
        "实际错误：{error:?}"
    );
    assert!(error.is_transient(), "5xx 对上上层是暂时的");
    assert_eq!(count_requests(&server, "/Views").await, 3, "3 次尝试后放弃");
}

#[tokio::test]
async fn a_single_attempt_policy_does_not_retry() {
    let server = MockServer::start().await;
    let provider = authorized(&server, RetryPolicy::none()).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    assert!(provider.libraries().await.is_err());
    assert_eq!(count_requests(&server, "/Views").await, 1);
}

#[tokio::test]
async fn unauthorized_is_not_retried_and_marks_state_expired() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当报未认证");
    assert!(matches!(error, ProviderError::Unauthorized), "{error:?}");
    assert!(!error.is_transient());
    assert_eq!(
        count_requests(&server, "/Views").await,
        1,
        "认证失败不该重试"
    );
    assert_eq!(
        provider.server_info().auth,
        AuthState::Expired {
            user_name: "alice".to_owned()
        }
    );
}

#[tokio::test]
async fn forbidden_is_not_retried_and_does_not_expire() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当报拒绝访问");
    assert!(
        !matches!(error, ProviderError::Unauthorized),
        "403 不是 401：上层收到 Unauthorized 会标成「登录已过期」：{error:?}"
    );
    assert!(matches!(error, ProviderError::Forbidden), "{error:?}");
    assert!(!error.is_transient(), "权限不足不该延迟重试");
    assert_eq!(count_requests(&server, "/Views").await, 1);
    // 权限不足不等于令牌过期：状态不能被改成 Expired。
    assert!(
        provider.server_info().auth.is_authorized(),
        "403 不该把会话标成过期：{:?}",
        provider.server_info().auth
    );
}

#[tokio::test]
async fn not_found_is_not_retried() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items/{ITEM_ID}")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider
        .item(&ItemId::new(ITEM_ID))
        .await
        .expect_err("应当报 NotFound");
    assert!(matches!(error, ProviderError::NotFound));
    assert_eq!(error.to_string(), "服务端没有该资源");
    assert_eq!(count_requests(&server, ITEM_ID).await, 1);
}

#[tokio::test]
async fn post_is_never_retried() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider
        .report_progress(&PlaybackProgress {
            session_seq: 0,
            play_session_id: None,
            item_id: ItemId::new("item"),
            source_id: SourceId::new("source"),
            position: Duration::from_secs(60),
            duration: Some(Duration::from_secs(3600)),
            played: false,
            paused: false,
            method: PlayMethod::DirectPlay,
        })
        .await
        .expect_err("应当失败");
    assert!(matches!(error, ProviderError::Http { status: 500 }));
    assert_eq!(
        count_requests(&server, "/Sessions/Playing/Progress").await,
        1,
        "上报进度不是幂等操作，绝不能重发"
    );
}

#[tokio::test]
async fn timeouts_are_retried() {
    let server = MockServer::start().await;

    // 预先写入令牌，跳过登录（登录请求同样会超时，那样测的就不是重试了）。
    let store = Arc::new(MemoryStore::new());
    store
        .set("jellyfin/srv-1/token", &SecretString::new(JELLYFIN_TOKEN))
        .expect("内存存储不会失败");
    let config = ProviderConfig::new(ServerId::new("srv-1"), server.uri());
    let provider = Provider::new(Flavor::Jellyfin, config, store as Arc<dyn CredentialStore>)
        .expect("wiremock 地址合法")
        .with_retry_policy(fast_retry(2))
        .with_request_timeout(Duration::from_millis(120));

    // 用 `image()`：它只需要令牌，不需要 user_id（这里没有走登录流程）。
    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/Images/Primary")))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(600)))
        .expect(2)
        .mount(&server)
        .await;

    let error = provider
        .image(nova_provider::ImageRequest::new(
            ItemId::new(ITEM_ID),
            nova_provider::ImageKind::Poster,
        ))
        .await
        .expect_err("应当超时");
    assert!(matches!(error, ProviderError::Network(_)), "{error:?}");
    assert_eq!(error.to_string(), "网络错误：请求超时");
    assert!(error.is_transient());
    assert_eq!(
        count_requests(&server, "/Images/Primary").await,
        2,
        "超时应当按策略重试"
    );
}

#[tokio::test]
async fn server_message_is_surfaced_without_the_token() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({ "Message": "该用户没有访问权限" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当失败");
    assert_eq!(error.to_string(), "服务端错误：该用户没有访问权限");
    assert!(matches!(error, ProviderError::Server { .. }));
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains(JELLYFIN_TOKEN),
        "错误里出现了令牌：{rendered}"
    );
}

#[tokio::test]
async fn five_xx_with_a_message_still_keeps_the_status_for_retry_classification() {
    let server = MockServer::start().await;
    let provider = authorized(&server, RetryPolicy::none()).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({ "Message": "服务暂时不可用" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当失败");
    assert!(
        matches!(error, ProviderError::Http { status: 503 }),
        "5xx 必须保留状态码，实际：{error:?}"
    );
    assert!(error.is_transient(), "503 对上层是暂时的");
}

#[tokio::test]
async fn undecodable_body_is_reported_as_decode_error() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>不是 JSON</html>"))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当失败");
    assert!(matches!(error, ProviderError::Decode(_)), "{error:?}");
    assert!(!error.is_transient(), "解码失败重试也没用");
}

#[tokio::test]
async fn unsupported_item_type_on_single_fetch_is_an_invalid_response() {
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Items/{ITEM_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Id": ITEM_ID,
            "Name": "合辑",
            "Type": "BoxSet"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider
        .item(&ItemId::new(ITEM_ID))
        .await
        .expect_err("未知类型必须报错");
    assert!(
        matches!(error, ProviderError::InvalidResponse(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn too_many_requests_is_retried_and_keeps_the_status() {
    // 429 是可重试的：它不携带"令牌错了"的语义，与服务端临时限流同义。
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(json!({ "Message": "请求过于频繁" })),
        )
        .expect(3)
        .mount(&server)
        .await;

    let error = provider.libraries().await.expect_err("应当失败");
    assert!(
        matches!(error, ProviderError::Http { status: 429 }),
        "429 必须保留状态码，实际：{error:?}"
    );
    assert!(error.is_transient(), "429 对上层是暂时错误");
    assert_eq!(
        count_requests(&server, "/Views").await,
        3,
        "仍受 max_attempts 限制"
    );
}

#[tokio::test]
async fn retry_after_seconds_is_used_as_the_backoff() {
    // `Retry-After: 1`（秒）——默认上限 5s，所以这里会真的等 1 秒。
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(2)).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .expect(2)
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    let error = provider.libraries().await.expect_err("应当失败");
    assert!(matches!(error, ProviderError::Http { status: 429 }));
    assert_eq!(count_requests(&server, "/Views").await, 2);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "Retry-After 说的 1 秒必须真的等：实际 {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn retry_after_on_a_post_is_ignored() {
    // POST 不重试：服务端让等多久都不该重发（上报进度重发会写两遍）。
    let server = MockServer::start().await;
    let provider = authorized(&server, fast_retry(3)).await;

    Mock::given(method("POST"))
        .and(path("/Sessions/Playing/Progress"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider
        .report_progress(&PlaybackProgress {
            session_seq: 0,
            play_session_id: None,
            item_id: ItemId::new("item"),
            source_id: SourceId::new("source"),
            position: Duration::from_secs(60),
            duration: Some(Duration::from_secs(3600)),
            played: false,
            paused: false,
            method: PlayMethod::DirectPlay,
        })
        .await
        .expect_err("应当失败");
    assert!(matches!(error, ProviderError::Http { status: 429 }));
    assert_eq!(
        count_requests(&server, "/Sessions/Playing/Progress").await,
        1
    );
}

/// 带上 `Retry-After` 上限的快速重试策略（上限小于 `Retry-After` 给的值）。
fn capped_retry(attempts: u32, cap: Duration) -> RetryPolicy {
    RetryPolicy::new(attempts)
        .with_delays(Duration::from_millis(1), Duration::from_millis(2))
        .with_retry_after_cap(cap)
}

#[tokio::test]
async fn retry_after_http_date_is_honored_and_capped() {
    // 服务端给的是未来时间（2099 年），解析成功后会被上限截到 80ms，
    // 这样既能证明日期格式被解析了，也不会让测试真的等几十年。
    let server = MockServer::start().await;
    let provider = authorized(&server, capped_retry(2, Duration::from_millis(80))).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "Wed, 21 Oct 2099 07:28:00 GMT"),
        )
        .expect(2)
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    let error = provider.libraries().await.expect_err("应当失败");
    let elapsed = started.elapsed();
    assert!(matches!(error, ProviderError::Http { status: 429 }));
    assert_eq!(count_requests(&server, "/Views").await, 2);
    assert!(
        elapsed >= Duration::from_millis(60),
        "HTTP 日期应当被解析并用于退避（上限 80ms），实际只等了 {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "等待必须被上限截住，实际等了 {elapsed:?}"
    );
}

#[tokio::test]
async fn absurd_retry_after_cannot_stall_the_request() {
    // `Retry-After: 3600` 是服务端的权力，但不是它拖死客户端的权力。
    let server = MockServer::start().await;
    let provider = authorized(&server, capped_retry(2, Duration::from_millis(50))).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3600"))
        .expect(2)
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    let error = provider.libraries().await.expect_err("应当失败");
    let elapsed = started.elapsed();
    assert!(matches!(error, ProviderError::Http { status: 429 }));
    assert!(
        elapsed < Duration::from_secs(2),
        "一小时的 Retry-After 必须被截到上限，实际等了 {elapsed:?}"
    );
}

#[tokio::test]
async fn unparsable_retry_after_falls_back_to_exponential_backoff() {
    let server = MockServer::start().await;
    let provider = authorized(&server, capped_retry(2, Duration::from_millis(50))).await;

    Mock::given(method("GET"))
        .and(path(format!("/Users/{USER_ID}/Views")))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "not-a-date"))
        .expect(2)
        .mount(&server)
        .await;

    let started = std::time::Instant::now();
    let error = provider.libraries().await.expect_err("应当失败");
    assert!(matches!(error, ProviderError::Http { status: 429 }));
    assert_eq!(count_requests(&server, "/Views").await, 2);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "解析失败的 Retry-After 应当被忽略，不该卡住"
    );
}
