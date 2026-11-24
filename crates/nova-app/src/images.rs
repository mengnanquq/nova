// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 图片装载：缓存键 → provider 下载 → 解码 → 推给界面。
//!
//! # 数据流
//!
//! ```text
//! ImageKey（界面要的图）
//!      │ 查 SQLite 里的 MediaItem（是否存在海报/背景图、版本 tag）
//!      ▼
//! ImageSpec（带 tag 的合成 URL）──> ImageCache::get_or_fetch ──> DecodedImage
//!                                        │
//!                                   Fetcher 适配器
//!                                        │
//!                                   provider.image(ImageRequest)
//! ```
//!
//! # 为什么用"合成 URL"当缓存键
//!
//! `nova-images` 只认"一个地址 → 字节"，而带认证头的下载只有 provider 知道怎么做。
//! 因此这里构造一个仅在本进程内使用的 `nova-image://image/<条目>/<种类>?w=<宽度>&tag=<tag>`
//! 作为缓存键与下载请求的描述；[`ProviderFetcher`] 再把它还原成
//! [`nova_provider::ImageRequest`]。合成 URL 里没有令牌，可以安全进日志与 SQLite 索引。
//!
//! **版本 tag** 同时出现在合成 URL 的查询串与 [`ImageRequest::with_tag`] 里：
//! 前者让缓存键随服务端换图而变化，后者让 provider 拼出的真实地址也带 tag。
//!
//! # 并发与失败
//!
//! - 同时最多 [`MAX_CONCURRENT_DOWNLOADS`] 张图在下载（`tokio::sync::Semaphore`）；
//! - 每个 `ImageKey` 只发起一次；失败的图记进 `failed` 集合，之后不再重试，
//!   避免"坏图 → 重试 → 依然坏"的死循环，界面保留首字母占位；
//! - 每次视图请求最多装载 [`MAX_IMAGES_PER_VIEW`] 张，防止一次刷新把整库海报都排进队列。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use nova_core::{ItemId, MediaItem};
use nova_images::{
    DecodedImage, FetchError, FetchFuture, Fetcher, ImageCache, ImageRequest, cache_key,
};
use nova_provider::{
    ImageKind as ProviderImageKind, ImageRequest as ProviderImageRequest, MediaProvider,
    ProviderError,
};
use nova_ui::{ImageKey, ImageKind};
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use url::Url;

use crate::local::Local;

/// 图片缓存默认预算：512 MiB。
pub const IMAGE_CACHE_BUDGET: u64 = 512 * 1024 * 1024;

/// 同时下载的图片数量上限。
pub const MAX_CONCURRENT_DOWNLOADS: usize = 4;

/// 单次视图最多请求的图片数量。
pub const MAX_IMAGES_PER_VIEW: usize = 120;

/// 卡片海报宽度。
pub const POSTER_WIDTH: u32 = 300;

/// hero / 详情背景图宽度。
pub const BACKDROP_WIDTH: u32 = 1280;

const URL_SCHEME: &str = "nova-image";
const URL_HOST: &str = "image";

/// 一张待装载的图片。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSpec {
    /// 界面侧图片键。
    pub key: ImageKey,
    /// 合成缓存地址（不含令牌）。
    pub url: String,
    /// 服务端条目 id（背景图可能属于父条目）。
    pub item_id: String,
    /// 服务端图片种类。
    pub provider_kind: ProviderImageKind,
    /// 目标宽度。
    pub width: u32,
    /// 服务端版本 tag。
    pub tag: Option<String>,
}

impl ImageSpec {
    /// 构造合成请求；`Logo` 在阶段 3 不装载，返回 `None`。
    pub fn new(
        item_id: impl Into<String>,
        kind: ImageKind,
        width: u32,
        tag: Option<String>,
    ) -> Option<Self> {
        let item_id = item_id.into();
        let key = ImageKey::new(item_id.clone(), kind);
        Self::with_key(key, item_id, kind, width, tag)
    }

    fn with_key(
        key: ImageKey,
        item_id: String,
        kind: ImageKind,
        width: u32,
        tag: Option<String>,
    ) -> Option<Self> {
        let provider_kind = provider_kind(kind)?;
        let tag = tag.filter(|tag| !tag.is_empty());
        let url = synthetic_url(&item_id, kind, width, tag.as_deref())?;
        Some(Self {
            key,
            url,
            item_id,
            provider_kind,
            width,
            tag,
        })
    }

