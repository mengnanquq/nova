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
//! - 每个 `ImageKey` 只发起一次；**永久**失败（404、解码错、空体）记进 `failed`
//!   集合，之后不再重试，避免“坏图 → 重试 → 依然坏”的死循环，界面保留首字母占位；
//!   临时失败（没有会话 / 网络 / 超时 / 5xx / 429）只 `forget` 本次请求，下次
//!   进入视口（或会话恢复在线）时再试（见 [`is_retryable`]）；会话恢复在线时
//!   [`ImageService::clear_failures`] 会把永久失败记录也清掉（重新登录后海报要能回来）；
//! - **按可见窗口请求**：界面把"当前视口内（± 缓冲行）的卡片"作为一批上报
//!   （[`ImageScope`]），滚动时补发新进入视口的键；还没开始下载就滚出视口的
//!   请求会被丢弃（[`RequestState::cancel_if_unwanted`]），所以快速滚动不会在
//!   队列里堆几千张图；已装载的图不会重复请求；
//! - **离开页面就释放可见窗口**（[`ImageService::release_scope`]）：`visible` 里的
//!   集合与"已请求"标记一起解除，这些键不再被 `ImageStore::trim` 当成保护对象，
//!   重新进入页面时按磁盘缓存重新装载（命中成本很低）；
//! - [`MAX_PREFETCH`] 只约束"首屏预热"这类一次性预取，不再限制整页图片；预取
//!   走**独立的** [`MAX_CONCURRENT_PREFETCH`] 名额池，永远不会占住可见窗口的
//!   下载名额（以前两者共用 4 个名额，用户打开大库时首屏海报排在 120 个预取后面）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use nova_core::{ItemId, MediaItem};
use nova_images::{
    DecodedImage, FetchError, FetchFuture, Fetcher, ImageCache, ImageError, ImageRequest, cache_key,
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

/// 一次性预取（首屏预热）最多请求的图片数量。
///
/// 可见窗口之外的图由视口上报补发（见 [`ImageScope`]），这里只是让"刷新 / 切库
/// 落地之前"先有一批图在路上；上限防止一次刷新把整库 4546 张海报都排进队列。
pub const MAX_PREFETCH: usize = 120;

/// 预取专用的并发名额。
///
/// 与可见窗口的 [`MAX_CONCURRENT_DOWNLOADS`] **分开计**：预取再慢也只占 1 个下载位，
/// 用户随后打开大库时首屏海报用可见窗口的 4 个名额，不会被 119 个排队中的预取
/// 堵在队头（体检 R6：预取与可见窗口共用一个 `Semaphore(4)` 时的队头阻塞）。
pub const MAX_CONCURRENT_PREFETCH: usize = 1;

/// 界面图片表（已解码的 `slint::Image`）的字节预算。
///
/// 依据：一张 300 宽的 2:3 海报解码后是 300×450×4 ≈ 0.51 MiB（真实库实测
/// 每进入一个新窗口 RSS 约 +0.5 MiB/张）；海外电影 4546 张全留在内存里 ≈ 2.3 GiB。
/// 64 MiB ≈ 125 张海报，是"一屏网格（42 张）+ 首页两三行（~30 张）+ 详情背景
/// 图（1280 宽 ≈ 3.5 MiB）"的 2–3 倍余量：滚回去一两屏直接复用，再远重读磁盘
/// 缓存。在屏上的键由 `ImageStore::trim` 的 `protect` 保护，不会当场变骨骸。
pub const IMAGE_STORE_BUDGET: u64 = 64 * 1024 * 1024;

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
    /// 每次请求时求值：窗口模式按 [`Local`] 的**即时**状态取 provider，
    /// 快照 / 测试用固定 provider。
    source: ProviderSource,
}

/// 取当前可用 provider 的来源（离线时返回 `None`）。
type ProviderSource = Arc<dyn Fn() -> Option<Arc<dyn MediaProvider>> + Send + Sync>;

impl ProviderFetcher {
    /// 用一个固定的 provider 构造；离线时传 `None`（只会命中磁盘缓存）。
    pub fn new(provider: Option<Arc<dyn MediaProvider>>) -> Self {
        let provider = Mutex::new(provider);
        Self {
            source: Arc::new(move || lock(&provider).clone()),
        }
    }

    /// 跟着 [`Local`] 的状态走：每次取图都重新问一次，登录 / 刷新后立刻生效。
    ///
    /// 以前 fetcher 是启动时一次性构造的：`NOVA_CREDENTIALS=memory` 启动时没有
    /// 令牌，之后应用内登录成功也不会换上新 provider，海报再也下载不下来。
    pub fn from_local(local: Arc<Local>) -> Self {
        Self {
            source: Arc::new(move || {
                crate::ui::online_provider(&local.status(), local.provider_dyn())
            }),
        }
    }

