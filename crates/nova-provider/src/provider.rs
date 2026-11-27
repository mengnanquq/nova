// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 媒体服务端抽象。
//!
//! 上层只认这一个 trait：新增一个服务端 = 新增一个实现，而不是在界面里加 `if`。
//! 方法分成四组：
//!
//! - **元信息**：[`kind`](MediaProvider::kind)、[`base_url`](MediaProvider::base_url)、
//!   [`server_info`](MediaProvider::server_info)、
//!   [`public_info`](MediaProvider::public_info)、[`logout`](MediaProvider::logout)。
//! - **认证**：[`authenticate`](MediaProvider::authenticate)。
//! - **浏览**：[`libraries`](MediaProvider::libraries)、[`items`](MediaProvider::items)、
//!   [`item`](MediaProvider::item)、[`children`](MediaProvider::children)、
//!   [`image`](MediaProvider::image)。
//! - **播放**：[`sources`](MediaProvider::sources)、
//!   [`sources_with_session`](MediaProvider::sources_with_session)、
//!   [`stream_target`](MediaProvider::stream_target)、
//!   [`report_progress`](MediaProvider::report_progress)。
//!
//! 所有方法都是 `&self`：实现内部用内部可变性保存令牌缓存与认证状态，
//! 因此 `Arc<dyn MediaProvider>` 可以被界面、播放器、增量同步同时持有。

use async_trait::async_trait;
use nova_core::{
    ItemId, MediaItem, MediaSource, PlaybackProgress, ServerInfo, ServerKind, SourceId,
};
use nova_secrets::SecretString;

use crate::error::ProviderError;
use crate::types::{ImageRequest, ItemQuery, Library, Page, StreamTarget};

/// 媒体服务器统一接口。
///
/// # 认证与令牌
///
/// [`authenticate`](Self::authenticate) 拿到令牌后**只**写进注入的
/// [`CredentialStore`](nova_secrets::CredentialStore)，
/// [`ServerInfo::auth`] 里保留的只是一个键名。密码以 [`SecretString`] 传入，
/// 调用方不需要（也不应该）在别处保存明文。
///
/// # 重试
///
/// 幂等 GET 的 5xx 与超时由实现内部按
/// [`RetryPolicy`](crate::RetryPolicy) 有限重试；调用方看到 `Err` 时
/// 说明重试已经用尽或错误不可重试（可用 [`ProviderError::is_transient`] 判断）。
#[async_trait]
pub trait MediaProvider: Send + Sync {
    /// 服务端类型。
    fn kind(&self) -> ServerKind;

    /// 规范化后的基址（不含末尾斜杠）。
    fn base_url(&self) -> &str;

    /// 当前服务器信息快照。
    ///
    /// 结构里没有任何明文令牌，可以整段打印或存进 `nova-storage`。
    fn server_info(&self) -> ServerInfo;

    /// 服务端自己公开（无需登录）的服务器信息：`GET /System/Info/Public`。
    ///
    /// 返回的 [`ServerInfo`] 只描述**服务端**：`name` / `version` 来自服务端，
    /// `kind` 按 `ProductName` 判定（对不上时回退到构造时的 flavor），
    /// `user_id` 为 `None`、`auth` 为 [`AuthState::Unauthorized`](nova_core::AuthState::Unauthorized)
    /// ——这两个字段属于当前会话，不属于服务器本身。
    ///
    /// 未登录也可以调用；[`authenticate`](Self::authenticate) 成功后会自己调一次，
    /// 其余方法不会重复请求这个端点。
    /// 把公开信息合并进本地 [`ServerInfo`](nova_core::ServerInfo) 快照。
    ///
    /// 只覆盖 `name` / `kind` / `version` 三个"服务端自述"的字段，**不碰**
    /// `auth` / `user_id` / `credential_key`——那些是本机会话状态。
    async fn public_info_as_server_info(&self) -> Result<ServerInfo, ProviderError>;

    /// 服务端的公开信息（不需要令牌）。
    ///
    /// 刻意返回窄结构体：公开端点拿不到 `auth` / `user_id`，用完整的 `ServerInfo`
    /// 会把"已登录"的本地状态冲成未认证。
    async fn public_info(&self) -> Result<crate::types::PublicServerInfo, ProviderError>;

    /// 用用户名密码登录。
    ///
    /// 成功后令牌进入凭证存储，返回更新过的服务器信息（含用户名与用户标识）。
    /// 失败时不会留下任何部分状态：令牌缓存与凭证存储都保持原样。
    async fn authenticate(
        &self,
        user: &str,
        password: &SecretString,
    ) -> Result<ServerInfo, ProviderError>;

    /// 列出媒体库（只含电影库与剧集库）。
    async fn libraries(&self) -> Result<Vec<Library>, ProviderError>;

    /// 查询条目列表。
    async fn items(&self, query: ItemQuery) -> Result<Page<MediaItem>, ProviderError>;

