// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 端到端测试：`wiremock` 提供图片，`tempfile` 提供缓存目录，全程不联网。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use nova_images::{
    DecodedImage, FetchError, FetchFuture, Fetcher, HttpFetcher, ImageCache, ImageError,
    ImageRequest, cache_key,
};
use nova_storage::Database;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 生成一张纯渐变 RGBA PNG（尺寸由参数决定）。
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[
                (x % 256) as u8,
                (y % 256) as u8,
                ((x * 3 + y * 7) % 256) as u8,
                255,
            ]);
        }
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
        .expect("测试内编码 PNG 不应失败");
    out
}

fn cache_in(dir: &TempDir, budget_bytes: u64) -> ImageCache {
    let db = Arc::new(Database::open_in_memory().expect("内存库应能打开"));
    ImageCache::new(dir.path().join("images"), db, budget_bytes).expect("缓存目录应能创建")
}

fn fetcher() -> HttpFetcher {
    HttpFetcher::try_default().expect("默认客户端应能构造")
}

/// 可控的慢速下载器：统计调用次数，并延迟返回固定字节。
struct SlowFetcher {
    bytes: Vec<u8>,
    calls: AtomicUsize,
    delay: Duration,
}

impl Fetcher for SlowFetcher {
    fn fetch<'a>(&'a self, _url: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(self.bytes.clone())
        })
    }
}

#[tokio::test]
async fn second_call_hits_disk_without_a_second_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items/1/Images/Primary"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png(64, 48)))
        .mount(&server)
        .await;

    let url = format!(
        "{}/Items/1/Images/Primary?api_key=secret-token",
        server.uri()
    );
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);
    let fetcher = fetcher();

    let first = cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher)
        .await
        .expect("首次应下载成功");
    assert_eq!((first.width, first.height), (64, 48));
    assert_eq!(first.rgba.len(), 64 * 48 * 4);

    let second = cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher)
        .await
        .expect("第二次应命中缓存");
    assert_eq!(second, first);

    assert_eq!(
        server.received_requests().await.expect("请求记录").len(),
        1,
        "第二次应当直接读盘，不再发请求"
    );

    let key = cache_key(&url, None);
    let path = cache.path_for(&key);
    assert!(path.is_file(), "缓存文件应落在分桶目录里");
    assert_eq!(
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str()),
        Some(&key[..2]),
        "分桶目录名应当是键的前两位"
    );
    assert_eq!(
        cache.cached_bytes().expect("索引可读"),
        u64::try_from(png(64, 48).len()).expect("长度转 u64")
    );
}

#[tokio::test]
async fn tag_change_is_a_new_cache_key() {
    let body = png(32, 32);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items/7/Images/Backdrop"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let url = format!("{}/Items/7/Images/Backdrop", server.uri());
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);
    let fetcher = fetcher();

    let with_v1 = ImageRequest::new(&url).with_tag(Some("v1"));
    cache.get_or_fetch(&with_v1, &fetcher).await.expect("首次");
    cache
        .get_or_fetch(&with_v1, &fetcher)
        .await
        .expect("同 tag 应命中");
    assert_eq!(server.received_requests().await.expect("请求记录").len(), 1);

    // 服务端换图后 URL 不变、tag 变了：必须当成另一张图重新下载。
    let with_v2 = ImageRequest::new(&url).with_tag(Some("v2"));
    cache
        .get_or_fetch(&with_v2, &fetcher)
        .await
        .expect("新 tag");
    assert_eq!(
        server.received_requests().await.expect("请求记录").len(),
        2,
        "tag 变化必须重新下载"
    );

    assert_ne!(cache_key(&url, Some("v1")), cache_key(&url, Some("v2")));
    assert!(cache.path_for(&cache_key(&url, Some("v1"))).is_file());
    assert!(cache.path_for(&cache_key(&url, Some("v2"))).is_file());
}

#[tokio::test]
async fn max_width_scales_down_but_does_not_refetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png(400, 300)))
        .mount(&server)
        .await;

    let url = format!("{}/big.png", server.uri());
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);
    let fetcher = fetcher();

    let scaled = cache
        .get_or_fetch(&ImageRequest::new(&url).with_max_width(200), &fetcher)
        .await
        .expect("应下载并缩放");
    assert_eq!((scaled.width, scaled.height), (200, 150), "应等比缩放");
    assert_eq!(scaled.rgba.len(), 200 * 150 * 4);

    // 另一个宽度只影响解码结果，不影响缓存键：不应再发请求。
    let smaller = cache
        .get_or_fetch(&ImageRequest::new(&url).with_max_width(100), &fetcher)
        .await
        .expect("应命中同一份缓存");
    assert_eq!((smaller.width, smaller.height), (100, 75));
    assert_eq!(server.received_requests().await.expect("请求记录").len(), 1);

    // 不给 max_width 就是原图。
    let original = cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher)
        .await
        .expect("应命中同一份缓存");
    assert_eq!((original.width, original.height), (400, 300));
    assert_eq!(server.received_requests().await.expect("请求记录").len(), 1);
}