    /// 从条目与界面图片键构造；条目没有这张图时返回 `None`（不发注定 404 的请求）。
    pub fn from_key(key: &ImageKey, item: &MediaItem) -> Option<Self> {
        let (url, tag, width) = match key.kind {
            ImageKind::Poster => (
                item.images.poster.as_deref()?,
                item.images.poster_tag.clone(),
                POSTER_WIDTH,
            ),
            ImageKind::Backdrop => (
                item.images.backdrop.as_deref()?,
                item.images.backdrop_tag.clone(),
                BACKDROP_WIDTH,
            ),
            ImageKind::Logo => return None,
        };
        // 背景图经常指向父条目（单集自己的 backdrop 是父剧集的），所以从服务端
        // 图片地址里解析真实条目 id，而不是直接用 key 里的 item_id。
        let item_id = item_id_from_image_url(url).unwrap_or_else(|| key.item_id.clone());
        Self::with_key(key.clone(), item_id, key.kind, width, tag)
    }
}

/// 下载适配器：把合成 URL 还原成 provider 请求。
pub struct ProviderFetcher {
    provider: Mutex<Option<Arc<dyn MediaProvider>>>,
}

impl ProviderFetcher {
    /// 用当前会话的 provider 构造；离线时传 `None`（只会命中磁盘缓存）。
    pub fn new(provider: Option<Arc<dyn MediaProvider>>) -> Self {
        Self {
            provider: Mutex::new(provider),
        }
    }

    /// 换会话后更新 provider（应用内登录成功时调用；`None` = 离线）。
    ///
    /// 下一次图片请求就生效；已失败/已开始的请求不受影响。
    pub fn set_provider(&self, provider: Option<Arc<dyn MediaProvider>>) {
        *lock(&self.provider) = provider;
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let endpoint = nova_images::endpoint_of(url);
        let Some(provider) = lock(&self.provider).clone() else {
            tracing::debug!(endpoint, "离线：图片未命中磁盘缓存，保留占位");
            // `FetchError` 没有"未配置 provider"变体，返回空响应体让缓存层报
            // `EmptyBody`；上层随后把它记成"不重试的失败"。
            return Ok(Vec::new());
        };
        let Some(parsed) = parse_synthetic_url(url) else {
            tracing::warn!(endpoint, "无法解析图片请求，按失败处理");
            return Ok(Vec::new());
        };
        let mut request =
            ProviderImageRequest::new(ItemId::new(parsed.item_id), parsed.provider_kind);
        if let Some(width) = parsed.width {
            request = request.with_width(width);
        }
        if let Some(tag) = parsed.tag {
            request = request.with_tag(tag);
        }
        match provider.image(request).await {
            Ok(bytes) => Ok(bytes),
            Err(error) => match error {
                ProviderError::Unauthorized => Err(FetchError::Http {
                    status: 401,
                    endpoint,
                }),
                ProviderError::NotFound => Err(FetchError::Http {
                    status: 404,
                    endpoint,
                }),
                ProviderError::Http { status } => Err(FetchError::Http { status, endpoint }),
                other => {
                    // provider 的 Network / Decode / Server 等无法构造进 FetchError
                    // （`FetchError::Transport` 需要 `reqwest::Error`，它不可构造）。
                    // 记一条 warn 后返回空体，缓存层按 EmptyBody 处理，界面保留占位。
                    tracing::warn!(endpoint, error = %other, "图片请求失败");
                    Ok(Vec::new())
                }
            },
        }
    }
}

impl Fetcher for ProviderFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        Box::pin(self.download(url))
    }
}

impl std::fmt::Debug for ProviderFetcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderFetcher")
            .field("online", &lock(&self.provider).is_some())
            .finish()
    }
}

/// 图片装载服务：持有缓存、下载器、并发闸门与"已请求/已失败"集合。
pub struct ImageService {
    cache: Arc<ImageCache>,
    fetcher: Arc<dyn Fetcher>,
    permits: Arc<Semaphore>,
    started: Mutex<HashSet<ImageKey>>,
    failed: Mutex<HashSet<ImageKey>>,
}