    /// 取单个条目。
    async fn item(&self, id: &ItemId) -> Result<MediaItem, ProviderError>;

    /// 取某个容器的直接子条目（剧集 → 季，季 → 集）。
    ///
    /// 返回空 `Vec` 表示该容器确实是空的；条目本身不存在时返回
    /// [`ProviderError::NotFound`]。
    async fn children(&self, id: &ItemId) -> Result<Vec<MediaItem>, ProviderError>;

    /// 取条目的可播放资源（同一部片的多个版本各算一个）。
    ///
    /// 只要资源时用它；需要**本会话**的服务端会话标识时用
    /// [`sources_with_session`](Self::sources_with_session)，不要「先调它、再查
    /// [`latest_play_session_id`](Self::latest_play_session_id)」——那两步之间能插进
    /// 别的 `PlaybackInfo`。
    async fn sources(&self, id: &ItemId) -> Result<Vec<MediaSource>, ProviderError>;

    /// 取资源，并把**这一次** `PlaybackInfo` 响应自带的 `PlaySessionId` 一并带回。
    ///
    /// player 在起播那一刻用这个值快照本会话的标识，随后写进每条
    /// [`PlaybackProgress::play_session_id`]。和「`sources()` + `latest_play_session_id()`」
    /// 的区别就在**原子性**：两个值出自同一个响应，中间插不进别的 `PlaybackInfo`
    /// （播放期间详情页刷新 `sources()`、同一部片重播都会覆盖条目上的“最近一次”，
    /// 那次覆盖如果正好夹在两次调用之间，本会话就会拿到别人的标识）。
    ///
    /// 默认实现退回两次调用：不记会话的实现（测试假 provider、离线方言）行为不变。
    async fn sources_with_session(
        &self,
        id: &ItemId,
    ) -> Result<crate::types::PlaybackSources, ProviderError> {
        let sources = self.sources(id).await?;
        Ok(crate::types::PlaybackSources {
            sources,
            play_session_id: self.latest_play_session_id(id),
        })
    }

    /// 条目**最近一次** `PlaybackInfo` 给出的服务端会话标识（`PlaySessionId`）。
    ///
    /// player 在起播那一刻（`PlaybackInfo` 刚返回）把它记进**本会话**，随后写进每条
    /// [`PlaybackProgress::play_session_id`]，上报时原样带回；上报端只在进度没带标识时
    /// 才回退到这里（旧版本落盘的待发文件）。不能等到发送时再查：同一部片重播（或
    /// 重试）时会再取一次 `PlaybackInfo`，条目上的记录已被新会话覆盖，旧会话的 `stop`
    /// 就会带上新会话的标识。
    ///
    /// "最近一次"也意味着它**不是**某个会话的常量：播放期间详情页刷新（`sources()`）
    /// 同样会覆盖它，所以只能当兜底，不能当会话身份。
    ///
    /// 默认 `None`：不记会话的实现（测试假 provider、离线方言）上报时退回老行为。
    fn latest_play_session_id(&self, _item: &ItemId) -> Option<String> {
        None
    }

    /// 构造可播放直链。
    ///
    /// 返回值的查询串里带令牌，`Debug` 已做脱敏；需要在别处传递时
    /// 只使用 [`StreamTarget::url`] 交给播放引擎。
    async fn stream_target(
        &self,
        item: &ItemId,
        source: &SourceId,
    ) -> Result<StreamTarget, ProviderError>;

    /// 上报"开始播放"。
    ///
    /// Jellyfin/Emby 需要先收到这条，才会把后续的 Progress/Stopped 关联到会话；
    /// 只发 Progress 时服务端日志是 `PlaybackStopped reported with null media info`，
    /// 条目上的续播点永远不更新（实测）。
    async fn report_started(&self, progress: &PlaybackProgress) -> Result<(), ProviderError>;

    /// 上报"结束播放"。
    ///
    /// **必须**在退出播放时发（不只是"看完"时）：Jellyfin/Emby 只在收到这条时才把
    /// `PlaybackPositionTicks` 持久化到条目上。只发 Progress 的表现是服务端日志一切正常、
    /// 但续播点永远是 0（实测）。
    async fn report_stopped(&self, progress: &PlaybackProgress) -> Result<(), ProviderError>;

    /// 上报播放进度。已看完时实现会额外提交一次"停止播放"。
    async fn report_progress(&self, progress: &PlaybackProgress) -> Result<(), ProviderError>;

    /// 下载图片字节。
    ///
    /// 请求**不**在地址里带令牌（改用认证头），因此图片地址可以安全地
    /// 存进缓存库、贴进日志。
    async fn image(&self, request: ImageRequest) -> Result<Vec<u8>, ProviderError>;

    /// 登出：删除凭证存储里的令牌，并把状态退回未认证。
    ///
    /// 幂等：没有令牌时同样返回 `Ok(())`。
    async fn logout(&self) -> Result<(), ProviderError>;
}