#[tokio::test]
async fn corrupt_cache_files_are_deleted_and_refetched_once() {
    let body = png(40, 40);
    // 三种"坏缓存"：0 字节、PNG 魔数截断、根本不是图片。
    let corrupt: [&[u8]; 3] = [&[], b"\x89PNG\r\n\x1a\n", b"<html>502 Bad Gateway</html>"];

    for bad in corrupt {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/heal.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let url = format!("{}/heal.png", server.uri());
        let dir = TempDir::new().expect("临时目录");
        let cache = cache_in(&dir, 0);
        let fetcher = fetcher();

        cache
            .get_or_fetch(&ImageRequest::new(&url), &fetcher)
            .await
            .expect("首次应成功");
        let path = cache.path_for(&cache_key(&url, None));
        std::fs::write(&path, bad).expect("写入坏缓存");

        // 坏文件不能让进程 panic，也不能把坏字节当结果返回。
        let healed = cache
            .get_or_fetch(&ImageRequest::new(&url), &fetcher)
            .await
            .expect("坏缓存应被自愈");
        assert_eq!((healed.width, healed.height), (40, 40));
        assert_eq!(
            server.received_requests().await.expect("请求记录").len(),
            2,
            "坏缓存应重取一次（且只重取一次）"
        );
        assert_eq!(
            std::fs::read(&path).expect("缓存文件应已重写"),
            body,
            "重取后应写回完整文件"
        );
    }
}

#[tokio::test]
async fn corrupt_cache_still_fails_when_refetch_also_fails() {
    let body = png(24, 24);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/flaky.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let url = format!("{}/flaky.png", server.uri());
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);
    let fetcher = fetcher();

    cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher)
        .await
        .expect("首次应成功");
    let path = cache.path_for(&cache_key(&url, None));
    std::fs::write(&path, b"garbage").expect("写入坏缓存");

    // 换成必然失败的下载器：坏缓存删掉后重取失败，应把下载错误报出来。
    let failing = FailingFetcher;
    let error = cache
        .get_or_fetch(&ImageRequest::new(&url), &failing)
        .await
        .expect_err("重取失败应报错");
    assert!(matches!(
        error,
        ImageError::Fetch(FetchError::Http { status: 503, .. })
    ));
    assert!(!path.exists(), "失败时坏文件不应留在磁盘上");
}

/// 永远返回 503 的下载器。
struct FailingFetcher;

impl Fetcher for FailingFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        let endpoint = nova_images::endpoint_of(url);
        Box::pin(async move {
            Err(FetchError::Http {
                status: 503,
                endpoint,
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_download_only_once() {
    let dir = TempDir::new().expect("临时目录");
    let cache = Arc::new(cache_in(&dir, 0));
    let fetcher = Arc::new(SlowFetcher {
        bytes: png(32, 32),
        calls: AtomicUsize::new(0),
        delay: Duration::from_millis(100),
    });
    let url = "https://media.example.com/Items/9/Images/Primary?api_key=secret";

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let fetcher = Arc::clone(&fetcher);
        tasks.push(tokio::spawn(async move {
            let request = ImageRequest::new(url);
            cache.get_or_fetch(&request, fetcher.as_ref()).await
        }));
    }

    for task in tasks {
        let image = task
            .await
            .expect("任务不应 panic")
            .expect("每个并发请求都应成功");
        assert_eq!((image.width, image.height), (32, 32));
    }

    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        1,
        "同一 key 的 8 个并发请求只应下载一次"
    );
}

#[tokio::test]
async fn evict_removes_oldest_files_and_reports_freed_bytes() {
    let body = png(50, 50);
    let body_len = u64::try_from(body.len()).expect("长度转 u64");

    let server = MockServer::start().await;
    for n in 1..=3 {
        Mock::given(method("GET"))
            .and(path(format!("/{n}.png")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
    }

    // 预算 = 一张图：三张图必须淘汰两张。
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, body_len);
    let fetcher = fetcher();

    let urls: Vec<String> = (1..=3)
        .map(|n| format!("{}/{n}.png", server.uri()))
        .collect();
    for url in &urls {
        cache
            .get_or_fetch(&ImageRequest::new(url), &fetcher)
            .await
            .expect("下载应成功");
    }
    assert_eq!(cache.cached_bytes().expect("索引可读"), 3 * body_len);

    let paths: Vec<_> = urls
        .iter()
        .map(|url| cache.path_for(&cache_key(url, None)))
        .collect();
    assert!(paths.iter().all(|path| path.is_file()));

    let freed = cache.evict().expect("淘汰应成功");
    assert_eq!(freed, 2 * body_len, "应释放两张图的字节数");
    assert!(!paths[0].exists(), "最旧的缓存文件应被删除");
    assert!(!paths[1].exists(), "第二旧的缓存文件应被删除");
    assert!(paths[2].is_file(), "最新的缓存文件应保留");
    assert!(cache.cached_bytes().expect("索引可读") <= body_len);

    // 保留下来的那张仍然可用，且不需要重新下载。
    let kept = cache
        .get_or_fetch(&ImageRequest::new(&urls[2]), &fetcher)
        .await
        .expect("保留的图片应命中");
    assert_eq!((kept.width, kept.height), (50, 50));
    assert_eq!(
        server.received_requests().await.expect("请求记录").len(),
        3,
        "淘汰后不应触发额外的下载"
    );
}

#[tokio::test]
async fn evict_tolerates_already_missing_files() {
    let body = png(20, 20);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gone.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);
    let fetcher = fetcher();
    let url = format!("{}/gone.png", server.uri());
    cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher)
        .await
        .expect("下载应成功");
    std::fs::remove_file(cache.path_for(&cache_key(&url, None))).expect("手工删文件");

    // 文件没了，淘汰照样成功，只是没有字节可释放。
    assert_eq!(cache.evict().expect("淘汰应成功"), 0);
    assert_eq!(cache.cached_bytes().expect("索引可读"), 0);
}

#[tokio::test]
async fn http_404_reports_status_without_leaking_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing.png"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let url = format!("{}/missing.png?api_key=super-secret", server.uri());
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);

    let error = cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher())
        .await
        .expect_err("404 应报错");
    let message = error.to_string();
    assert!(
        matches!(
            error,
            ImageError::Fetch(FetchError::Http {
                status: 404,
                ref endpoint
            }) if !endpoint.contains("api_key")
        ),
        "404 应当是明确的 HTTP 状态错误：{message}"
    );
    assert!(message.contains("404"), "{message}");
    assert!(!message.contains("api_key"), "{message}");
    assert!(!message.contains("super-secret"), "{message}");
    assert_eq!(cache.cached_bytes().expect("索引可读"), 0, "失败不应入库");
}