impl ImageService {
    /// 用磁盘缓存与下载器构造。
    pub fn new(cache: Arc<ImageCache>, fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            cache,
            fetcher,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            started: Mutex::new(HashSet::new()),
            failed: Mutex::new(HashSet::new()),
        }
    }

    /// 磁盘缓存。
    pub fn cache(&self) -> &Arc<ImageCache> {
        &self.cache
    }

    /// 装载一张图。失败会被记录且不再重试。
    pub async fn load(&self, spec: &ImageSpec) -> Option<(ImageKey, DecodedImage)> {
        {
            let failed = lock(&self.failed);
            if failed.contains(&spec.key) {
                return None;
            }
        }
        let permit = self.permits.acquire().await.ok()?;
        let request = ImageRequest::new(&spec.url).with_max_width(spec.width);
        let request = match spec.tag.as_deref() {
            Some(tag) => request.with_tag(Some(tag)),
            None => request,
        };
        let outcome = self
            .cache
            .get_or_fetch(&request, self.fetcher.as_ref())
            .await;
        drop(permit);
        match outcome {
            Ok(image) => Some((spec.key.clone(), image)),
            Err(error) => {
                tracing::warn!(endpoint = nova_images::endpoint_of(&spec.url), %error, "图片装载失败（不重试）");
                lock(&self.failed).insert(spec.key.clone());
                None
            }
        }
    }

    /// 把一组界面图片键排进后台队列，成功时调用 `deliver`。
    ///
    /// 必须从已有 tokio runtime 的上下文调用（窗口模式在 UI 线程调用也安全，
    /// 只是 `spawn` 发生在后台 runtime 上）。
    pub fn request<F>(
        self: &Arc<Self>,
        local: &Arc<Local>,
        keys: Vec<ImageKey>,
        runtime: &Handle,
        deliver: F,
    ) where
        F: Fn(ImageKey, DecodedImage) + Send + Sync + 'static,
    {
        let deliver: Arc<dyn Fn(ImageKey, DecodedImage) + Send + Sync> = Arc::new(deliver);
        for key in keys.into_iter().take(MAX_IMAGES_PER_VIEW) {
            if !self.mark_started(&key) {
                continue;
            }
            let service = Arc::clone(self);
            let local = Arc::clone(local);
            let deliver = Arc::clone(&deliver);
            runtime.spawn(async move {
                let Some(spec) = local.image_spec(&key) else {
                    // 条目暂时没有这张图（例如刚刷新还没写进缓存）：解除标记，
                    // 下次视图重建时可以再试一次。
                    service.unmark(&key);
                    return;
                };
                if let Some((key, decoded)) = service.load(&spec).await {
                    deliver(key, decoded);
                }
            });
        }
    }

    /// 是否已经为这个键发过请求。
    pub fn was_requested(&self, key: &ImageKey) -> bool {
        lock(&self.started).contains(key) || lock(&self.failed).contains(key)
    }

    fn mark_started(&self, key: &ImageKey) -> bool {
        if lock(&self.failed).contains(key) {
            return false;
        }
        lock(&self.started).insert(key.clone())
    }

    fn unmark(&self, key: &ImageKey) {
        lock(&self.started).remove(key);
    }
}

impl std::fmt::Debug for ImageService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageService")
            .field("permits", &MAX_CONCURRENT_DOWNLOADS)
            .field("started", &lock(&self.started).len())
            .field("failed", &lock(&self.failed).len())
            .finish_non_exhaustive()
    }
}

/// 解码结果 → `slint::Image`。**必须在 UI 线程调用**（纹理上传属于界面线程）。
pub fn image_from_decoded(decoded: &DecodedImage) -> Option<slint::Image> {
    use i_slint_core::graphics::{Rgba8Pixel, SharedPixelBuffer};

    if decoded.width == 0 || decoded.height == 0 {
        return None;
    }
    let expected = (decoded.width as usize)
        .checked_mul(decoded.height as usize)?
        .checked_mul(4)?;
    if decoded.rgba.len() != expected {
        tracing::warn!(
            width = decoded.width,
            height = decoded.height,
            bytes = decoded.rgba.len(),
            "解码结果长度与尺寸不一致，丢弃"
        );
        return None;
    }
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &decoded.rgba,
        decoded.width,
        decoded.height,
    );
    Some(slint::Image::from_rgba8(buffer))
}

