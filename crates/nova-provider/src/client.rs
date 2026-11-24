// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! [`MediaProvider`] 的具体实现：一套代码同时服务 Jellyfin 与 Emby。
//!
//! 差异集中在 [`Flavor`]（认证头名字、密码字段名、令牌兜底响应头），
//! 其余流程完全共用，理由见 [`mod@crate::mapping`] 的模块文档。
//!
//! # 令牌的生命周期
//!
//! ```text
//! 密码(SecretString) ──POST AuthenticateByName──> 令牌(SecretString)
//!                                                   │
//!                        ┌──────────────────────────┼─────────────────────────┐
//!                        ▼                          ▼                         ▼
//!               CredentialStore.set          Provider 内缓存            拼 Authorization 头
//!               （键名 jellyfin/<id>/token）  （RwLock<State>）         （只活一次请求）
//! ```
//!
//! 缓存只是为了不必每个请求都读一次钥匙串；明文在内存里始终是 [`SecretString`]，
//! 唯一的例外是拼 `Authorization` 头与 `api_key` 查询串时的那一份 `String`——
//! 它跟着 `reqwest` 的请求结构一起被丢弃。`Debug` 输出里不会有它，见
//! [`Provider`] 的手写 `Debug`。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use async_trait::async_trait;
use nova_core::{
    AuthState, ItemId, Language, MediaItem, MediaKind, MediaSource, PlaybackProgress, ServerId,
    ServerInfo, ServerKind, SourceId,
};
use nova_secrets::{CredentialStore, SecretError, SecretString};
use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::ProviderError;
use crate::http::{
    RetryPolicy, build_url, parse_base_url, parse_retry_after, validate_path_segment,
};
use crate::mapping::{
    Flavor, RawAuthenticationResult, RawErrorBody, RawItem, RawItemsResponse, RawPlaybackInfo,
    RawPublicSystemInfo, RawUser, duration_to_ticks, kind_from_public, kind_to_api, map_item,
    map_items, map_library, map_sources, play_method_to_api,
};
use crate::provider::MediaProvider;
use crate::types::{ImageRequest, ItemQuery, Library, Page, StreamTarget};
use crate::{CLIENT_NAME, CLIENT_VERSION};

/// 默认的单次请求超时。
///
/// 媒体库首页可能触发服务端全盘扫描，30 秒已经足够宽松；更长的等待应该由界面
/// 显示进度，而不是让一个 HTTP 请求挂在那里。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 连接阶段的超时上限。
///
/// 整体 `timeout` 管的是“从发出请求到收完响应”，TCP 握手卡住时要靠它自己兜住；
/// 5 秒足够跨市、跨洋连上家用服务器，再长就只能怪网络了。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 连接超时的下限。
///
/// 小于 1 秒的连接超时在真实网络上几乎必定误判（DNS 解析都不止），
/// 所以只有当调用方把整体超时调得比它更小时才会低于这个值（见 [`connect_timeout_for`]）。
pub const MIN_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// 声明自己是哪个设备。
///
/// `id` 应当在**同一台机器上保持稳定**（服务端用它区分播放会话、去重"继续观看"），
/// 但不要用网卡 MAC 之类能跨应用关联的硬件指纹。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// 显示名（"客厅的 Mac mini"）。
    pub name: String,
    /// 稳定标识。
    pub id: String,
}

impl DeviceInfo {
    /// 构造设备信息。
    pub fn new(name: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
        }
    }
}

/// 构造 [`Provider`] 需要的信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    /// 本机分配的服务器标识，用于拼凭证键名（`jellyfin/<server-id>/token`）。
    pub server_id: ServerId,
    /// 服务器基址，只接受 `http` / `https`。
    pub base_url: String,
    /// 界面上显示的服务器名；`None` 时用主机名。
    pub display_name: Option<String>,
    /// 设备信息。
    pub device: DeviceInfo,
    /// 服务端元数据语言偏好。
    pub metadata_language: Language,
    /// 上次认证得到的服务端用户标识（即 `ServerInfo.user_id`）。
    ///
    /// 多个 `/Users/{user_id}/…` 端点需要它，而它只在 `authenticate()` 的响应里出现；
    /// 新进程只拿得到凭证存储里的令牌，所以调用方应当把上次存下的 `user_id` 传进来。
    /// 留 `None` 也不会直接失败：[`Provider`] 会在第一次需要它时用当前令牌
    /// 惰性解析一次（`GET /Users/Me`）。
    pub user_id: Option<String>,
}

impl ProviderConfig {
    /// 最小配置：设备名固定为 `Nova`，设备标识由 `server_id` 派生。
    pub fn new(server_id: ServerId, base_url: impl Into<String>) -> Self {
        let device_id = format!("nova-{}", server_id.as_str());
        Self {
            server_id,
            base_url: base_url.into(),
            display_name: None,
            device: DeviceInfo::new(CLIENT_NAME, device_id),
            metadata_language: Language::undetermined(),
            user_id: None,
        }
    }

    /// 指定显示名。
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// 指定设备信息。
    pub fn with_device(mut self, device: DeviceInfo) -> Self {
        self.device = device;
        self
    }

    /// 指定元数据语言偏好。
    pub fn with_metadata_language(mut self, language: Language) -> Self {
        self.metadata_language = language;
        self
    }

    /// 指定上次认证得到的用户标识。
    ///
    /// 给了就在构造时写进状态，不会再发 `GET /Users/Me`；空串按 `None` 处理。
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }
}

/// 内部可变状态：令牌缓存 + 服务器信息快照。
struct State {
    info: ServerInfo,
    token: Option<SecretString>,
}