    fn provider(&self) -> Option<Arc<dyn MediaProvider>> {
        (self.source)()
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let endpoint = nova_images::endpoint_of(url);
        let Some(provider) = self.provider() else {
            // 离线 / 未登录：磁盘缓存在这之前已经查过了，这次只是跳过。
            tracing::debug!(endpoint, "没有可用的会话：跳过本次图片请求（不记为失败）");
            return Err(FetchError::Unavailable);
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
                // 本机没有令牌：请求根本没发出去，不是服务端 401。
                ProviderError::NoToken => {
                    tracing::debug!(endpoint, "本机没有令牌：跳过本次图片请求（不记为失败）");
                    Err(FetchError::Unavailable)
                }
                ProviderError::Unauthorized => Err(FetchError::Http {
                    status: 401,
                    endpoint,
                }),
                // 403 是“没权限”，不是“令牌过期”：只保留真实状态码，不谎报成 401。
                ProviderError::Forbidden => Err(FetchError::Http {
                    status: 403,
                    endpoint,
                }),
                ProviderError::NotFound => Err(FetchError::Http {
                    status: 404,
                    endpoint,
                }),
                ProviderError::Http { status } => Err(FetchError::Http { status, endpoint }),
                // 网络 / 超时：本次跳过，下次视口（或会话恢复）再试。
                // （以前这里走 `Ok(Vec::new())` → `EmptyBody` → 永久记进 `failed`，
                // 断网一次之后整屏海报就再也不请求了。）
                ProviderError::Network(_) => Err(FetchError::Transient { endpoint }),
                other => {
                    // provider 的 Decode / Server 等无法构造进 FetchError
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
            .field("online", &self.provider().is_some())
            .finish()
    }
}

/// 图片请求的来源（页面 / 首页行）。
///
/// 可见窗口按来源分别记录：同一来源刷新窗口时替换自己的集合，其它来源不受影响
/// （首页逐行上报，各库行的可见区间互不覆盖）。取消判定看所有来源的**并集**。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ImageScope {
    /// 首页 hero（背景图 + 海报兜底）。
    HomeHero,
    /// 首页横向行；`0` = 继续观看，`1` = 最近添加，`2 + n` = 第 n 个库行。
    HomeRow(usize),
    /// 媒体库网格。
    Library,
    /// 搜索页网格。
    Search,
    /// 详情页（背景图 + 海报）。
    Detail,
}

/// "已请求 / 已失败 / 当前可见"三份状态。
///
/// 逻辑全部与 tokio 无关，单独抽出来是为了能直接对"可见窗口 → 需要请求的键"
/// 做确定性测试（不需要 runtime / 数据库）。
#[derive(Default)]
struct RequestState {
    /// 已发起（在途或已完成）的键。
    started: HashSet<ImageKey>,
    /// 装载失败、不再重试的键。
    failed: HashSet<ImageKey>,
    /// 各来源最后一次上报的可见键。
    visible: HashMap<ImageScope, HashSet<ImageKey>>,
    /// 只被预取排队（尚未开始、也还没被可见窗口要）的键。
    ///
    /// `begin_window` 遇到这里的键会把它**升级**为可见窗口请求：预取只有 1 个名额，
    /// 如果可见键排在 100 个预取后面就会被队头阻塞（预取与可见窗口共用 `started`
    /// 标记，光看"已请求"无法区分优先级）。
    prefetched: HashSet<ImageKey>,
}

impl RequestState {
    /// 释放某个来源的可见窗口（页面离屏 / 路由切走）。
    ///
    /// 只有**没有别的来源还在要**这些键时才解除"已请求"标记：首页 hero 与卡片可能
    /// 同键，详情页与搜索页也可能同时可见。解除标记后重新进入页面会重新上报，
    /// 按磁盘缓存装载（不重复下载）。返回真正被释放（不再受任何来源保护）的键。
    fn release(&mut self, scope: ImageScope) -> Vec<ImageKey> {
        let Some(keys) = self.visible.remove(&scope) else {
            return Vec::new();
        };
        let released: Vec<ImageKey> = keys
            .into_iter()
            .filter(|key| !self.is_wanted(key))
            .collect();
        for key in &released {
            self.started.remove(key);
            self.prefetched.remove(key);
        }
        released
    }

    /// 释放所有首页行的可见窗口（hero + 继续观看 / 最近添加 / 各库行）。
    fn release_home_rows(&mut self) -> Vec<ImageKey> {
        let scopes: Vec<ImageScope> = self
            .visible
            .keys()
            .copied()
            .filter(|scope| matches!(scope, ImageScope::HomeHero | ImageScope::HomeRow(_)))
            .collect();
        scopes
            .into_iter()
            .flat_map(|scope| self.release(scope))
            .collect()
    }
}

impl RequestState {
    /// 记录某个来源的可见窗口，返回其中**需要新发请求**的键（去重，保持顺序）。
    ///
    /// 已经在途 / 已装载 / 已失败的键不会重复请求；同一次上报里的重复键只算
    /// 一次（首页 hero 的背景图与海报是两张不同的图，这里的去重只是防御）。
    /// **只被预取排队的键会升级为可见窗口请求**：预取名额只有 1 个，可见键不能
    /// 排在预取队列后面（返回的 `fresh` 会交给可见窗口的 `load_window`）。
    fn begin_window(&mut self, scope: ImageScope, keys: &[ImageKey]) -> Vec<ImageKey> {
        let mut wanted = HashSet::with_capacity(keys.len());
        let mut fresh = Vec::new();
        for key in keys {
            if !wanted.insert(key.clone()) {
                continue;
            }
            if self.failed.contains(key) {
                continue;
            }
            if self.started.insert(key.clone()) {
                fresh.push(key.clone());
            } else if self.prefetched.remove(key) {
                // 之前只是预取排队（`started` 已置位）：升级为可见窗口请求，
                // `started` 不用再插一次，但必须真的 spawn 一个可见任务。
                fresh.push(key.clone());
            }
        }
        self.visible.insert(scope, wanted);
        fresh
    }

    /// 这个键是否还在某个来源的可见窗口里。
    fn is_wanted(&self, key: &ImageKey) -> bool {
        self.visible.values().any(|keys| keys.contains(key))
    }

    /// 任务被取消（滚出视口、还没开始下载）。
    ///
    /// 只有确实没人再要它时才解除"已请求"标记，否则用户滚回来时这张图会永远
    /// 不再被请求。返回是否真的取消了。
    fn cancel_if_unwanted(&mut self, key: &ImageKey) -> bool {
        if self.is_wanted(key) {
            return false;
        }
        self.started.remove(key);
        self.prefetched.remove(key);
        true
    }