/// 缓存键（测试与诊断用）。
pub fn key_for(url: &str, tag: Option<&str>) -> String {
    cache_key(url, tag)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn provider_kind(kind: ImageKind) -> Option<ProviderImageKind> {
    match kind {
        ImageKind::Poster => Some(ProviderImageKind::Poster),
        ImageKind::Backdrop => Some(ProviderImageKind::Backdrop),
        ImageKind::Logo => None,
    }
}

fn kind_segment(kind: ImageKind) -> Option<&'static str> {
    match kind {
        ImageKind::Poster => Some("poster"),
        ImageKind::Backdrop => Some("backdrop"),
        ImageKind::Logo => None,
    }
}

fn kind_from_segment(segment: &str) -> Option<(ImageKind, ProviderImageKind)> {
    match segment {
        "poster" => Some((ImageKind::Poster, ProviderImageKind::Poster)),
        "backdrop" => Some((ImageKind::Backdrop, ProviderImageKind::Backdrop)),
        _ => None,
    }
}

/// 构造仅在本进程内使用的合成 URL。
fn synthetic_url(item_id: &str, kind: ImageKind, width: u32, tag: Option<&str>) -> Option<String> {
    let kind = kind_segment(kind)?;
    let mut url = Url::parse(&format!("{URL_SCHEME}://{URL_HOST}/item")).ok()?;
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.push(item_id).push(kind);
    }
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("w", &width.to_string());
        if let Some(tag) = tag.filter(|tag| !tag.is_empty()) {
            query.append_pair("tag", tag);
        }
    }
    Some(url.into())
}

struct ParsedImage {
    item_id: String,
    provider_kind: ProviderImageKind,
    width: Option<u32>,
    tag: Option<String>,
}

fn parse_synthetic_url(url: &str) -> Option<ParsedImage> {
    let url = Url::parse(url).ok()?;
    if url.scheme() != URL_SCHEME {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    if segments.len() != 3 || segments[0] != "item" {
        return None;
    }
    let (_, provider_kind) = kind_from_segment(segments[2])?;
    let mut width = None;
    let mut tag = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "w" => width = value.parse().ok(),
            "tag" => tag = Some(value.into_owned()),
            _ => {}
        }
    }
    Some(ParsedImage {
        item_id: segments[1].to_owned(),
        provider_kind,
        width,
        tag,
    })
}