#[tokio::test]
async fn non_image_body_is_a_decode_error_without_leaking_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/not-image.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"<html>login</html>".to_vec()))
        .mount(&server)
        .await;

    let url = format!("{}/not-image.png?api_key=super-secret", server.uri());
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);

    let error = cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher())
        .await
        .expect_err("非图片应报错");
    let message = error.to_string();
    assert!(
        matches!(
            error,
            ImageError::Decode {
                bytes: 18,
                ref endpoint,
                ..
            } if !endpoint.contains("api_key")
        ),
        "应当是解码错误并带上字节数：{message}"
    );
    assert!(message.contains("18 字节"), "{message}");
    assert!(!message.contains("api_key"), "{message}");
    assert!(!message.contains("super-secret"), "{message}");
    assert_eq!(cache.cached_bytes().expect("索引可读"), 0, "坏响应不应入库");
}

#[tokio::test]
async fn empty_body_is_reported_separately() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/empty.png"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let url = format!("{}/empty.png?api_key=super-secret", server.uri());
    let dir = TempDir::new().expect("临时目录");
    let cache = cache_in(&dir, 0);

    let error = cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher())
        .await
        .expect_err("空响应应报错");
    assert!(
        matches!(error, ImageError::EmptyBody { ref endpoint } if !endpoint.contains("api_key")),
        "空响应应当是独立的错误：{error}"
    );
    assert!(!error.to_string().contains("super-secret"));
    assert_eq!(cache.cached_bytes().expect("索引可读"), 0);
}

#[tokio::test]
async fn nested_cache_dir_and_bucket_dirs_are_created_automatically() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/auto.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(png(16, 16)))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("临时目录");
    let root = dir.path().join("deep").join("nested").join("images");
    let db = Arc::new(Database::open_in_memory().expect("内存库"));
    let cache = ImageCache::new(root.clone(), db, 0).expect("应自动创建目录");
    assert!(root.is_dir(), "根目录应被创建");

    let url = format!("{}/auto.png", server.uri());
    cache
        .get_or_fetch(&ImageRequest::new(&url), &fetcher())
        .await
        .expect("下载应成功");

    let path = cache.path_for(&cache_key(&url, None));
    assert!(path.is_file());
    assert!(
        root.join(&cache_key(&url, None)[..2]).is_dir(),
        "分桶目录应被创建"
    );
}

#[test]
fn debug_outputs_never_contain_the_query() {
    let url = "https://host/Items/1/Images/Primary?api_key=super-secret";
    let request = ImageRequest::new(url).with_tag(Some("v1"));
    let printed = format!("{request:?}");
    assert!(!printed.contains("api_key"), "{printed}");
    assert!(!printed.contains("super-secret"), "{printed}");
    assert!(
        printed.contains("https://host/Items/1/Images/Primary"),
        "{printed}"
    );

    let image = DecodedImage {
        width: 2,
        height: 2,
        rgba: vec![0; 16],
    };
    assert!(format!("{image:?}").contains("rgba_bytes"), "{image:?}");
}