    /// 解除"已请求"标记（例如界面的图片表把它淘汰了，需要能重新装载）。
    fn forget(&mut self, key: &ImageKey) {
        self.started.remove(key);
        self.prefetched.remove(key);
    }
}

/// 一次装载的来源：可见窗口（可取消、走 `permits`）或预取（走独立名额、按代次作废）。
#[derive(Clone, Copy)]
enum SpawnMode {
    Window,
    Prefetch { epoch: u64 },
}

/// 图片装载服务：持有缓存、下载器、并发闸门与请求状态。
pub struct ImageService {
    cache: Arc<ImageCache>,
    fetcher: Arc<dyn Fetcher>,
    /// 可见窗口的下载名额。
    permits: Arc<Semaphore>,
    /// 预取专用名额（与 `permits` 分开，见 [`MAX_CONCURRENT_PREFETCH`]）。
    prefetch_permits: Arc<Semaphore>,
    /// 预取代次：离开首页时 +1，排队中、还没开始的预取发现代次变了就丢弃。
    prefetch_epoch: AtomicU64,
    requests: Mutex<RequestState>,
}

impl ImageService {
    /// 用磁盘缓存与下载器构造。
    pub fn new(cache: Arc<ImageCache>, fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            cache,
            fetcher,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            prefetch_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_PREFETCH)),
            prefetch_epoch: AtomicU64::new(0),
            requests: Mutex::new(RequestState::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, RequestState> {
        lock(&self.requests)
    }

    /// 磁盘缓存。
    pub fn cache(&self) -> &Arc<ImageCache> {
        &self.cache
    }

    /// 装载一张图。失败会被记录且不再重试。
    pub async fn load(&self, spec: &ImageSpec) -> Option<(ImageKey, DecodedImage)> {
        if self.state().failed.contains(&spec.key) {
            return None;
        }
        let permit = self.permits.acquire().await.ok()?;
        self.fill(spec, permit).await
    }

    /// 可见窗口里的装载：**拿到并发名额后**再确认一次仍然在视口内。
    ///
    /// 排队期间用户可能已经滑走（滚动的一致性保证），这时丢弃请求并解除标记，
    /// 以免快速滚动时把已经没人看的图一张张下载完。
    async fn load_window(&self, spec: &ImageSpec) -> Option<(ImageKey, DecodedImage)> {
        if self.state().failed.contains(&spec.key) {
            return None;
        }
        let permit = self.permits.acquire().await.ok()?;
        if self.state().cancel_if_unwanted(&spec.key) {
            return None;
        }
        self.fill(spec, permit).await
    }

    /// 预取装载：先拿**预取专用**名额，拿到后再确认页面还在（代次没变）。
    ///
    /// 与 [`Self::load_window`] 的区别是名额池不同：即使 [`MAX_PREFETCH`] 个预取
    /// 全在排队，可见窗口的请求仍然能用满 [`MAX_CONCURRENT_DOWNLOADS`] 个名额。
    async fn load_prefetch(
        &self,
        spec: &ImageSpec,
        epoch: u64,
    ) -> Option<(ImageKey, DecodedImage)> {
        if self.state().failed.contains(&spec.key) {
            return None;
        }
        let permit = self.prefetch_permits.acquire().await.ok()?;
        if self.prefetch_epoch.load(Ordering::SeqCst) != epoch {
            // 排队期间已经离开首页：这张图不再需要，解除标记让下次进首页重新上报。
            self.state().forget(&spec.key);
            return None;
        }
        if !self.state().prefetched.contains(&spec.key) {
            // 排队期间已经被可见窗口升级（`begin_window` 已经把 `prefetched` 拿掉，
            // `started` 仍置位）：可见任务会负责这张图，预取直接退出。
            return None;
        }
        let loaded = self.fill(spec, permit).await;
        // 预取结束（成功或失败）后都不再是"排队中的预取"了。
        self.state().prefetched.remove(&spec.key);
        loaded
    }

    /// 已持名额时的真实装载（单张图不会在 `load` 与 `fill` 之间被重复请求）。
    async fn fill(
        &self,
        spec: &ImageSpec,
        permit: tokio::sync::SemaphorePermit<'_>,
    ) -> Option<(ImageKey, DecodedImage)> {
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
            // 临时失败（没有会话 / 网络 / 超时 / 5xx）：只是这次没联网，不是“这张图
            // 坏了”。解除已请求标记，下次进入视口（或会话恢复在线）还会重新尝试。
            Err(error) if is_retryable(&error) => {
                tracing::debug!(
                    endpoint = nova_images::endpoint_of(&spec.url),
                    %error,
                    "图片本次跳过，恢复后重试"
                );
                self.state().forget(&spec.key);
                None
            }
            Err(error) => {
                tracing::warn!(endpoint = nova_images::endpoint_of(&spec.url), %error, "图片装载失败（不重试）");
                self.state().failed.insert(spec.key.clone());
                None
            }
        }
    }

    /// 清空永久失败记录，让之前失败的图重新可请求。
    ///
    /// 只在会话恢复在线时调用（`ui::Shell::apply_status`）：`failed` 的本意是
    /// “这张图坏了、不重试”，但它记不出“当时没有网络 / 没有权限”。不清理的话，
    /// 断网一次或重新登录之后，那些海报永远停在首字母占位。
    pub fn clear_failures(&self) {
        let mut state = self.state();
        let failed: Vec<ImageKey> = state.failed.drain().collect();
        for key in &failed {
            // 连“已请求”标记一起解除：否则重新上报可见窗口时 `begin_window` 看到
            // `started` 已有标记，不会真的重发下载。
            state.started.remove(key);
            state.prefetched.remove(key);
        }
        if !failed.is_empty() {
            tracing::debug!(keys = failed.len(), "会话恢复在线，清空图片失败记录");
        }
    }

    /// 上报某个来源当前的可见窗口：新进入视口的键排进后台队列，成功时调用 `deliver`。
    ///
    /// 滚出视口且**尚未开始下载**的键会被丢弃（见 [`RequestState::cancel_if_unwanted`]），
    /// 因此在途任务数始终受并发闸门约束，快速滚动不会堆积几千个排队任务。
    ///
    /// 必须从已有 tokio runtime 的上下文调用（窗口模式在 UI 线程调用也安全，
    /// 只是 `spawn` 发生在后台 runtime 上）。
    pub fn request_window<F>(
        self: &Arc<Self>,
        local: &Arc<Local>,
        scope: ImageScope,
        keys: &[ImageKey],
        runtime: &Handle,
        deliver: F,
    ) where
        F: Fn(ImageKey, DecodedImage) + Send + Sync + 'static,
    {
        let fresh = self.state().begin_window(scope, keys);
        self.spawn(fresh, local, runtime, deliver, SpawnMode::Window);
    }