/// 从服务端图片地址（`.../Items/<id>/Images/<kind>...`）里取条目 id。
fn item_id_from_image_url(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    let index = segments.iter().position(|segment| *segment == "Items")?;
    let id = segments.get(index + 1)?;
    if id.is_empty() {
        None
    } else {
        Some((*id).to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nova_core::{ImageSet, ItemId, MediaItem, MediaKind};
    use nova_images::ImageCache;
    use nova_storage::Database;

    use super::*;

    /// 1×1 PNG：假下载器返回它，用来走通"下载 → 解码 → 落盘"。
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x38,
        0x61, 0xa3, 0xf1, 0x1f, 0x00, 0x05, 0x28, 0x02, 0x2c, 0x27, 0xfa, 0x01, 0xdb, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    struct FakeFetcher {
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeFetcher {
        fn new(fail: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail,
            }
        }
    }

    impl Fetcher for FakeFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail {
                    return Err(FetchError::Http {
                        status: 404,
                        endpoint: nova_images::endpoint_of(url),
                    });
                }
                Ok(PNG_1X1.to_vec())
            })
        }
    }

    fn item_with_images(poster: Option<&str>, backdrop: Option<&str>) -> MediaItem {
        MediaItem {
            id: ItemId::new("item-1"),
            kind: MediaKind::Episode,
            title: "Pilot".to_owned(),
            sort_title: "Pilot".to_owned(),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime: None,
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet {
                poster: poster.map(str::to_owned),
                backdrop: backdrop.map(str::to_owned),
                logo: None,
                thumb: None,
                poster_tag: Some("ptag".to_owned()),
                backdrop_tag: Some("btag".to_owned()),
            },
            user_data: nova_core::UserData::default(),
        }
    }

    #[test]
    fn synthetic_url_roundtrips_item_kind_width_and_tag() {
        let spec = ImageSpec::new(
            "abc",
            ImageKind::Poster,
            POSTER_WIDTH,
            Some("t1".to_owned()),
        )
        .expect("海报请求");
        assert!(spec.url.starts_with("nova-image://image/item/abc/poster"));
        let parsed = parse_synthetic_url(&spec.url).expect("应可解析");
        assert_eq!(parsed.item_id, "abc");
        assert_eq!(parsed.provider_kind, ProviderImageKind::Poster);
        assert_eq!(parsed.width, Some(POSTER_WIDTH));
        assert_eq!(parsed.tag.as_deref(), Some("t1"));
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(parse_synthetic_url("https://host/Items/1/Images/Primary").is_none());
    }

    #[test]
    fn spec_uses_parent_item_for_backdrop_and_carries_tags() {
        let item = item_with_images(
            Some("http://host/Items/own/Images/Primary?tag=ptag"),
            Some("http://host/Items/parent/Images/Backdrop?tag=btag"),
        );
        let poster = ImageSpec::from_key(&ImageKey::poster("item-1"), &item).expect("海报");
        assert_eq!(poster.item_id, "own");
        assert_eq!(poster.tag.as_deref(), Some("ptag"));
        assert_eq!(poster.width, POSTER_WIDTH);

        let backdrop = ImageSpec::from_key(&ImageKey::backdrop("item-1"), &item).expect("背景图");
        assert_eq!(
            backdrop.key,
            ImageKey::backdrop("item-1"),
            "界面键必须是原始条目"
        );
        assert_eq!(backdrop.item_id, "parent");
        assert_eq!(backdrop.tag.as_deref(), Some("btag"));
        assert_eq!(backdrop.width, BACKDROP_WIDTH);
    }

    #[test]
    fn missing_images_are_not_requested() {
        let item = item_with_images(None, None);
        assert!(ImageSpec::from_key(&ImageKey::poster("item-1"), &item).is_none());
        assert!(ImageSpec::from_key(&ImageKey::backdrop("item-1"), &item).is_none());
    }

    #[test]
    fn successful_load_writes_a_cache_file() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("临时目录");
            let db = Arc::new(Database::open_in_memory().expect("内存库"));
            let cache = Arc::new(
                ImageCache::new(
                    dir.path().join("images"),
                    Arc::clone(&db),
                    IMAGE_CACHE_BUDGET,
                )
                .expect("缓存目录"),
            );
            let fetcher = Arc::new(FakeFetcher::new(false));
            let service = ImageService::new(Arc::clone(&cache), fetcher.clone());
            let spec = ImageSpec::new("abc", ImageKind::Poster, POSTER_WIDTH, Some("t".to_owned()))
                .expect("请求");

            let loaded = service.load(&spec).await.expect("应能装载");
            assert_eq!(loaded.0, ImageKey::poster("abc"));
            assert_eq!((loaded.1.width, loaded.1.height), (1, 1));

            let key = cache_key(&spec.url, spec.tag.as_deref());
            assert!(cache.path_for(&key).is_file(), "缓存文件应当落盘");
            assert!(cache.cached_bytes().expect("索引") > 0);
            // 第二次直接命中磁盘缓存，不再下载。
            let again = service.load(&spec).await.expect("应能命中缓存");
            assert_eq!(again.0, ImageKey::poster("abc"));
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn failures_are_not_retried() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("临时目录");
            let db = Arc::new(Database::open_in_memory().expect("内存库"));
            let cache = Arc::new(
                ImageCache::new(dir.path().join("images"), db, IMAGE_CACHE_BUDGET).expect("缓存"),
            );
            let fetcher = Arc::new(FakeFetcher::new(true));
            let service = ImageService::new(cache, fetcher.clone());
            let spec = ImageSpec::new("abc", ImageKind::Poster, POSTER_WIDTH, None).expect("请求");

            assert!(service.load(&spec).await.is_none());
            assert!(service.load(&spec).await.is_none());
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1, "失败后不应重试");
            assert!(service.was_requested(&spec.key));
        });
    }
}