/// Emby / Jellyfin 的统一实现。
///
/// 用法：
///
/// ```no_run
/// # use std::sync::Arc;
/// use nova_core::ServerId;
/// use nova_provider::{Flavor, MediaProvider, Provider, ProviderConfig};
/// use nova_secrets::MemoryStore;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let store = Arc::new(MemoryStore::new());
/// let config = ProviderConfig::new(ServerId::new("srv-1"), "http://192.168.1.10:8096");
/// let provider = Provider::new(Flavor::Jellyfin, config, store)?;
/// assert!(provider.server_info().is_plaintext());
/// # Ok(())
/// # }
/// ```
pub struct Provider {
    flavor: Flavor,
    base: Url,
    /// 规范化后的基址字符串（无末尾斜杠），用于对外展示。
    display_base: String,
    plaintext: bool,
    http: reqwest::Client,
    credentials: Arc<dyn CredentialStore>,
    retry: RetryPolicy,
    device: DeviceInfo,
    credential_key: String,
    state: RwLock<State>,
    /// 惰性解析用户标识（`GET /Users/Me`）的串行化锁。
    ///
    /// 值本身只有一份，存在 `state.info.user_id` 里；这把锁只用来保证
    /// “并发的第一次调用只真的发一次请求”，见 [`Provider::user_id`]。
    user_id_resolve: tokio::sync::Mutex<()>,
    /// 每个条目最近一次 `PlaybackInfo` 给出的 `PlaySessionId`。
    ///
    /// 上报进度必须带它，否则服务端把上报当成"无媒体信息的会话"直接忽略（实测）。
    /// 由 `sources()` 写入、`report_progress()` 读取；按条目覆盖即可。
    play_sessions: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl Provider {
    /// 构造 provider。
    ///
    /// 会校验基址并尝试从凭证存储里取回上次登录的令牌（取不到就按未认证处理）。
    /// `config.user_id` 会直接写进状态（见 [`ProviderConfig::with_user_id`]）；
    /// 它为空而令牌有效时不会报错，而是在第一次需要用户标识时惰性解析。
    /// 基址不合规（`file://`、空主机、`javascript:` …）返回
    /// [`ProviderError::Unsupported`]。
    pub fn new(
        flavor: Flavor,
        config: ProviderConfig,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self, ProviderError> {
        let base = parse_base_url(&config.base_url)?;
        let http = build_http_client(DEFAULT_REQUEST_TIMEOUT)?;

        let credential_key = format!(
            "{}/{}/token",
            flavor.credential_prefix(),
            config.server_id.as_str()
        );

        // 读不到就按未认证处理：钥匙串锁着不该让整个界面打不开。
        let token = match credentials.get(&credential_key) {
            Ok(token) => token,
            Err(error) => {
                tracing::warn!(error = %error, "读取凭证失败，先按未认证处理");
                None
            }
        };

        let display_name = config
            .display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| base.host_str().unwrap_or("未命名服务器").to_owned());
        let display_base = base.as_str().trim_end_matches('/').to_owned();

        let auth = match token {
            Some(_) => AuthState::Authorized {
                // 令牌能取回来，但用户名要等一次认证或服务端查询才能知道。
                user_name: String::new(),
                credential_key: credential_key.clone(),
            },
            None => AuthState::Unauthorized,
        };

        let info = ServerInfo {
            id: config.server_id,
            kind: flavor.server_kind(),
            name: display_name,
            base_url: display_base.clone(),
            version: None,
            user_id: config.user_id.filter(|id| !id.is_empty()),
            auth,
            metadata_language: config.metadata_language,
        };
        let plaintext = info.is_plaintext();

        Ok(Self {
            flavor,
            base,
            display_base,
            plaintext,
            http,
            credentials,
            retry: RetryPolicy::default(),
            device: config.device,
            credential_key,
            state: RwLock::new(State { info, token }),
            user_id_resolve: tokio::sync::Mutex::new(()),
            play_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 注入重试策略（默认 3 次尝试、指数退避）。
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// 注入单次请求超时。
    ///
    /// 超时时间参与"超时是否重试"的判定，所以必须在请求前设置好。
    /// 重建 HTTP 客户端失败时保留原来的超时，只记一条告警。
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        match build_http_client(timeout) {
            Ok(http) => self.http = http,
            Err(error) => tracing::warn!(%error, "重建 HTTP 客户端失败，保留原超时"),
        }
        self
    }

    /// 服务端产品差异。
    pub fn flavor(&self) -> Flavor {
        self.flavor
    }
    /// 凭证存储里的键名（`jellyfin/<server-id>/token`），`AuthState::Authorized` 里存的就是它。
    pub fn credential_key(&self) -> &str {
        &self.credential_key
    }

    /// 基址是否使用明文 HTTP。
    pub fn is_plaintext(&self) -> bool {
        self.plaintext
    }

    /// 把公开接口里的服务器名与版本写回状态，返回更新后的快照。
    ///
    /// 失败时保留 `fallback`（本地已有的名字）：公开接口不可用不该让登录失败。
    /// 只在 [`MediaProvider::authenticate`] 里调用，其余端点不会打这个接口。
    async fn refresh_public_info(&self, fallback: ServerInfo) -> ServerInfo {
        match self.public_info().await {
            Ok(public) => {
                let mut state = self.write_state();
                state.info.name = public.name;
                state.info.version = public.version;
                state.info.clone()
            }
            Err(error) => {
                tracing::debug!(error = %error, "取公开服务器信息失败，保留本地信息");
                fallback
            }
        }
    }

    /// 当前用户标识。
    ///
    /// 状态里没有时（例如新进程只从凭证存储里恢复到了令牌），用当前认证头惰性发
    /// 一次 `GET /Users/Me` 把 `Id` 解析出来并**写回状态**；后续调用直接命中状态。
    /// 连令牌都没有（或服务端说令牌无效）时才报 [`ProviderError::Unauthorized`]。
    ///
    /// 防并发重复请求：慢路径在 [`Provider::user_id_resolve`] 上排队，只有拿到锁的
    /// 那个调用会真的发请求，其余在锁释放后重新读状态就命中了（值的唯一来源是
    /// `state.info.user_id`，不存在两份缓存需要同步）。
    async fn user_id(&self) -> Result<String, ProviderError> {
        if let Some(id) = self.current_user_id()? {
            return Ok(id);
        }

        let _resolving = self.user_id_resolve.lock().await;
        if let Some(id) = self.current_user_id()? {
            return Ok(id);
        }

        let resolved = self.resolve_user_id().await?;
        self.write_state().info.user_id = Some(resolved.clone());
        Ok(resolved)
    }

    /// 读状态里已有的用户标识（已校验）；`None` 表示还不知道。
    fn current_user_id(&self) -> Result<Option<String>, ProviderError> {
        let id = self
            .read_state()
            .info
            .user_id
            .clone()
            .filter(|id| !id.is_empty());
        let Some(id) = id else {
            return Ok(None);
        };
        // 用户标识会被拼进 `/Users/{id}/…`；它来自服务端，同样当不可信输入。
        validate_path_segment(&id)
            .map_err(|_| ProviderError::InvalidResponse(USER_ID_SEGMENT_FIELD))?;
        Ok(Some(id))
    }

    /// `GET /Users/Me` → `Id`。
    async fn resolve_user_id(&self) -> Result<String, ProviderError> {
        let url = self.url(&["Users", "Me"], &[])?;
        let raw: RawUser = self.get_json(url).await?;
        let id = raw
            .id
            .filter(|id| !id.is_empty())
            .ok_or(ProviderError::InvalidResponse("用户信息里没有 id"))?;
        validate_path_segment(&id)
            .map_err(|_| ProviderError::InvalidResponse(USER_ID_SEGMENT_FIELD))?;
        tracing::debug!("已用已存令牌解析出用户标识");
        Ok(id)
    }

    fn token(&self) -> Result<SecretString, ProviderError> {
        self.read_state()
            .token
            .clone()
            .ok_or(ProviderError::Unauthorized)
    }

    fn read_state(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// `Authorization: MediaBrowser Client="Nova", Device="…", DeviceId="…", Version="…", Token="…"`。
    ///
    /// `token` 为 `None` 时用于登录请求本身（此时还没有令牌）。
    ///
    /// 各字段里的值一律**严格校验**而不是“洗干净再用”：悄悄改写设备名会让
    /// 用户在服务端看到的设备列表与自己填的对不上，而继续用一个被改坏的令牌
    /// 只会得到一堆莫名其妙的 401。
    fn auth_header(&self, token: Option<&SecretString>) -> Result<HeaderValue, ProviderError> {
        let mut value = format!(
            "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
            checked_header_value(CLIENT_NAME, CLIENT_FIELD)?,
            checked_header_value(&self.device.name, DEVICE_NAME_FIELD)?,
            checked_header_value(&self.device.id, DEVICE_ID_FIELD)?,
            checked_header_value(CLIENT_VERSION, CLIENT_FIELD)?,
        );
        if let Some(token) = token {
            value.push_str(", Token=\"");
            value.push_str(checked_header_value(token.expose(), TOKEN_FIELD)?);
            value.push('"');
        }
        // 只校验、不回显：失败时绝不能把 value 塞进错误信息（里面有令牌）。
        HeaderValue::from_str(&value).map_err(|_| ProviderError::InvalidResponse("认证头无法编码"))
    }

    async fn send<F>(&self, make: F, idempotent: bool) -> Result<Response, ProviderError>
    where
        F: Fn() -> RequestBuilder,
    {
        self.send_accepting(make, idempotent, &[]).await
    }

    /// 与 [`Self::send`] 相同，但 `accept` 里的失败状态码不转成错误，而是原样交回调用方。
    ///
    /// 只给“失败状态本身就是一种答案”的端点用：`PlaybackInfo` 对容器条目回 `400`
    /// / `404`，意思是“这个条目没有媒体源”（见 [`MediaProvider::sources`]）。
    /// `401` 这类语义明确的失败不要放进 `accept`。
    async fn send_accepting<F>(
        &self,
        make: F,
        idempotent: bool,
        accept: &[StatusCode],
    ) -> Result<Response, ProviderError>
    where
        F: Fn() -> RequestBuilder,
    {
        // 只对幂等 GET 重试；POST（上报进度）重发可能把进度写两遍。
        let attempts = if idempotent {
            self.retry.max_attempts()
        } else {
            1
        };
        let mut last_error: Option<ProviderError> = None;
        // 上一轮失败响应给出的 `Retry-After`，只在下一轮重试时用一次。
        let mut retry_after: Option<Duration> = None;

        for attempt in 1..=attempts {
            if attempt > 1 {
                let delay = self
                    .retry
                    .delay_with_retry_after(attempt - 1, retry_after.take());
                tracing::debug!(
                    attempt,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    "重试请求"
                );
                tokio::time::sleep(delay).await;
            }

            match make().send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() || accept.contains(&status) {
                        return Ok(response);
                    }
                    let retryable = is_retryable_status(status);
                    // 头必须在响应被消费之前读出来。
                    let after = if retryable {
                        retry_after_delay(response.headers())
                    } else {
                        None
                    };
                    let error = self.error_from_response(response).await;
                    if retryable && attempt < attempts {
                        retry_after = after;
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
                Err(source) => {
                    // 只有超时值得重试：连不上、TLS 握手失败重试也没用。
                    let retryable = source.is_timeout();
                    let error = network_error(source);
                    if retryable && attempt < attempts {
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| ProviderError::Network("请求失败".to_owned())))
    }

    /// 把失败响应翻译成错误。
    ///
    /// - 401 额外把状态标成“需要重新登录”；403 只报未授权，不动状态
    ///   （权限不足不等于令牌过期，误标会让用户被无故登出）。
    /// - 5xx 与 429 一律保留状态码，让 [`ProviderError::is_transient`] 能正确回答
    ///   “上层还要不要再试”；服务端给的可读原因**完全不入日志**（它可能回显
    ///   请求地址与令牌），只记状态码与长度。
    async fn error_from_response(&self, response: Response) -> ProviderError {
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            self.mark_expired();
            return ProviderError::Unauthorized;
        }
        if status == StatusCode::FORBIDDEN {
            return ProviderError::Unauthorized;
        }
        if status == StatusCode::NOT_FOUND {
            return ProviderError::NotFound;
        }
        let message = response
            .json::<RawErrorBody>()
            .await
            .ok()
            .and_then(|body| body.message)
            .filter(|message| !message.trim().is_empty());
        match message {
            Some(message) if is_retryable_status(status) => {
                tracing::debug!(
                    status = status.as_u16(),
                    message_len = message.chars().count(),
                    "服务端错误"
                );
                ProviderError::Http {
                    status: status.as_u16(),
                }
            }
            Some(message) => ProviderError::server_message(message),
            None => ProviderError::Http {
                status: status.as_u16(),
            },
        }
    }

    fn mark_expired(&self) {
        let mut state = self.write_state();
        let user_name = state.info.auth.user_name().unwrap_or_default().to_owned();
        state.info.auth = AuthState::Expired { user_name };
    }

    /// 发一次认证请求（不含错误分类，交给调用方决定要不要换字段名重试）。
    async fn post_auth(
        &self,
        url: &Url,
        body: &BTreeMap<&str, &str>,
    ) -> Result<Response, ProviderError> {
        let url = url.clone();
        let body = body.clone();
        let name = self.flavor.auth_header_name();
        let header = self.auth_header(None)?;
        self.send(
            move || {
                self.http
                    .post(url.clone())
                    .header(name, header.clone())
                    .json(&body)
            },
            false,
        )
        .await
    }

    /// 记下条目对应的播放会话标识（见 `play_sessions` 字段的注释）。
    fn remember_play_session(&self, item: &ItemId, session: &str) {
        let mut sessions = self
            .play_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 防止长时间运行时无界增长：这是"最近用到的会话"，不是审计日志。
        if sessions.len() > 256 {
            sessions.clear();
        }
        sessions.insert(item.as_str().to_owned(), session.to_owned());
    }

    fn play_session_for(&self, item: &str) -> Option<String> {
        self.play_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(item)
            .cloned()
    }

    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Result<Url, ProviderError> {
        build_url(&self.base, segments, query)
    }

    async fn get_bytes(&self, url: Url) -> Result<Vec<u8>, ProviderError> {
        // 普通端点没有令牌就不要再发请求了：直接报未认证。
        let header = self.auth_header(Some(&self.token()?))?;
        let name = self.flavor.auth_header_name();
        let response = self
            .send(
                || self.http.get(url.clone()).header(name, header.clone()),
                true,
            )
            .await?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(network_error)
    }

    /// `token` 为 `None` 时只带客户端信息头，用于不需要认证的公开端点。
    async fn get_bytes_with(
        &self,
        url: Url,
        token: Option<&SecretString>,
    ) -> Result<Vec<u8>, ProviderError> {
        let header = self.auth_header(token)?;
        let name = self.flavor.auth_header_name();
        let response = self
            .send(
                || self.http.get(url.clone()).header(name, header.clone()),
                true,
            )
            .await?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(network_error)
    }

    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T, ProviderError> {
        let bytes = self.get_bytes(url).await?;
        decode_json(&bytes)
    }

    /// 匿名（或仅带已有令牌）的 GET + 解码，用于 `/System/Info/Public` 这类公开端点：
    /// 未登录时也必须能拿到服务器名与版本，否则新用户根本看不到要登录的是哪台服务器。
    async fn get_json_public<T: DeserializeOwned>(&self, url: Url) -> Result<T, ProviderError> {
        let token = self.read_state().token.clone();
        let bytes = self.get_bytes_with(url, token.as_ref()).await?;
        decode_json(&bytes)
    }

    async fn post_json<B>(&self, url: Url, body: &B) -> Result<(), ProviderError>
    where
        B: Serialize + Sync,
    {
        let header = self.auth_header(Some(&self.token()?))?;
        let name = self.flavor.auth_header_name();
        self.send(
            || {
                self.http
                    .post(url.clone())
                    .header(name, header.clone())
                    .json(body)
            },
            false,
        )
        .await?;
        Ok(())
    }

    /// 取条目详情原始结构（`MediaSources` 也在里面）。
    async fn item_detail(&self, id: &ItemId) -> Result<RawItem, ProviderError> {
        let user_id = self.user_id().await?;
        let url = self.url(&["Users", &user_id, "Items", id.as_str()], &[])?;
        self.get_json(url).await
    }

    /// 左边是参数名、右边是参数值，统一转成 `build_url` 需要的借用切片。
    fn pairs(params: &[(String, String)]) -> Vec<(&str, &str)> {
        params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect()
    }
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 手写而不是 derive：`credentials` 不是 Debug，而令牌缓存干脆不出现。
        // 设备**名**是用户可控字符串（可能是真实姓名或主机名），只打印稳定 ID。
        f.debug_struct("Provider")
            .field("flavor", &self.flavor)
            .field("base_url", &self.display_base)
            .field("plaintext", &self.plaintext)
            .field("device_id", &self.device.id)
            .field("credential_key", &self.credential_key)
            .field("retry", &self.retry)
            .field("server_info", &self.read_state().info)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl MediaProvider for Provider {
    fn kind(&self) -> ServerKind {
        self.flavor.server_kind()
    }

    fn base_url(&self) -> &str {
        &self.display_base
    }

    fn server_info(&self) -> ServerInfo {
        self.read_state().info.clone()
    }

    async fn authenticate(
        &self,
        user: &str,
        password: &SecretString,
    ) -> Result<ServerInfo, ProviderError> {
        let url = self.url(&["Users", "AuthenticateByName"], &[])?;

        let mut body = BTreeMap::new();
        body.insert("Username", user);
        body.insert(self.flavor.password_field(), password.expose());

        // Emby 各版本的密码字段名不统一（4.10 只认 `Pw`，更早的只认 `Password`）。
        // 401 时换字段名重试一次；`send()` 已经把 401 转成 `Unauthorized` 错误，
        // 所以这里按错误类型判断，而不是看状态码。
        let response = match self.post_auth(&url, &body).await {
            Err(ProviderError::Unauthorized) => match self.flavor.legacy_password_field() {
                Some(legacy) => {
                    let mut legacy_body = BTreeMap::new();
                    legacy_body.insert("Username", user);
                    legacy_body.insert(legacy, password.expose());
                    tracing::debug!(field = legacy, "Emby 拒绝 Pw，改用旧字段名重试");
                    self.post_auth(&url, &legacy_body).await?
                }
                None => return Err(ProviderError::Unauthorized),
            },
            other => other?,
        };

        // 旧版 Emby 只在响应头里给令牌，两种来源都要收。
        let header_token = self
            .flavor
            .token_header_name()
            .and_then(|name| response.headers().get(name))
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        let bytes = response.bytes().await.map_err(network_error)?;
        let parsed: RawAuthenticationResult = serde_json::from_slice(&bytes)
            .map_err(|error| ProviderError::Decode(error.to_string()))?;

        let token = parsed
            .access_token
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty())
            .or(header_token)
            .ok_or(ProviderError::InvalidResponse("认证响应里没有令牌"))?;
        let token = SecretString::new(token);

        self.credentials
            .set(&self.credential_key, &token)
            .map_err(credentials_error)?;

        let user_name = parsed
            .user
            .as_ref()
            .and_then(|user| user.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| user.to_owned());
        let user_id = parsed
            .user
            .as_ref()
            .and_then(|user| user.id.clone())
            .filter(|id| !id.is_empty());
        // 服务端自称的 id 只做诊断用：它和本机分配的 ServerId 不是一回事。
        if let Some(server_id) = parsed.server_id.as_deref() {
            tracing::debug!(server_id, "认证成功");
        }

        let info = {
            // 拿到锁再写状态：避免一个正在跑的 `/Users/Me` 解析把旧用户标识写回来。
            let _resolving = self.user_id_resolve.lock().await;
            let mut state = self.write_state();
            state.token = Some(token);
            state.info.user_id = user_id;
            state.info.auth = AuthState::Authorized {
                user_name: user_name.clone(),
                credential_key: self.credential_key.clone(),
            };
            state.info.clone()
        };

        // 只记显示名，不记基址：后者是内网地址 / 主机名，日志会被贴进 issue。
        tracing::info!(server = %info.name, user = %user_name, "登录成功");

        // 顺带把只有公开接口才有的字段（服务器名、版本）填上。每个会话只在这里
        // 请求一次，其余端点不会重复打 `/System/Info/Public`。
        // 拿不到不影响登录：令牌已经进存储，会话已经可用。
        Ok(self.refresh_public_info(info).await)
    }

    async fn public_info(&self) -> Result<ServerInfo, ProviderError> {
        let url = self.url(&["System", "Info", "Public"], &[])?;
        let raw: RawPublicSystemInfo = self.get_json_public(url).await?;
        let current = self.read_state().info.clone();
        Ok(ServerInfo {
            id: current.id,
            kind: kind_from_public(
                raw.product_name.as_deref(),
                raw.local_addresses.is_some(),
                self.flavor.server_kind(),
            ),
            name: raw
                .server_name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(current.name),
            base_url: current.base_url,
            version: raw.version.filter(|version| !version.trim().is_empty()),
            // 这两个字段描述的是“当前会话”，不是服务器；公开接口也拿不到。
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: current.metadata_language,
        })
    }

    async fn libraries(&self) -> Result<Vec<Library>, ProviderError> {
        let user_id = self.user_id().await?;
        let url = self.url(&["Users", &user_id, "Views"], &[])?;
        let response: RawItemsResponse = self.get_json(url).await?;
        Ok(response.items.iter().filter_map(map_library).collect())
    }

    async fn items(&self, query: ItemQuery) -> Result<Page<MediaItem>, ProviderError> {
        let user_id = self.user_id().await?;

        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(parent) = &query.parent {
            params.push(("ParentId".to_owned(), parent.as_str().to_owned()));
        }
        // 总是带 `IncludeItemTypes`：不带的话服务端会把 `Folder`/`CollectionFolder` 之类
        // 也算进 `Limit` 与 `TotalRecordCount`，而映射层会把它们丢掉——用户看到的是
        // "`--limit 3` 只返回 1 条"（实测：真实 Emby 库 9111 条时就是这样）。
        // 调用方没指定类型时按"能渲染的类型"过滤，语义就是"给我能显示的东西"。
        let types: Vec<&str> = if query.kinds.is_empty() {
            DEFAULT_BROWSE_KINDS
                .iter()
                .copied()
                .map(kind_to_api)
                .collect()
        } else {
            query.kinds.iter().copied().map(kind_to_api).collect()
        };
        params.push(("IncludeItemTypes".to_owned(), types.join(",")));
        if let Some(term) = query.search.as_deref().map(str::trim) {
            if !term.is_empty() {
                params.push(("SearchTerm".to_owned(), term.to_owned()));
            }
        }
        if let Some(sort) = query.sort {
            params.push(("SortBy".to_owned(), sort.as_api().to_owned()));
            params.push(("SortOrder".to_owned(), "Ascending".to_owned()));
        }
        params.push(("Recursive".to_owned(), "true".to_owned()));
        // 实测（Jellyfin 12.4）：`Fields=MediaSources` 在**列表**接口上有效，会把
        // `Size` / `Container` / `MediaStreams` 一并带回。
        // 千万不要加 `RunTimeTicks`：它不在 `ItemFields` 枚举里，服务端静默忽略；
        // 列表里的 `MediaSources[].RunTimeTicks` 实测也是 `null`，时长只有详情与
        // `PlaybackInfo` 才有（所以列表里的 `MediaSource::runtime` 通常为 `None`）。
        params.push(("Fields".to_owned(), "MediaSources,ChildCount".to_owned()));
        if let Some(start) = query.start {
            params.push(("StartIndex".to_owned(), start.to_string()));
        }
        if let Some(limit) = query.limit {
            params.push(("Limit".to_owned(), limit.to_string()));
        }

        let url = self.url(&["Users", &user_id, "Items"], &Self::pairs(&params))?;
        let response: RawItemsResponse = self.get_json(url).await?;

        let items = map_items(&response.items, &self.base);
        let total = response
            .total_record_count
            .unwrap_or_else(|| u32::try_from(items.len()).unwrap_or(u32::MAX));
        Ok(Page {
            items,
            total,
            start: query.start.unwrap_or(0),
            limit: query.limit.unwrap_or(0),
        })
    }

    async fn item(&self, id: &ItemId) -> Result<MediaItem, ProviderError> {
        let raw = self.item_detail(id).await?;
        map_item(&raw, &self.base)
    }

    async fn children(&self, id: &ItemId) -> Result<Vec<MediaItem>, ProviderError> {
        let user_id = self.user_id().await?;
        let url = self.url(
            &["Users", &user_id, "Items"],
            &[
                ("ParentId", id.as_str()),
                ("Recursive", "false"),
                ("SortBy", "ParentIndexNumber,IndexNumber"),
                ("SortOrder", "Ascending"),
                ("Fields", "ChildCount"),
            ],
        )?;
        let response: RawItemsResponse = self.get_json(url).await?;
        Ok(map_items(&response.items, &self.base))
    }

    async fn sources(&self, id: &ItemId) -> Result<Vec<MediaSource>, ProviderError> {
        let user_id = self.user_id().await?;
        let url = self.url(
            &["Items", id.as_str(), "PlaybackInfo"],
            &[("UserId", user_id.as_str())],
        )?;
        let header = self.auth_header(Some(&self.token()?))?;
        let name = self.flavor.auth_header_name();
        // 真实服务端行为：容器条目（`Folder`、剧集、季）调 `PlaybackInfo` 直接回
        // `400`（部分版本回 `404`），而不是 `200` + 空数组。“没有媒体源”是个正常
        // 结果，当成错误会让界面把一个空库/剧集母页渲染成错误页。
        // 其它 4xx（尤其 401 / 403）仍然报错。
        let response = self
            .send_accepting(
                || self.http.get(url.clone()).header(name, header.clone()),
                true,
                &[StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND],
            )
            .await?;
        if !response.status().is_success() {
            tracing::debug!(
                status = response.status().as_u16(),
                "该条目没有 PlaybackInfo（容器条目）"
            );
            return Ok(Vec::new());
        }

        let bytes = response.bytes().await.map_err(network_error)?;
        let raw: RawPlaybackInfo = decode_json(&bytes)?;
        if let Some(session) = raw.play_session_id.as_deref().filter(|s| !s.is_empty()) {
            self.remember_play_session(id, session);
        }
        if !raw.media_sources.is_empty() {
            return Ok(map_sources(&raw.media_sources, id, &self.base));
        }

        // 有些服务端（旧版 Emby、以及按文件直读的库）在 PlaybackInfo 里返回空列表，
        // 真正可播的资源挂在条目详情上。补一次详情请求，而不是直接告诉上层"不可播放"。
        let detail = self.item_detail(id).await?;
        Ok(map_sources(&detail.media_sources, id, &self.base))
    }

    async fn stream_target(
        &self,
        item: &ItemId,
        source: &SourceId,
    ) -> Result<StreamTarget, ProviderError> {
        // 播放器只收一条裸地址，令牌只能走查询串——这是整个 crate 里唯一这么做的地方。
        let token = self.token()?;
        let url = self.url(
            &["Videos", item.as_str(), "stream"],
            &[
                ("Static", "true"),
                ("MediaSourceId", source.as_str()),
                ("api_key", token.expose()),
            ],
        )?;
        Ok(StreamTarget::new(
            url.into(),
            item.clone(),
            source.clone(),
            self.plaintext,
        ))
    }

    async fn report_started(&self, progress: &PlaybackProgress) -> Result<(), ProviderError> {
        let position_ticks = duration_to_ticks(progress.position);
        let body = ProgressReport {
            play_session_id: self.play_session_for(progress.item_id.as_str()),
            item_id: progress.item_id.as_str().to_owned(),
            media_source_id: progress.source_id.as_str().to_owned(),
            position_ticks,
            is_paused: false,
            is_muted: false,
            can_seek: true,
            play_method: play_method_to_api(progress.method),
        };
        let url = self.url(&["Sessions", "Playing"], &[])?;
        self.post_json(url, &body).await
    }

    async fn report_stopped(&self, progress: &PlaybackProgress) -> Result<(), ProviderError> {
        let body = ProgressReport {
            play_session_id: self.play_session_for(progress.item_id.as_str()),
            item_id: progress.item_id.as_str().to_owned(),
            media_source_id: progress.source_id.as_str().to_owned(),
            position_ticks: duration_to_ticks(progress.position),
            is_paused: true,
            is_muted: false,
            can_seek: false,
            play_method: play_method_to_api(progress.method),
        };
        let url = self.url(&["Sessions", "Playing", "Stopped"], &[])?;
        self.post_json(url, &body).await
    }

    async fn report_progress(&self, progress: &PlaybackProgress) -> Result<(), ProviderError> {
        let body = ProgressReport {
            play_session_id: self.play_session_for(progress.item_id.as_str()),
            item_id: progress.item_id.as_str().to_owned(),
            media_source_id: progress.source_id.as_str().to_owned(),
            position_ticks: duration_to_ticks(progress.position),
            is_paused: progress.paused,
            is_muted: false,
            can_seek: true,
            play_method: play_method_to_api(progress.method),
        };
        let url = self.url(&["Sessions", "Playing", "Progress"], &[])?;
        // 这里**不再**兼职发 Stopped：结束会话由 `report_stopped` 独占，
        // 否则"看完"和"退出"两条路径会各发一次，语义也混（旧实现只在 played 时发，
        // 结果正常退出永远不落盘，续播点恒为 0）。
        self.post_json(url, &body).await
    }

    async fn image(&self, request: ImageRequest) -> Result<Vec<u8>, ProviderError> {
        let mut params: Vec<(String, String)> = vec![("quality".to_owned(), "90".to_owned())];
        if let Some(width) = request.width {
            params.push(("maxWidth".to_owned(), width.to_string()));
        }
        if let Some(tag) = request.tag.as_deref().filter(|tag| !tag.is_empty()) {
            params.push(("tag".to_owned(), tag.to_owned()));
        }
        let url = self.url(
            &[
                "Items",
                request.item.as_str(),
                "Images",
                request.kind.as_api(),
            ],
            &Self::pairs(&params),
        )?;
        self.get_bytes(url).await
    }

    async fn logout(&self) -> Result<(), ProviderError> {
        self.credentials
            .delete(&self.credential_key)
            .map_err(credentials_error)?;
        // 与惰性解析争同一把锁：否则一个在飞的 `/Users/Me` 会在登出后
        // 把用户标识写回状态（令牌没了，标识却还在）。
        let _resolving = self.user_id_resolve.lock().await;
        let mut state = self.write_state();
        state.token = None;
        state.info.user_id = None;
        state.info.auth = AuthState::Unauthorized;
        Ok(())
    }
}

/// 上报进度 / 停止会话的请求体。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
struct ProgressReport {
    /// 没有会话标识时省略：服务端会把它当成"没有媒体信息"的会话而忽略，
    /// 但发一个空串更糟（有些版本会 400）。
    #[serde(skip_serializing_if = "Option::is_none")]
    play_session_id: Option<String>,
    item_id: String,
    media_source_id: String,
    position_ticks: i64,
    is_paused: bool,
    is_muted: bool,
    can_seek: bool,
    play_method: &'static str,
}

/// 调用方没指定类型时的默认过滤：这几种是界面真正能渲染的。
///
/// 不包含 `Folder`/`CollectionFolder`/`BoxSet`——它们会被映射层丢弃，带进来只会
/// 让 `Limit` 与 `TotalRecordCount` 虚高。
const DEFAULT_BROWSE_KINDS: [MediaKind; 4] = [
    MediaKind::Movie,
    MediaKind::Series,
    MediaKind::Season,
    MediaKind::Episode,
];

/// 构造 HTTP 客户端。
///
/// 这里**没有**也不会有 `danger_accept_invalid_certs`：证书校验交给 rustls 与平台根证书。
/// 连接阶段单独设超时：没有它的话，一个不回 SYN-ACK 的地址会一直卡到整体超时
/// （默认 30 秒），而“连不上”本来应该很快就有结果。
fn build_http_client(timeout: Duration) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .user_agent(format!("{CLIENT_NAME}/{CLIENT_VERSION}"))
        .connect_timeout(connect_timeout_for(timeout))
        .timeout(timeout)
        .build()
        .map_err(|error| ProviderError::Network(format!("无法初始化 HTTP 客户端：{error}")))
}

/// 连接超时：默认 [`CONNECT_TIMEOUT`]，但不超过整体超时。
///
/// 整体超时本身已经小于 [`MIN_CONNECT_TIMEOUT`] 时（只会出现在测试里）跟着整体超时走，
/// 否则连接超时反而会晚于整体超时，写这个值就没有意义了。
fn connect_timeout_for(timeout: Duration) -> Duration {
    if timeout < MIN_CONNECT_TIMEOUT {
        timeout
    } else {
        CONNECT_TIMEOUT.min(timeout)
    }
}

/// 网络错误的统一翻译。
///
/// `reqwest::Error` 的 `Display` 里带着完整请求地址（含查询串），
/// 所以先用 `without_url` 把它摘掉再取文本；分类信息单独给一份固定文案。
fn network_error(source: reqwest::Error) -> ProviderError {
    let timed_out = source.is_timeout();
    let connect = source.is_connect();
    let decode = source.is_decode();
    tracing::debug!(error = %source.without_url(), "HTTP 请求失败");
    let message = if timed_out {
        "请求超时"
    } else if connect {
        "无法连接服务器"
    } else if decode {
        "响应体解码失败"
    } else {
        "请求失败"
    };
    ProviderError::Network(message.to_owned())
}

/// JSON 解码错误的统一翻译。
fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProviderError> {
    serde_json::from_slice(bytes).map_err(|error| ProviderError::Decode(error.to_string()))
}

fn credentials_error(error: SecretError) -> ProviderError {
    ProviderError::Credentials {
        message: error.to_string(),
    }
}

/// 认证头里的值只允许可见 ASCII（含空格），且不能含引号或反斜杠。
///
/// 引号会提前闭合 `Key="Value"`，反斜杠是各实现用来转义引号的符号，
/// 两者都能让服务端把后面的内容读成另一个字段。控制字符与换行更直接：
/// 它们会切开请求头。非 ASCII 字符虽然在引号里，但 HTTP 头只允许 ASCII，
/// 放进去只会得到 `HeaderValue` 编码失败的错误，不如早点指出是哪个字段。
///
/// 返回的错误只带字段名（静态字符串），不会回显字段值——设备名、令牌都可能是敏感内容。
fn checked_header_value<'a>(value: &'a str, field: &'static str) -> Result<&'a str, ProviderError> {
    let legal = value
        .chars()
        .all(|ch| ch == ' ' || (ch.is_ascii_graphic() && !matches!(ch, '"' | '\\')));
    if legal {
        Ok(value)
    } else {
        Err(ProviderError::Unsupported(field))
    }
}

const CLIENT_FIELD: &str = "客户端名包含不允许的字符（只允许 ASCII 可见字符，不能含引号或反斜杠）";
const DEVICE_NAME_FIELD: &str =
    "设备名包含不允许的字符（只允许 ASCII 可见字符，不能含引号或反斜杠）";
const DEVICE_ID_FIELD: &str =
    "设备 ID 包含不允许的字符（只允许 ASCII 可见字符，不能含引号或反斜杠）";
const TOKEN_FIELD: &str = "认证令牌包含不允许的字符（只允许 ASCII 可见字符，不能含引号或反斜杠）";
const USER_ID_SEGMENT_FIELD: &str = "服务端返回的用户标识不能用于请求路径";

/// 响应状态是否值得在**本层**重试（仅对幂等 GET 生效）。
///
/// 429 与服务端暂时不可用同义：服务端明确说了“现在别来”，等一会儿再来是对的；
/// 其余 4xx 是“你的请求有问题”，重试只会重复错误。
fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

/// 读 `Retry-After`，解析不了就当没有。
fn retry_after_delay(headers: &HeaderMap) -> Option<Duration> {
    let raw = headers.get(RETRY_AFTER)?.to_str().ok()?;
    let delay = parse_retry_after(raw, std::time::SystemTime::now());
    if delay.is_none() {
        tracing::debug!("Retry-After 无法解析，退回指数退避");
    }
    delay
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_secrets::MemoryStore;

    fn config() -> ProviderConfig {
        ProviderConfig::new(ServerId::new("srv-1"), "http://127.0.0.1:8096")
    }

    fn provider() -> Provider {
        Provider::new(Flavor::Jellyfin, config(), Arc::new(MemoryStore::new()))
            .expect("合法地址应当能构造")
    }

    #[test]
    fn rejects_unsupported_base_urls() {
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "https://", ""] {
            let mut config = config();
            config.base_url = bad.to_owned();
            let error = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
                .expect_err(&format!("{bad:?} 应当构造失败"));
            assert!(
                matches!(error, ProviderError::Unsupported(_)),
                "{bad:?} → {error:?}"
            );
        }
    }

    #[test]
    fn credential_key_follows_flavor_and_server_id() {
        let jellyfin = provider();
        assert_eq!(jellyfin.credential_key(), "jellyfin/srv-1/token");
        let emby =
            Provider::new(Flavor::Emby, config(), Arc::new(MemoryStore::new())).expect("应当构造");
        assert_eq!(emby.credential_key(), "emby/srv-1/token");
    }

    #[test]
    fn base_url_is_normalized_and_plaintext_is_flagged() {
        let provider = provider();
        assert_eq!(provider.base_url(), "http://127.0.0.1:8096");
        assert!(provider.is_plaintext());
        let info = provider.server_info();
        assert!(info.is_plaintext());
        assert_eq!(info.kind, ServerKind::Jellyfin);
        assert_eq!(info.name, "127.0.0.1", "没有显示名时用主机名");
        assert_eq!(info.auth, AuthState::Unauthorized);

        let secure = Provider::new(
            Flavor::Jellyfin,
            ProviderConfig::new(ServerId::new("s"), "https://media.example.com/emby/"),
            Arc::new(MemoryStore::new()),
        )
        .expect("应当构造");
        assert!(!secure.is_plaintext());
        assert_eq!(secure.base_url(), "https://media.example.com/emby");
    }

    #[test]
    fn existing_token_in_store_marks_state_authorized() {
        let store = Arc::new(MemoryStore::new());
        store
            .set("jellyfin/srv-1/token", &SecretString::new("stored-token"))
            .expect("内存存储不会失败");
        let provider = Provider::new(
            Flavor::Jellyfin,
            config(),
            Arc::clone(&store) as Arc<dyn CredentialStore>,
        )
        .expect("应当构造");
        let info = provider.server_info();
        assert!(info.auth.is_authorized());
        assert_eq!(
            info.auth,
            AuthState::Authorized {
                user_name: String::new(),
                credential_key: "jellyfin/srv-1/token".to_owned(),
            }
        );
    }

    #[test]
    fn debug_never_prints_the_token() {
        let store = Arc::new(MemoryStore::new());
        store
            .set(
                "jellyfin/srv-1/token",
                &SecretString::new("SUPER-SECRET-TOKEN-VALUE"),
            )
            .expect("内存存储不会失败");
        let provider = Provider::new(
            Flavor::Jellyfin,
            config(),
            Arc::clone(&store) as Arc<dyn CredentialStore>,
        )
        .expect("应当构造");
        let debug = format!("{provider:?}");
        assert!(
            !debug.contains("SUPER-SECRET-TOKEN-VALUE"),
            "Debug 泄漏令牌：{debug}"
        );
        assert!(debug.contains("jellyfin/srv-1/token"));
        assert!(debug.contains("flavor: Jellyfin"));
    }

    #[test]
    fn auth_header_matches_the_media_browser_format() {
        let provider = provider();
        let header = provider
            .auth_header(Some(&SecretString::new("tok123")))
            .expect("应当能构造头部");
        let text = header.to_str().expect("应当是可读文本");
        assert_eq!(
            text,
            format!(
                "MediaBrowser Client=\"Nova\", Device=\"Nova\", DeviceId=\"nova-srv-1\", Version=\"{CLIENT_VERSION}\", Token=\"tok123\""
            )
        );

        let anonymous = provider.auth_header(None).expect("应当能构造头部");
        let text = anonymous.to_str().expect("应当是可读文本");
        assert!(!text.contains("Token="), "登录请求不该带 Token：{text}");
    }

    #[test]
    fn auth_header_rejects_header_breaking_values() {
        // 严格要求而不是静默清洗：设备名被“洗干净”了而没人知道，反而是更坏的结果。
        for (name, id) in [
            ("客厅\"的 Mac", "dev-1"),
            ("客厅的 Mac\\mini", "dev-1"),
            ("带换行的名字\r\nX-Evil: 1", "dev-1"),
            ("客㕔的 Mac", "dev-1"),
            ("正常名字", "dev\"1"),
            ("正常名字", "dev\n1"),
        ] {
            let mut config = config();
            config.device = DeviceInfo::new(name, id);
            let provider = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
                .expect("构造不该失败，校验发生在拼头部时");
            let error = provider
                .auth_header(None)
                .expect_err(&format!("设备名/ID {name:?}/{id:?} 必须被拒绝"));
            assert!(
                matches!(error, ProviderError::Unsupported(_)),
                "{name:?}/{id:?} → {error:?}"
            );
            let text = error.to_string();
            assert!(
                text.contains("设备名") || text.contains("设备 ID"),
                "错误文案应当指明字段：{text}"
            );
        }

        // 合法的名字（含空格、逗号）原样进头部，不被改写。
        let mut config = config();
        config.device = DeviceInfo::new("Living Room, Mac mini", "dev-1");
        let provider = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
            .expect("应当构造");
        let header = provider.auth_header(None).expect("应当能构造头部");
        let text = header.to_str().expect("应当是可读文本");
        assert!(
            text.contains("Device=\"Living Room, Mac mini\""),
            "引号内的逗号是合法字面量：{text}"
        );
        assert_eq!(text.matches('"').count(), 8, "不能多造引号：{text}");
    }

    #[test]
    fn auth_header_rejects_header_breaking_tokens() {
        let provider = provider();
        let error = provider
            .auth_header(Some(&SecretString::new("tok\"en")))
            .expect_err("令牌里的引号必须被拒绝");
        assert!(matches!(error, ProviderError::Unsupported(_)));
        let text = error.to_string();
        assert!(text.contains("令牌"), "错误文案应当指明字段：{text}");
        assert!(!text.contains("tok"), "错误文案不能回显令牌：{text}");
    }

    #[tokio::test]
    async fn user_id_rejects_traversal_segments_and_control_chars() {
        let provider = provider();
        for bad in ["..", ".", "a\rb", "a\0b"] {
            provider.write_state().info.user_id = Some(bad.to_owned());
            let error = provider
                .user_id()
                .await
                .expect_err(&format!("用户标识 {bad:?} 必须被拒绝"));
            assert!(
                matches!(error, ProviderError::InvalidResponse(_)),
                "{bad:?} → {error:?}"
            );
        }
        provider.write_state().info.user_id = Some("normal-id".to_owned());
        assert_eq!(
            provider.user_id().await.expect("合法标识应当能用"),
            "normal-id"
        );
    }

    #[test]
    fn debug_prints_the_device_id_but_not_its_name() {
        let mut config = config();
        // 设备名是用户可控字符串，可能带主机名、真实姓名之类的内容。
        config.device = DeviceInfo::new("SECRET-DEVICE-NAME-alice-mbp", "nova-dev-1");
        let provider = Provider::new(Flavor::Jellyfin, config, Arc::new(MemoryStore::new()))
            .expect("应当构造");
        let debug = format!("{provider:?}");
        assert!(
            !debug.contains("SECRET-DEVICE-NAME"),
            "Debug 不该打印设备名：{debug}"
        );
        assert!(debug.contains("nova-dev-1"), "应当保留设备 ID：{debug}");
    }

    #[tokio::test]
    async fn unauthenticated_calls_fail_fast() {
        let provider = provider();
        // 没有令牌时惰性解析也不该发请求：直接就是未认证。
        let error = provider
            .user_id()
            .await
            .expect_err("没有 user_id 必须报未认证");
        assert!(matches!(error, ProviderError::Unauthorized));
        let error = provider.token().expect_err("没有令牌必须报未认证");
        assert!(matches!(error, ProviderError::Unauthorized));
    }

    #[test]
    fn connect_timeout_is_bounded_by_the_request_timeout() {
        // 默认：5 秒，远小于 30 秒的整体超时。
        assert_eq!(
            connect_timeout_for(DEFAULT_REQUEST_TIMEOUT),
            CONNECT_TIMEOUT
        );
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
        // 整体超时更短时不能超过它（否则连接超时永远轮不到生效）。
        assert_eq!(
            connect_timeout_for(Duration::from_secs(2)),
            Duration::from_secs(2)
        );
        // 至少 1 秒；整体超时本身小于 1 秒（只会出现在测试里）时跟着它走。
        assert_eq!(
            connect_timeout_for(Duration::from_secs(1)),
            MIN_CONNECT_TIMEOUT
        );
        assert_eq!(
            connect_timeout_for(Duration::from_millis(200)),
            Duration::from_millis(200)
        );
        for timeout in [
            Duration::from_millis(1),
            Duration::from_millis(200),
            Duration::from_secs(1),
            Duration::from_secs(3),
            Duration::from_secs(30),
            Duration::from_secs(600),
        ] {
            let connect = connect_timeout_for(timeout);
            assert!(
                connect <= timeout,
                "连接超时 {connect:?} 不能超过整体超时 {timeout:?}"
            );
            assert!(
                connect >= MIN_CONNECT_TIMEOUT || connect == timeout,
                "连接超时 {connect:?} 既没有下限也没有跟着整体超时"
            );
        }
    }

    #[test]
    fn build_http_client_accepts_both_timeout_extremes() {
        // 构造本身不该因为超时值而失败（参数校验都在 connect_timeout_for 里）。
        assert!(build_http_client(DEFAULT_REQUEST_TIMEOUT).is_ok());
        assert!(build_http_client(Duration::from_millis(50)).is_ok());
    }

    #[test]
    fn pairs_flattens_owned_params() {
        let params = vec![
            ("A".to_owned(), "1".to_owned()),
            ("B".to_owned(), "2".to_owned()),
        ];
        assert_eq!(Provider::pairs(&params), vec![("A", "1"), ("B", "2")]);
    }
}