    /// 一次性预取（首屏预热）：不参与可见窗口，按 [`MAX_PREFETCH`] 截断。
    ///
    /// 预取不做"滚出视口就取消"：调用点发生在视图落地之前，这时还没有可见窗口
    /// 可以解释这批键。上限保证代价可控，剩余的图由视口上报补发。
    pub fn request_prefetch<F>(
        self: &Arc<Self>,
        local: &Arc<Local>,
        keys: Vec<ImageKey>,
        runtime: &Handle,
        deliver: F,
    ) where
        F: Fn(ImageKey, DecodedImage) + Send + Sync + 'static,
    {
        let mut seen = HashSet::new();
        let mut fresh = Vec::new();
        {
            let mut state = self.state();
            for key in keys {
                if !seen.insert(key.clone()) || state.failed.contains(&key) {
                    continue;
                }
                if state.started.insert(key.clone()) {
                    state.prefetched.insert(key.clone());
                    fresh.push(key);
                }
                if fresh.len() >= MAX_PREFETCH {
                    break;
                }
            }
        }
        let epoch = self.prefetch_epoch.load(Ordering::SeqCst);
        self.spawn(
            fresh,
            local,
            runtime,
            deliver,
            SpawnMode::Prefetch { epoch },
        );
    }

    /// 作废所有还没开始的预取（离开首页时调用）。
    ///
    /// 已经在下载的那一张会正常完成（没法中途取消网络请求）；排队中的会在拿到
    /// 名额后发现代次变了，直接丢弃并解除"已请求"标记。
    pub fn cancel_prefetch(&self) {
        self.prefetch_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// 释放一个来源的可见窗口（页面离屏 / 路由切走）。
    ///
    /// 释放后这些键不再出现在 [`Self::visible_keys`] 里，`ImageStore::trim` 不会再
    /// 把它们当保护对象；`cancel_if_unwanted` 也会丢弃还没开始的在途请求。
    pub fn release_scope(&self, scope: ImageScope) {
        let released = self.state().release(scope);
        if !released.is_empty() {
            tracing::debug!(?scope, keys = released.len(), "释放页面可见图片窗口");
        }
    }

    /// 释放所有首页行（hero + 继续观看 / 最近添加 / 各库行）的可见窗口。
    pub fn release_home_rows(&self) {
        let released = self.state().release_home_rows();
        if !released.is_empty() {
            tracing::debug!(keys = released.len(), "释放首页可见图片窗口");
        }
    }

    fn spawn<F>(
        self: &Arc<Self>,
        keys: Vec<ImageKey>,
        local: &Arc<Local>,
        runtime: &Handle,
        deliver: F,
        mode: SpawnMode,
    ) where
        F: Fn(ImageKey, DecodedImage) + Send + Sync + 'static,
    {
        let deliver: Arc<dyn Fn(ImageKey, DecodedImage) + Send + Sync> = Arc::new(deliver);
        for key in keys {
            let service = Arc::clone(self);
            let local = Arc::clone(local);
            let deliver = Arc::clone(&deliver);
            runtime.spawn(async move {
                match mode {
                    SpawnMode::Window => {
                        if service.state().cancel_if_unwanted(&key) {
                            return;
                        }
                        let Some(spec) = local.image_spec(&key) else {
                            // 条目暂时没有这张图（例如刚刷新还没写进缓存）：解除标记，
                            // 下次视图重建时可以再试一次。
                            service.state().forget(&key);
                            return;
                        };
                        if let Some((key, decoded)) = service.load_window(&spec).await {
                            deliver(key, decoded);
                        }
                    }
                    SpawnMode::Prefetch { epoch } => {
                        let Some(spec) = local.image_spec(&key) else {
                            service.state().forget(&key);
                            return;
                        };
                        if let Some((key, decoded)) = service.load_prefetch(&spec, epoch).await {
                            deliver(key, decoded);
                        }
                    }
                }
            });
        }
    }

    /// 是否已经为这个键发过请求。
    pub fn was_requested(&self, key: &ImageKey) -> bool {
        let state = self.state();
        state.started.contains(key) || state.failed.contains(key)
    }

    /// 当前所有来源可见窗口的并集（界面图片表做淘汰时用它保护在屏上的图）。
    pub fn visible_keys(&self) -> HashSet<ImageKey> {
        let state = self.state();
        state
            .visible
            .values()
            .flat_map(|keys| keys.iter().cloned())
            .collect()
    }

    /// 解除"已请求"标记，让淘汰掉的图在重新进入视口时能再次装载。
    pub fn forget(&self, keys: &[ImageKey]) {
        let mut state = self.state();
        for key in keys {
            state.forget(key);
        }
    }
}

impl std::fmt::Debug for ImageService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageService")
            .field("permits", &MAX_CONCURRENT_DOWNLOADS)
            .field("prefetch_permits", &MAX_CONCURRENT_PREFETCH)
            .field("started", &self.state().started.len())
            .field("failed", &self.state().failed.len())
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

/// 这次失败是否只是“暂时的”（可以重试），而不是“这张图坏了”。
///
/// 判据落在错误本身而不是下载器实现上：`ProviderFetcher` 与 `HttpFetcher` 都
/// 可能产生网络 / 5xx，把判据放在这里就不必让每个下载器各写一遍。
fn is_retryable(error: &ImageError) -> bool {
    match error {
        ImageError::Fetch(FetchError::Unavailable | FetchError::Transient { .. }) => true,
        ImageError::Fetch(FetchError::Transport { .. }) => true,
        ImageError::Fetch(FetchError::Http { status, .. }) => *status >= 500 || *status == 429,
        _ => false,
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

    use nova_core::{
        AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo,
        ServerKind,
    };
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

    /// 本地没有令牌时，图片请求根本没发出去（provider 直接报未认证）。
    ///
    /// 以前这一行被当成服务端 401：图片进 `failed` 集合，之后**永远不再重试**，
    /// 用户重新登录、换了令牌也看不到海报。没有令牌属于「本次跳过」，不是失败。
    #[test]
    fn a_missing_token_skips_the_image_instead_of_failing_it() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("临时目录");
            let server_id = "srv-image-offline";
            {
                let db = Database::open(dir.path().join("nova.db")).expect("建库");
                db.upsert_server(&offline_server_info(server_id), 1)
                    .expect("写服务器");
                let mut item =
                    item_with_images(Some("http://host/Items/m1/Images/Primary?tag=t"), None);
                item.id = ItemId::new("m1");
                item.kind = MediaKind::Movie;
                db.upsert_items(&ServerId::new(server_id), &[item], 1)
                    .expect("写条目");
            }
            let env = crate::env::Env {
                data_dir: dir.path().to_path_buf(),
                credentials: crate::env::CredentialMode::Memory,
            };
            let local = Arc::new(Local::open(&env, None).expect("打开状态目录"));
            assert!(
                matches!(local.status(), crate::status::Status::MissingToken { .. }),
                "没有令牌应当是 MissingToken：{:?}",
                local.status()
            );
            assert!(
                local.provider_dyn().is_some(),
                "provider 的构造不要求有令牌；门控在下载这一层"
            );
            let service = ImageService::new(
                Arc::clone(&local.image_cache),
                Arc::new(ProviderFetcher::new(local.provider_dyn())),
            );
            let key = ImageKey::poster("m1");
            let spec = local.image_spec(&key).expect("条目有海报");

            assert!(service.load(&spec).await.is_none());
            assert!(
                !service.was_requested(&key),
                "没有令牌不是下载失败：不能记成 failed，否则登录后也不会重试"
            );
            assert!(service.load(&spec).await.is_none());
        });
    }

    /// 应用内登录成功之后，图片下载器必须能拿到新会话。
    ///
    /// 以前 fetcher 是启动时一次性构造的（`NOVA_CREDENTIALS=memory` 启动时没有令牌
    /// 就永远是 `None`）；现在每次都问 `Local` 的即时状态。
    #[test]
    fn the_image_fetcher_follows_the_session_status() {
        use nova_provider::{Flavor, Provider, ProviderConfig};
        use nova_secrets::{CredentialStore, MemoryStore, SecretString};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/Items/m1/Images/Primary"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(PNG_1X1.to_vec()))
                .expect(1)
                .mount(&server)
                .await;

            let dir = tempfile::tempdir().expect("临时目录");
            let server_id = "srv-image-login";
            {
                let db = Database::open(dir.path().join("nova.db")).expect("建库");
                let mut info = offline_server_info(server_id);
                info.base_url = server.uri();
                db.upsert_server(&info, 1).expect("写服务器");
                let mut item =
                    item_with_images(Some("http://host/Items/m1/Images/Primary?tag=t"), None);
                item.id = ItemId::new("m1");
                item.kind = MediaKind::Movie;
                db.upsert_items(&ServerId::new(server_id), &[item], 1)
                    .expect("写条目");
            }
            let env = crate::env::Env {
                data_dir: dir.path().to_path_buf(),
                credentials: crate::env::CredentialMode::Memory,
            };
            let local = Arc::new(Local::open(&env, None).expect("打开状态目录"));
            let key = ImageKey::poster("m1");
            let spec = local.image_spec(&key).expect("条目有海报");
            let service = ImageService::new(
                Arc::clone(&local.image_cache),
                Arc::new(ProviderFetcher::from_local(Arc::clone(&local))),
            );

            assert!(service.load(&spec).await.is_none(), "离线时不应下载");

            // 应用内登录：新 provider 与 Connected 状态一起落地。
            let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
            store
                .set(
                    &format!("jellyfin/{server_id}/token"),
                    &SecretString::new("token-image"),
                )
                .expect("写令牌");
            let provider = Provider::new(
                Flavor::Jellyfin,
                ProviderConfig::new(ServerId::new(server_id), server.uri()),
                Arc::clone(&store),
            )
            .expect("wiremock 地址合法");
            let mut info = offline_server_info(server_id);
            info.base_url = server.uri();
            info.auth = AuthState::Authorized {
                user_name: "tester".to_owned(),
                credential_key: format!("jellyfin/{server_id}/token"),
            };
            local.adopt_server(provider, &info).expect("接管新会话");
            assert!(local.status().is_connected());

            let loaded = service.load(&spec).await.expect("登录后应当能下载海报");
            assert_eq!(loaded.0, key);
            assert_eq!((loaded.1.width, loaded.1.height), (1, 1));
        });
    }

    fn offline_server_info(id: &str) -> ServerInfo {
        ServerInfo {
            id: ServerId::new(id),
            kind: ServerKind::Jellyfin,
            name: "离线测试".to_owned(),
            base_url: "http://127.0.0.1:8096".to_owned(),
            version: None,
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: Language::undetermined(),
        }
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

    /// 可切换成功 / 失败的下载器（会话恢复前后对比用）。
    struct SwitchableFetcher {
        calls: AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
    }

    impl SwitchableFetcher {
        fn new(fail: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: std::sync::atomic::AtomicBool::new(fail),
            }
        }

        fn set_failing(&self, failing: bool) {
            self.fail.store(failing, Ordering::SeqCst);
        }
    }

    impl Fetcher for SwitchableFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.fail.load(Ordering::SeqCst) {
                    return Err(FetchError::Http {
                        status: 404,
                        endpoint: nova_images::endpoint_of(url),
                    });
                }
                Ok(PNG_1X1.to_vec())
            })
        }
    }

    /// 会话恢复在线（重新登录 / 刷新成功）后，之前失败的图要能重新请求。
    ///
    /// 修复前 `failed` 永远不清理：断网一次、重新登录也不会把那屏海报拿回来。
    #[test]
    fn clear_failures_lets_a_recovered_session_reload_the_same_key() {
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
            let fetcher = Arc::new(SwitchableFetcher::new(true));
            let service =
                ImageService::new(Arc::clone(&cache), Arc::clone(&fetcher) as Arc<dyn Fetcher>);
            let spec = ImageSpec::new("abc", ImageKind::Poster, POSTER_WIDTH, None).expect("请求");

            assert!(service.load(&spec).await.is_none());
            assert!(service.was_requested(&spec.key), "404 属于永久失败");

            // 会话恢复在线（`ui::Shell::apply_status` 在这里调 `clear_failures`）。
            fetcher.set_failing(false);
            service.clear_failures();

            let loaded = service
                .load(&spec)
                .await
                .expect("恢复在线后同一个键要能重新请求");
            assert_eq!(loaded.0, spec.key);
            assert_eq!(
                fetcher.calls.load(Ordering::SeqCst),
                2,
                "恢复后要真的再发一次下载"
            );
        });
    }

    /// 网络错误 / 超时 / 5xx 只是“这次没拿到”，不能记成“这张图坏了”
    /// （以前它们返回空体 → `EmptyBody` → 永久 `failed`，断网一次海报就再也
    /// 不会重试）。
    #[test]
    fn a_network_error_does_not_blacklist_the_image() {
        use nova_provider::{Flavor, Provider, ProviderConfig, RetryPolicy};
        use nova_secrets::{CredentialStore, MemoryStore, SecretString};

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
            // 端口 1 上没有监听者：连接会被立即拒绝（确定性的网络错误，不依赖外网）。
            let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
            store
                .set("jellyfin/srv-dead/token", &SecretString::new("token-dead"))
                .expect("写令牌");
            let provider = Provider::new(
                Flavor::Jellyfin,
                ProviderConfig::new(ServerId::new("srv-dead"), "http://127.0.0.1:1"),
                Arc::clone(&store),
            )
            .expect("地址合法")
            .with_retry_policy(RetryPolicy::none());
            let provider: Arc<dyn MediaProvider> = Arc::new(provider);
            let service = ImageService::new(cache, Arc::new(ProviderFetcher::new(Some(provider))));
            let spec = ImageSpec::new("abc", ImageKind::Poster, POSTER_WIDTH, None).expect("请求");

            assert!(service.load(&spec).await.is_none());
            assert!(
                !service.was_requested(&spec.key),
                "网络错误只是本次跳过：记成 failed 的话恢复后也不会重试"
            );
        });
    }

    /// 403 不能被谎报成 401（401 会让上层把会话标成过期）。
    #[test]
    fn a_forbidden_image_keeps_its_real_status_code() {
        use nova_provider::{Flavor, Provider, ProviderConfig};
        use nova_secrets::{CredentialStore, MemoryStore, SecretString};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/Items/abc/Images/Primary"))
                .respond_with(ResponseTemplate::new(403))
                .expect(1)
                .mount(&server)
                .await;
            let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
            store
                .set("jellyfin/srv-403/token", &SecretString::new("token-403"))
                .expect("写令牌");
            let provider = Provider::new(
                Flavor::Jellyfin,
                ProviderConfig::new(ServerId::new("srv-403"), server.uri()),
                Arc::clone(&store),
            )
            .expect("wiremock 地址合法");
            let provider: Arc<dyn MediaProvider> = Arc::new(provider);
            let fetcher = ProviderFetcher::new(Some(provider));
            let spec = ImageSpec::new("abc", ImageKind::Poster, POSTER_WIDTH, None).expect("请求");

            match fetcher.fetch(&spec.url).await {
                Err(FetchError::Http { status, .. }) => {
                    assert_eq!(status, 403, "403 要保留真实状态码（不是 401）");
                }
                other => panic!("应当是一次 403：{other:?}"),
            }
            server.verify().await;
        });
    }

    fn keys(prefix: &str, count: usize) -> Vec<ImageKey> {
        (0..count)
            .map(|index| ImageKey::poster(format!("{prefix}{index:02}")))
            .collect()
    }

    /// 可见窗口 → 需要请求的键：同一次上报里的重复只发一次，已请求过的不重发。
    #[test]
    fn window_requests_each_key_once_and_only_new_ones() {
        let mut state = RequestState::default();
        let window = [
            ImageKey::poster("a"),
            ImageKey::poster("b"),
            ImageKey::poster("a"),
        ];
        assert_eq!(
            state.begin_window(ImageScope::Library, &window),
            vec![ImageKey::poster("a"), ImageKey::poster("b")],
            "重复键只请求一次"
        );
        // 同一个窗口再报一次（滚动但窗口未变）：不再发请求。
        assert!(state.begin_window(ImageScope::Library, &window).is_empty());
        // 滚到新窗口：只发新进入视口的键。
        let next = [ImageKey::poster("b"), ImageKey::poster("c")];
        assert_eq!(
            state.begin_window(ImageScope::Library, &next),
            vec![ImageKey::poster("c")]
        );
        assert!(state.started.contains(&ImageKey::poster("a")), "a 已请求过");
        assert!(!state.is_wanted(&ImageKey::poster("a")), "a 已滚出视口");
    }

    /// 滚出视口且尚未开始下载 → 取消并解除标记（滚回来时可以重新请求）。
    #[test]
    fn scrolling_out_cancels_unstarted_requests() {
        let mut state = RequestState::default();
        let first = keys("p", 10);
        let requested = state.begin_window(ImageScope::Library, &first);
        assert_eq!(requested.len(), 10);

        // 快速滚动：新窗口只剩 p07..p09。
        let second = keys("p", 10)[7..].to_vec();
        assert!(state.begin_window(ImageScope::Library, &second).is_empty());

        let mut cancelled = Vec::new();
        for (index, key) in first.iter().enumerate() {
            if state.cancel_if_unwanted(key) {
                cancelled.push(index);
            }
        }
        assert_eq!(
            cancelled,
            (0..7).collect::<Vec<_>>(),
            "只取消滚出视口的前 7 张"
        );
        // 取消后标记已解除：再滚回来会重新请求。
        let again = state.begin_window(ImageScope::Library, &first);
        assert_eq!(again.len(), 7, "取消过的前 7 张重新进入视口时要重新请求");
    }

    /// 只要还有一个来源需要这张图，就不能取消（首页 hero 与卡片可能同键）。
    #[test]
    fn a_key_wanted_by_any_scope_is_not_cancelled() {
        let mut state = RequestState::default();
        let poster = ImageKey::poster("hero");
        state.begin_window(ImageScope::HomeHero, std::slice::from_ref(&poster));
        state.begin_window(ImageScope::Detail, std::slice::from_ref(&poster));
        // 详情页离开：首页 hero 仍然需要它。
        state.begin_window(ImageScope::Detail, &[]);
        assert!(!state.cancel_if_unwanted(&poster), "还有来源需要它");
        state.begin_window(ImageScope::HomeHero, &[]);
        assert!(state.cancel_if_unwanted(&poster), "最后一个来源也不要了");
    }

    /// 失败过的键不再进队列（哪怕它又滚回视口）。
    #[test]
    fn failed_keys_never_enter_the_queue_again() {
        let mut state = RequestState::default();
        let key = ImageKey::poster("bad");
        state.failed.insert(key.clone());
        assert!(state.begin_window(ImageScope::Library, &[key]).is_empty());
    }

    /// 可见窗口里的键一个都不能少：旧行为是整页 `take(120)`，第 121 张永远轮不到。
    #[test]
    fn a_visible_window_is_never_truncated_to_the_old_120_cap() {
        let mut state = RequestState::default();
        let window = keys("w", 300);
        let requested = state.begin_window(ImageScope::Library, &window);
        assert_eq!(requested.len(), 300, "可见窗口里的 300 张都要请求");
        assert_eq!(requested.last(), window.last());
    }

    /// 页面离屏：释放可见窗口后这些键不再受保护，没开始的请求也能取消。
    ///
    /// 修复前 `home.slint` 在离屏时一次都不上报、Rust 侧 `request_home_images`
    /// 又在非首页直接 `return`，所以 `ImageScope::HomeRow(n)` 的 `visible` 集合
    /// 永远停在离开时的值：`is_wanted` 恒为真、排队中的请求不会取消、这些键
    /// 在 `ImageStore::trim` 里永远被当成 protect，白占预算。
    #[test]
    fn releasing_a_page_clears_its_window_and_unprotects_keys() {
        let mut state = RequestState::default();
        let hero = ImageKey::backdrop("hero");
        let row = keys("p", 4);
        let library = keys("lib", 2);
        state.begin_window(ImageScope::HomeHero, std::slice::from_ref(&hero));
        state.begin_window(ImageScope::HomeRow(0), &row);
        state.begin_window(ImageScope::HomeRow(1), &keys("q", 2));
        state.begin_window(ImageScope::Library, &library);
        assert!(state.is_wanted(&row[0]));

        state.release_home_rows();
        assert!(!state.is_wanted(&hero), "hero 窗口必须清空");
        assert!(!state.is_wanted(&row[0]), "HomeRow 窗口必须清空");
        assert!(
            state.cancel_if_unwanted(&row[0]),
            "释放后排队中的请求必须能取消"
        );
        assert!(
            state.is_wanted(&library[0]),
            "别的页面（Library）的窗口不受影响"
        );

        state.release(ImageScope::Library);
        assert!(!state.is_wanted(&library[0]));
        assert!(state.cancel_if_unwanted(&library[0]));

        // 重新进入页面：释放过的键会重新排进请求（磁盘缓存命中，不重复下载）。
        let again = state.begin_window(ImageScope::HomeRow(0), &row);
        assert_eq!(again.len(), row.len(), "释放后重新进入页面要重新上报");
    }

    /// 同一个键被两个来源要时，释放其中一个不能把它从另一个窗口里拿掉。
    #[test]
    fn releasing_one_scope_keeps_keys_wanted_by_another() {
        let mut state = RequestState::default();
        let poster = ImageKey::poster("shared");
        state.begin_window(ImageScope::Detail, std::slice::from_ref(&poster));
        state.begin_window(ImageScope::HomeHero, std::slice::from_ref(&poster));
        state.release(ImageScope::Detail);
        assert!(state.is_wanted(&poster), "首页 hero 还要这张图");
        assert!(!state.cancel_if_unwanted(&poster));
        state.release_home_rows();
        assert!(!state.is_wanted(&poster));
        assert!(state.cancel_if_unwanted(&poster));
    }

    /// 只被预取排队的键进入视口时，必须升级为可见窗口请求。
    ///
    /// 预取只有 1 个名额；如果 `begin_window` 看到 `started` 已有标记就跳过，
    /// 可见窗里的图会排在 119 个预取后面（队头阻塞以另一种形式回来）。
    #[test]
    fn a_prefetched_key_is_promoted_to_a_visible_request() {
        let mut state = RequestState::default();
        let key = ImageKey::poster("queued");
        // 模拟 `request_prefetch`：已经置位 `started`，但只是预取排队。
        state.started.insert(key.clone());
        state.prefetched.insert(key.clone());

        let fresh = state.begin_window(ImageScope::Library, std::slice::from_ref(&key));
        assert_eq!(
            fresh,
            vec![key.clone()],
            "预取排队的键进入视口时必须 spawn 一个可见任务"
        );
        assert!(!state.prefetched.contains(&key), "升级后不再算排队中的预取");
        assert!(state.started.contains(&key), "仍然只算已请求一次");

        // 已由可见窗口请求过的键再上报，不重复 spawn。
        assert!(
            state
                .begin_window(ImageScope::Library, std::slice::from_ref(&key))
                .is_empty()
        );
    }

    fn poster_item(id: &str) -> MediaItem {
        let mut item = item_with_images(
            Some(&format!("http://host/Items/{id}/Images/Primary")),
            None,
        );
        item.id = ItemId::new(id);
        item
    }

    /// 一个带服务器 + 条目的真实 `Local`（图片键能解析成 `ImageSpec`）。
    fn local_with_items(dir: &std::path::Path, ids: &[String]) -> Arc<Local> {
        {
            let db = Database::open(dir.join("nova.db")).expect("建库");
            db.upsert_server(
                &ServerInfo {
                    id: ServerId::new("srv-img"),
                    kind: ServerKind::Jellyfin,
                    name: "图片测试".to_owned(),
                    base_url: "http://127.0.0.1:8096".to_owned(),
                    version: None,
                    user_id: None,
                    auth: AuthState::Unauthorized,
                    metadata_language: Language::undetermined(),
                },
                1,
            )
            .expect("写服务器");
            let items: Vec<MediaItem> = ids.iter().map(|id| poster_item(id)).collect();
            db.upsert_items(&ServerId::new("srv-img"), &items, 1)
                .expect("写条目");
        }
        let env = crate::env::Env {
            data_dir: dir.to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        Arc::new(Local::open(&env, None).expect("打开状态目录"))
    }

    /// 阻塞式假下载器：`p` 开头的条目（预取键）会一直等 `gate` 放行，可见键立即返回。
    struct GatedFetcher {
        gate: tokio::sync::Semaphore,
        prefetch_calls: AtomicUsize,
        visible_calls: AtomicUsize,
    }

    impl GatedFetcher {
        fn new() -> Self {
            Self {
                gate: tokio::sync::Semaphore::new(0),
                prefetch_calls: AtomicUsize::new(0),
                visible_calls: AtomicUsize::new(0),
            }
        }

        fn is_prefetch(url: &str) -> bool {
            parse_synthetic_url(url)
                .map(|parsed| parsed.item_id.starts_with('p'))
                .unwrap_or(false)
        }
    }

    impl Fetcher for GatedFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            let prefetch = Self::is_prefetch(url);
            Box::pin(async move {
                if prefetch {
                    self.prefetch_calls.fetch_add(1, Ordering::SeqCst);
                    let _permit = self.gate.acquire().await;
                } else {
                    self.visible_calls.fetch_add(1, Ordering::SeqCst);
                }
                Ok(PNG_1X1.to_vec())
            })
        }
    }

    #[test]
    fn visible_window_is_not_blocked_by_queued_prefetch() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("临时目录");
            let mut ids: Vec<String> = (0..MAX_PREFETCH).map(|i| format!("p{i:03}")).collect();
            ids.push("visible".to_owned());
            let local = local_with_items(dir.path(), &ids);
            let db = Arc::new(Database::open_in_memory().expect("内存库"));
            let cache = Arc::new(
                ImageCache::new(dir.path().join("images"), db, IMAGE_CACHE_BUDGET)
                    .expect("缓存目录"),
            );
            let fetcher = Arc::new(GatedFetcher::new());
            let service = Arc::new(ImageService::new(
                Arc::clone(&cache),
                fetcher.clone() as Arc<dyn Fetcher>,
            ));
            let handle = runtime.handle().clone();
            let (sender, mut received) = tokio::sync::mpsc::unbounded_channel::<ImageKey>();

            let prefetch_keys: Vec<ImageKey> = ids[..MAX_PREFETCH]
                .iter()
                .map(|id| ImageKey::poster(id.clone()))
                .collect();
            let prefetch_sender = sender.clone();
            service.request_prefetch(&local, prefetch_keys, &handle, move |key, _| {
                let _ = prefetch_sender.send(key);
            });
            // 等预取真正开始：第 1 张被 gate 拦住，其余在预取名额上排队。
            // 轮询而非固定 sleep：负载高时 150ms 内预取可能还没被调度到。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while fetcher.prefetch_calls.load(Ordering::SeqCst) == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "10 秒内预取一张都没发起：request_prefetch 没有把任务交给 runtime"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            let visible = ImageKey::poster("visible");
            let visible_sender = sender.clone();
            service.request_window(
                &local,
                ImageScope::Library,
                std::slice::from_ref(&visible),
                &handle,
                move |key, _| {
                    let _ = visible_sender.send(key);
                },
            );
            let delivered = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if let Some(key) = received.recv().await
                        && key == visible
                    {
                        break;
                    }
                }
            })
            .await;
            assert!(
                delivered.is_ok(),
                "可见窗口的请求被排队中的预取阻塞了：修复前预取与可见窗口共用 \
                 Semaphore(4)，120 个预取占满名额后首屏海报要排在它们后面"
            );
            assert_eq!(fetcher.visible_calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn leaving_home_discards_queued_prefetch() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("测试 runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("临时目录");
            let ids: Vec<String> = (0..MAX_PREFETCH).map(|i| format!("p{i:03}")).collect();
            let local = local_with_items(dir.path(), &ids);
            let db = Arc::new(Database::open_in_memory().expect("内存库"));
            let cache = Arc::new(
                ImageCache::new(dir.path().join("images"), db, IMAGE_CACHE_BUDGET)
                    .expect("缓存目录"),
            );
            let fetcher = Arc::new(GatedFetcher::new());
            let service = Arc::new(ImageService::new(
                Arc::clone(&cache),
                fetcher.clone() as Arc<dyn Fetcher>,
            ));
            let handle = runtime.handle().clone();
            let keys: Vec<ImageKey> = ids.iter().map(|id| ImageKey::poster(id.clone())).collect();
            service.request_prefetch(&local, keys.clone(), &handle, |_, _| {});
            // 等第一张预取真正发起（被 gate 拦住，其余在预取名额上排队）：
            // 固定 sleep 150ms 在负载高的 CI 上会等到“还没开始”，断言于是偶发红灯。
            let started = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while fetcher.prefetch_calls.load(Ordering::SeqCst) == 0 {
                assert!(
                    std::time::Instant::now() < started,
                    "10 秒内预取一张都没发起：request_prefetch 没有把任务交给 runtime"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(
                fetcher.prefetch_calls.load(Ordering::SeqCst),
                1,
                "预取名额只有 1 个：第一张没放手前不能有第二张开始"
            );

            // 离开首页：排队中的预取必须作废。
            service.cancel_prefetch();
            fetcher.gate.add_permits(MAX_PREFETCH * 2);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let requested = keys.iter().filter(|key| service.was_requested(key)).count();
                if requested == 1 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "排队中的预取没有被丢弃：仍有 {requested} 个键处于已请求状态"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(
                fetcher.prefetch_calls.load(Ordering::SeqCst),
                1,
                "离开首页后，尚未开始的预取不能再发下载"
            );
        });
    }
}
