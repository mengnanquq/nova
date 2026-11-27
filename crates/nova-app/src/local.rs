// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 本地状态与会话：SQLite + 凭证 + provider + 图片缓存。
//!
//! 这一层对应 `nova-cli` 里的 `App`：**服务器选择、provider 构造、会话恢复**都按
//! 同样的规则做，保证"命令行登录过"就等价于"桌面端已登录"：
//!
//! - 服务器来自 `nova.db.servers()`；只有一台时直接用它，多台且没给 `--server`
//!   时降级为离线状态（界面仍可浏览缓存）；
//! - provider 用 `ProviderConfig::with_user_id(ServerInfo.user_id)` 构造，
//!   令牌交给 `nova-secrets`，桌面端自己从不保存明文；
//! - 令牌缺失/过期只改变 [`Status`]，不阻止启动，也不清空缓存。
//!
//! 所有数据库操作都是同步的；async 上下文里由 [`crate::refresh`] 放进
//! `spawn_blocking`，避免阻塞 tokio 工作线程。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use nova_core::{
    AuthState, MediaItem, MediaKind, MediaSource, ServerId, ServerKind, UserPreferences,
};
use nova_images::ImageCache;
use nova_provider::{DeviceInfo, Flavor, Library, MediaProvider, Provider, ProviderConfig};
use nova_secrets::CredentialStore;

/// 「首页」标签的哨兵 id（与真实库 id 不会冲突）。
pub const HOME_TAB_ID: &str = "@home";
use nova_storage::{Database, StoredServer};
use nova_ui::{
    DetailView, HomeView, ImageKey, LibraryTab, LibraryView, PlayerShellView, SearchView,
};

use crate::env::Env;
use crate::error::AppError;
use crate::images::{IMAGE_CACHE_BUDGET, ImageSpec};
use crate::status::Status;
use crate::views;

/// 本地状态：一次启动内共享（`Arc<Local>`）。
pub struct Local {
    /// 状态目录。
    pub data_dir: PathBuf,
    /// SQLite 句柄（与 CLI 共用同一个 `nova.db`）。
    pub db: Arc<Database>,
    /// 图片磁盘缓存（`<data>/images`）。
    pub image_cache: Arc<ImageCache>,
    /// 凭证存储。
    pub credentials: Arc<dyn CredentialStore>,
    /// 令牌是否会持久化。
    pub credentials_persistent: bool,
    /// 凭证后端退化说明。
    pub credential_note: Option<String>,
    /// 用户偏好（启动时读一次）。
    pub preferences: UserPreferences,
    /// 选中的服务器；没有服务器时为 `None`。
    ///
    /// 加锁是为了支持应用内登录：`adopt_server` 通过 `&self` 热替换会话，
    /// 与只读的视图构建互不竞争。
    pub server: Mutex<Option<StoredServer>>,
    /// 当前会话的 provider；未登录/地址不合规时为 `None`。
    pub provider: Mutex<Option<Arc<Provider>>>,
    status: Mutex<Status>,
    /// 状态变化回调（窗口模式用它把新状态投给界面）。
    status_listener: Mutex<Option<StatusListener>>,
    libraries: Mutex<Vec<Library>>,
}

/// 状态变化回调：参数是刚刚生效的 [`Status`]。
type StatusListener = Arc<dyn Fn(&Status) + Send + Sync>;

impl Local {
    /// 打开数据目录、数据库、图片缓存，并尽力构造 provider。
    ///
    /// 任何"没有服务器 / 没有令牌 / 令牌过期"的情况都只表现为 [`Status`] 的差异，
    /// 不返回错误——离线浏览是设计目标。
    pub fn open(env: &Env, requested_server: Option<&str>) -> Result<Self, AppError> {
        std::fs::create_dir_all(&env.data_dir)?;
        let db_path = env.data_dir.join("nova.db");
        let db = Arc::new(Database::open(&db_path)?);
        let preferences = db.preferences()?;

        let credentials = crate::env::open_credentials(env.credentials);
        if let Some(note) = &credentials.note {
            tracing::info!(note, "凭证存储退化");
        }
        let servers = db.servers()?;
        let (server, mut status) = select_server(servers, requested_server)?;

        let mut provider = None;
        if let Some(stored) = &server {
            match build_provider(stored, &credentials.store) {
                Ok(built) => {
                    let token_present = credentials
                        .store
                        .get(built.credential_key())
                        .ok()
                        .flatten()
                        .is_some_and(|token| !token.is_empty());
                    status = if token_present {
                        // 只要凭证存储里有令牌就按已连接启动，交给后台刷新去验证：
                        // 库里的 `Expired` 可能是上次真的 401，也可能是“本地曾经
                        // 没有令牌”被误写下的。刷新成功会写回 `Authorized`，真 401
                        // 再写回 `Expired`。
                        Status::Connected {
                            server: stored.info.name.clone(),
                            base_url: stored.info.base_url.clone(),
                        }
                    } else if matches!(stored.info.auth, AuthState::Expired { .. }) {
                        Status::Expired {
                            url: stored.info.base_url.clone(),
                        }
                    } else {
                        Status::MissingToken {
                            url: stored.info.base_url.clone(),
                            note: credentials.note.clone(),
                        }
                    };
                    provider = Some(Arc::new(built));
                }
                Err(error) => {
                    tracing::warn!(%error, "无法构造 provider，退化为离线浏览");
                    status = Status::RefreshFailed {
                        message: format!("无法构造服务端会话：{error}"),
                    };
                }
            }
        }

        let cache_dir = env.data_dir.join("images");
        let image_cache = Arc::new(ImageCache::new(
            cache_dir,
            Arc::clone(&db),
            IMAGE_CACHE_BUDGET,
        )?);
        match image_cache.evict() {
            Ok(freed) if freed > 0 => tracing::info!(freed, "启动时淘汰了超出预算的图片缓存"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "启动时清理图片缓存失败"),
        }

        // 媒体库列表是持久化的（schema v3）：离线启动时标签仍是真实库名，
        // 而不是靠"缓存里有条目就合成一个「全部」"。旧库里还没写过列表时
        // `tabs()` 会继续用那个兜底。
        let libraries = match &server {
            Some(stored) => match db.libraries(&stored.info.id) {
                Ok(libraries) => libraries.into_iter().map(library_from_stored).collect(),
                Err(error) => {
                    tracing::warn!(%error, "读取媒体库列表失败，离线标签退化为「全部」");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        Ok(Self {
            data_dir: env.data_dir.clone(),
            db,
            image_cache,
            credentials: credentials.store,
            credentials_persistent: credentials.persistent,
            credential_note: credentials.note,
            preferences,
            server: Mutex::new(server),
            provider: Mutex::new(provider),
            status: Mutex::new(status),
            status_listener: Mutex::new(None),
            libraries: Mutex::new(libraries),
        })
    }

    /// 当前选中的服务器快照（应用内登录可热替换）。
    pub fn server(&self) -> Option<StoredServer> {
        lock(&self.server).clone()
    }

    /// 当前会话的 provider（应用内登录可热替换）。
    pub fn provider(&self) -> Option<Arc<Provider>> {
        lock(&self.provider).clone()
    }

    /// 当前连接状态。
    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    /// 覆盖连接状态（刷新失败、令牌过期时用）。
    ///
    /// 注册过监听器时会把新状态通知出去：令牌过期不应该等下一次启动才变成横幅。
    pub fn set_status(&self, status: Status) {
        *lock(&self.status) = status.clone();
        let listener = lock(&self.status_listener).clone();
        if let Some(listener) = listener {
            // 在锁外调用：监听器会去投递界面消息，不能持着状态锁。
            listener(&status);
        }
    }

    /// 注册状态变化回调（窗口模式把它投给界面；快照 / 打印模式不注册）。
    ///
    /// 只保留最新注册的那一个：同一个进程只开一个窗口。
    pub fn set_status_listener(&self, listener: StatusListener) {
        *lock(&self.status_listener) = Some(listener);
    }

    /// 当前已知的服务端媒体库（刷新后才有；启动时从 SQLite 读回）。
    pub fn libraries(&self) -> Vec<Library> {
        lock(&self.libraries).clone()
    }

    /// 记录刷新拿到的媒体库列表，并持久化（schema v4 起连**服务端顺序**一起存）。
    ///
    /// 以前只放内存：进程一退，离线启动就只剩一个合成的「全部」标签，真实库名
    /// 全部丢失。写入失败只记日志（内存里仍是新的，下一次刷新还会重试）。
    /// 顺序按传入切片（`/Views` 的服务端顺序）写进 `position` 列，读回保持同一顺序
    /// （真机 R6-8：以前按显示名读回，刷新后顶栏标签会重排）。
    pub fn set_libraries(&self, libraries: Vec<Library>) {
        if let Some(server) = self.server() {
            let stored: Vec<nova_storage::StoredLibrary> =
                libraries.iter().map(library_to_stored).collect();
            if let Err(error) = self.db.replace_libraries(&server.info.id, &stored) {
                tracing::warn!(
                    %error,
                    server = server.info.id.as_str(),
                    "媒体库列表写入本地库失败（离线标签可能退化）"
                );
            }
        }
        *lock(&self.libraries) = libraries;
    }

    /// 顶部导航标签。
    ///
    /// 刷新前没有服务端媒体库记录（SQLite 里没有 libraries 表），此时只要缓存里有内容，
    /// 就合成一个「全部」标签；「首页」标签始终在第一位。
    pub fn tabs(&self) -> Vec<LibraryTab> {
        // 第一个标签永远是「首页」：库标签是刷新之后才出现的，如果没有这个入口，
        // 用户进了媒体库就再也回不到首页（实测：刷新完成时首页入口直接消失）。
        // 用独立哨兵 id 而不是空串——空串在离线分支里表示合成出来的「全部」库，
        // 两者必须区分，否则 `library("")` 会拿到首页标签。
        let mut tabs = vec![LibraryTab::new(HOME_TAB_ID, "首页")];
        let libraries = self.libraries();
        if !libraries.is_empty() {
            tabs.extend(
                libraries
                    .iter()
                    .map(|library| LibraryTab::new(library.id.as_str(), library.name.clone())),
            );
            return tabs;
        }
        match self.server() {
            Some(server) if self.db.item_count(&server.info.id).unwrap_or(0) > 0 => {
                tabs.push(LibraryTab::new("", "全部"));
                tabs
            }
            _ => tabs,
        }
    }

    /// 用缓存构建首页（离线状态由顶部横幅表达，hero 始终是真实内容）。
    pub fn home(&self, search: Option<&str>) -> Result<HomeView, AppError> {
        let status = self.status();
        let tabs = self.tabs();
        match self.server() {
            Some(server) => Ok(views::home_view(
                &self.db,
                &server.info.id,
                &self.preferences,
                &status,
                tabs,
                search,
            )?),
            None => Ok(HomeView::empty(tabs)),
        }
    }

    /// 用缓存构建媒体库页。
    ///
    /// `sort` 只影响本地缓存的条目顺序（见 [`views::LibrarySort`]），不参与查询。
    pub fn library(
        &self,
        library_id: &str,
        search: Option<&str>,
        sort: views::LibrarySort,
    ) -> Result<LibraryView, AppError> {
        let tabs = self.tabs();
        let tab = tabs
            .iter()
            .find(|tab| tab.id == library_id)
            .cloned()
            .unwrap_or_else(|| {
                let title = if library_id.is_empty() {
                    "全部"
                } else {
                    library_id
                };
                LibraryTab::new(library_id, title)
            });
        let Some(server) = self.server() else {
            return Ok(LibraryView {
                id: tab.id.clone(),
                title: tab.title.clone(),
                library_tabs: tabs,
                ..LibraryView::default()
            });
        };
        Ok(views::library_view(
            &self.db,
            &server.info.id,
            &tab,
            tabs,
            search,
            sort,
        )?)
    }

    /// 用缓存构建搜索结果页（离线可用；输入时的即时反馈也走这里）。
    pub fn search_view(&self, term: &str, limit: usize) -> Result<SearchView, AppError> {
        match self.server() {
            Some(server) => Ok(views::search_view(&self.db, &server.info.id, term, limit)?),
            None => Ok(SearchView::from_cards(term, Vec::new())),
        }
    }

    /// 本地缓存里匹配 `term` 的条目总数（不受搜索上限截断，R6-10）。
    ///
    /// 服务端搜索只返回前 `limit` 条，且实测 `TotalRecordCount` 也可能等于
    /// 返回条数；结果头要提示“显示前 N 部”就需要一个可信的总数下限，
    /// 本地缓存是现成的（离线时也是唯一来源）。
    pub fn search_count(&self, term: &str) -> usize {
        let Some(server) = self.server() else {
            return 0;
        };
        self.db
            .search_item_count(&server.info.id, views::search_kinds(), term)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, term, "本地搜索计数失败");
                0
            })
    }

    /// 用缓存构建详情页。
    pub fn detail(&self, item_id: &str) -> Result<Option<DetailView>, AppError> {
        let Some(server) = self.server() else {
            return Ok(None);
        };
        Ok(views::detail_view(&self.db, &server.info.id, item_id)?)
    }

    /// 切换「我的列表」的本地状态，返回切换后的状态。
    pub fn toggle_mylist(&self, item_id: &str) -> Result<bool, AppError> {
        let Some(server) = self.server() else {
            return Err(AppError::Config("还没有连接服务器".to_string()));
        };
        Ok(self
            .db
            .toggle_mylist(&server.info.id, &nova_core::ItemId::new(item_id))?)
    }

    /// 用缓存构建播放外壳。
    pub fn player(&self, item_id: &str) -> Result<Option<PlayerShellView>, AppError> {
        let Some(server) = self.server() else {
            return Ok(None);
        };
        Ok(views::player_view(&self.db, &server.info.id, item_id)?)
    }

    /// 界面图片键 → 具体请求（条目没有这张图时返回 `None`）。
    pub fn image_spec(&self, key: &ImageKey) -> Option<ImageSpec> {
        let server = self.server()?;
        let item = match self.db.item(
            &server.info.id,
            &nova_core::ItemId::new(key.item_id.clone()),
        ) {
            Ok(Some(item)) => item,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(%error, "读取条目失败，跳过图片");
                return None;
            }
        };
        ImageSpec::from_key(key, &item)
    }

    /// 写条目缓存（幂等批量）。
    pub fn cache_items(&self, items: &[MediaItem]) -> Result<(), AppError> {
        let Some(server) = self.server() else {
            return Ok(());
        };
        self.db
            .upsert_items(&server.info.id, items, nova_storage::unix_now())?;
        Ok(())
    }

    /// 写搜索结果条目（幂等批量）。
    ///
    /// 与 [`Local::cache_items`]（列表刷新，媒体库归属的权威来源）的区别：搜索结果里
    /// 的 `ParentId` 是服务端的**物理父项**（Emby 可能是子文件夹 / 合集，搜索响应
    /// 甚至不带 `ParentId`），拿它覆盖缓存会让条目从当前媒体库消失（体检 R4-3）。
    pub fn cache_search_items(&self, items: &[MediaItem]) -> Result<(), AppError> {
        let Some(server) = self.server() else {
            return Ok(());
        };
        let items: Vec<MediaItem> = items
            .iter()
            .map(|item| keep_library_membership(&self.db, &server.info.id, item))
            .collect();
        self.db
            .upsert_items(&server.info.id, &items, nova_storage::unix_now())?;
        Ok(())
    }

    /// 写单条条目与它的可播放版本（详情页按需刷新用）。
    ///
    /// 详情接口给的 `ParentId` 是物理父项，不是媒体库归属；直接覆盖缓存会让条目
    /// 从当前媒体库消失，返回时同一下标换成另一部片（体检 R4-3）。
    pub fn cache_item_and_sources(
        &self,
        item: &MediaItem,
        sources: &[MediaSource],
    ) -> Result<(), AppError> {
        let Some(server) = self.server() else {
            return Ok(());
        };
        let item = keep_library_membership(&self.db, &server.info.id, item);
        self.db.upsert_items(
            &server.info.id,
            std::slice::from_ref(&item),
            nova_storage::unix_now(),
        )?;
        self.db.upsert_sources(&server.info.id, sources)?;
        Ok(())
    }

    /// 把 provider 当前看到的服务器信息写回本地库（与 CLI 的 `persist_info` 同规则）。
    ///
    /// 关键点：从令牌恢复的会话里 `user_name` 是空串，不能用它覆盖登录时存下的名字。
    /// 收 `&dyn MediaProvider`：刷新路径只需要 `server_info()`，不必要求具体类型。
    pub fn persist_info(&self, provider: &dyn MediaProvider) -> Result<(), AppError> {
        let mut info = provider.server_info();
        let stored = self.db.server(&info.id)?;
        if let Some(stored) = &stored {
            if info.auth.user_name().is_some_and(str::is_empty) {
                if let Some(name) = stored
                    .info
                    .auth
                    .user_name()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                {
                    info.auth = match &info.auth {
                        AuthState::Authorized { credential_key, .. } => AuthState::Authorized {
                            user_name: name,
                            credential_key: credential_key.clone(),
                        },
                        other => other.clone(),
                    };
                }
            }
        }
        let changed = match &stored {
            Some(stored) => {
                stored.info.user_id != info.user_id
                    || stored.info.name != info.name
                    || stored.info.version != info.version
                    || stored.info.kind != info.kind
                    || stored.info.auth != info.auth
            }
            None => true,
        };
        if changed {
            self.db.upsert_server(&info, nova_storage::unix_now())?;
            tracing::debug!(server = info.id.as_str(), "服务器信息有变化，已写回本地库");
        }
        Ok(())
    }

    /// 应用内登录成功后接管新会话：写 SQLite、替换 provider 与选中服务器。
    ///
    /// 与 CLI 的 `persist_info` 同规则：数据库是主记录；`created_at` 已存在时保持
    /// 不变。成功后状态置为 [`Status::Connected`]，媒体库列表清空（等刷新重建）。
    pub fn adopt_server(
        &self,
        provider: Provider,
        info: &nova_core::ServerInfo,
    ) -> Result<(), AppError> {
        let now = nova_storage::unix_now();
        // 先读旧行拿 created_at（upsert 不改它），再写新信息。
        let created_at = self
            .db
            .server(&info.id)?
            .map(|stored| stored.created_at)
            .unwrap_or(now);
        self.db.upsert_server(info, now)?;
        *lock(&self.server) = Some(nova_storage::StoredServer {
            info: info.clone(),
            created_at,
            last_seen_at: now,
        });
        *lock(&self.provider) = Some(Arc::new(provider));
        self.set_libraries(Vec::new());
        self.set_status(Status::Connected {
            server: info.name.clone(),
            base_url: info.base_url.clone(),
        });
        Ok(())
    }

    /// 标记会话过期并写回 SQLite，让下次启动直接显示“登录已过期”。
    pub fn mark_expired(&self) {
        let Some(server) = self.server() else {
            return;
        };
        let user_name = self
            .provider()
            .and_then(|provider| provider.server_info().auth.user_name().map(str::to_owned))
            .or_else(|| server.info.auth.user_name().map(str::to_owned))
            .unwrap_or_default();
        let mut info = server.info.clone();
        info.auth = AuthState::Expired {
            user_name: user_name.clone(),
        };
        if let Err(error) = self.db.upsert_server(&info, nova_storage::unix_now()) {
            tracing::warn!(%error, "写回过期状态失败");
        }
        self.set_status(Status::Expired {
            url: info.base_url.clone(),
        });
        tracing::warn!(server = info.id.as_str(), user = user_name, "会话已过期");
    }

    /// 以 trait object 形式取 provider（图片下载适配器只需要 `MediaProvider`）。
    pub fn provider_dyn(&self) -> Option<Arc<dyn MediaProvider>> {
        self.provider()
            .map(|provider| provider as Arc<dyn MediaProvider>)
    }

    /// 服务器标识（测试与诊断用）。
    pub fn server_id(&self) -> Option<ServerId> {
        self.server().map(|server| server.info.id)
    }

    /// 数据目录里的数据库路径。
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("nova.db")
    }

    /// 图片缓存目录。
    pub fn images_dir(&self) -> &Path {
        self.image_cache.dir()
    }
}

/// 详情 / 搜索结果写缓存时，保住列表刷新（`refresh`）建立的**媒体库归属**。
///
/// `MediaItem.parent_id` 在本应用里同时承担两件事：播放层级（集→季→剧集）与
/// 媒体库归属（电影 / 剧集→库）。列表刷新按 `ParentId=<库>` 查询，写下的归属就是
/// 事实；详情 / 搜索接口给的却是服务端的物理父项（Emby 里可能是子文件夹或合集，
/// 搜索响应甚至不带 `ParentId`）。直接覆盖会让条目从当前媒体库消失：返回库时
/// `scroll_y` 没变、同一下标却换成了另一部片（体检 R4-3）。
///
/// 季 / 集是真正的层级，仍跟随服务端（不能被这条规则挡住）。
fn keep_library_membership(db: &Database, server: &ServerId, item: &MediaItem) -> MediaItem {
    let mut item = item.clone();
    if !matches!(item.kind, MediaKind::Movie | MediaKind::Series) {
        return item;
    }
    if let Ok(Some(cached)) = db.item(server, &item.id)
        && cached.parent_id.is_some()
    {
        if cached.parent_id != item.parent_id {
            tracing::debug!(
                item = item.id.as_str(),
                cached = ?cached.parent_id,
                fresh = ?item.parent_id,
                "详情 / 搜索的父项与库归属不同，保留库归属"
            );
        }
        item.parent_id = cached.parent_id;
    }
    item
}

/// 选出要操作的服务器；规则与 CLI 一致。
fn select_server(
    servers: Vec<StoredServer>,
    requested: Option<&str>,
) -> Result<(Option<StoredServer>, Status), AppError> {
    if let Some(id) = requested {
        return match servers
            .into_iter()
            .find(|server| server.info.id.as_str() == id)
        {
            Some(server) => Ok((Some(server), Status::NoServer)),
            None => Err(AppError::Config(format!(
                "本机没有 id 为 `{id}` 的服务器（用 `nova servers` 查看）"
            ))),
        };
    }
    match servers.len() {
        0 => Ok((None, Status::NoServer)),
        1 => Ok((servers.into_iter().next(), Status::NoServer)),
        count => Ok((None, Status::AmbiguousServer { count })),
    }
}

/// 构造 provider；设备标识按服务器派生，与 CLI 保持同一台"设备"。
fn build_provider(
    server: &StoredServer,
    credentials: &Arc<dyn CredentialStore>,
) -> Result<Provider, AppError> {
    let flavor = match server.info.kind {
        ServerKind::Jellyfin => Flavor::Jellyfin,
        ServerKind::Emby => Flavor::Emby,
    };
    let mut config = ProviderConfig::new(server.info.id.clone(), server.info.base_url.clone())
        .with_display_name(server.info.name.clone())
        .with_device(DeviceInfo::new(
            "Nova Desktop",
            format!("nova-desktop-{}", server.info.id.as_str()),
        ));
    // 上次登录时服务端给的用户标识：有它就不用每个进程都发一次 `GET /Users/Me`。
    if let Some(user_id) = server.info.user_id.clone().filter(|id| !id.is_empty()) {
        config = config.with_user_id(user_id);
    }
    Provider::new(flavor, config, Arc::clone(credentials)).map_err(AppError::from)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `nova-provider::Library` → 存储层记录（schema v3 的 `libraries` 表）。
fn library_to_stored(library: &Library) -> nova_storage::StoredLibrary {
    nova_storage::StoredLibrary {
        id: library.id.clone(),
        name: library.name.clone(),
        kind: library.kind,
        collection_type: library.collection_type.clone(),
        item_count: library.item_count,
    }
}

/// 存储层记录 → `nova-provider::Library`。
fn library_from_stored(library: nova_storage::StoredLibrary) -> Library {
    Library {
        id: library.id,
        name: library.name,
        kind: library.kind,
        collection_type: library.collection_type,
        item_count: library.item_count,
    }
}

#[cfg(test)]
mod tests {
    use nova_core::{
        AuthState, ImageSet, Language, MediaItem, MediaKind, ServerId, ServerInfo, UserData,
    };

    use super::*;
    use crate::env::CredentialMode;

    fn info(id: &str) -> ServerInfo {
        ServerInfo {
            id: ServerId::new(id),
            kind: ServerKind::Jellyfin,
            name: "测试".to_owned(),
            base_url: "http://127.0.0.1:8096".to_owned(),
            version: None,
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: Language::undetermined(),
        }
    }

    fn stored(id: &str) -> StoredServer {
        StoredServer {
            info: info(id),
            created_at: 1,
            last_seen_at: 1,
        }
    }

    fn movie(id: &str, title: &str) -> MediaItem {
        MediaItem {
            id: nova_core::ItemId::new(id),
            kind: MediaKind::Movie,
            title: title.to_owned(),
            sort_title: title.to_owned(),
            original_title: None,
            year: Some(2024),
            overview: None,
            genres: Vec::new(),
            runtime: None,
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet::default(),
            user_data: UserData::default(),
        }
    }

    #[test]
    fn server_selection_matches_cli_rules() {
        assert!(matches!(
            select_server(Vec::new(), None).expect("空列表"),
            (None, Status::NoServer)
        ));
        let (server, _) = select_server(vec![stored("a")], None).expect("单台");
        assert_eq!(server.as_ref().map(|s| s.info.id.as_str()), Some("a"));
        let (server, status) = select_server(vec![stored("a"), stored("b")], None).expect("多台");
        assert!(server.is_none());
        assert!(matches!(status, Status::AmbiguousServer { count: 2 }));
        assert!(select_server(vec![stored("a")], Some("zzz")).is_err());
        let (server, _) = select_server(vec![stored("a"), stored("b")], Some("b")).expect("指定");
        assert_eq!(server.as_ref().map(|s| s.info.id.as_str()), Some("b"));
    }

    #[test]
    fn empty_data_dir_starts_offline_without_panicking() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let local = Local::open(&env, None).expect("空目录应能启动");
        assert!(matches!(local.status(), Status::NoServer));
        assert!(local.server().is_none());
        // 即使没有任何库/缓存，「首页」标签也必须存在——否则用户无法回到首页。
        let tabs = local.tabs();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].title, "首页");
        // 状态不再占用 hero 位：首页是空态，状态由顶部横幅表达。
        let home = local.home(None).expect("构建首页");
        assert!(home.hero.is_none());
        assert!(home.continue_watching.is_empty() && home.recently_added.is_empty());
        assert!(local.status().banner_data().is_some());
    }

    /// 令牌过期发生在后台任务里（详情 / 搜索 / 刷新）：状态变化必须立刻通知界面，
    /// 否则横幅要等到下一次启动才变成「登录已过期」。
    #[test]
    fn mark_expired_notifies_the_status_listener_and_persists() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let server_id = "srv-status-listener";
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            let mut server = info(server_id);
            server.auth = AuthState::Authorized {
                user_name: "阿明".to_owned(),
                credential_key: format!("jellyfin/{server_id}/token"),
            };
            db.upsert_server(&server, 1).expect("写服务器");
        }
        let local = Local::open(&env, None).expect("打开状态目录");
        let seen: Arc<Mutex<Vec<Status>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        local.set_status_listener(Arc::new(move |status: &Status| {
            lock(&recorder).push(status.clone());
        }));

        local.mark_expired();

        assert!(
            matches!(local.status(), Status::Expired { .. }),
            "内存状态要变成过期：{:?}",
            local.status()
        );
        let stored = local
            .db
            .server(&ServerId::new(server_id))
            .expect("读服务器")
            .expect("服务器还在");
        assert!(
            matches!(stored.info.auth, AuthState::Expired { .. }),
            "过期要落到本地库：{:?}",
            stored.info.auth
        );
        let seen = lock(&seen).clone();
        assert_eq!(seen.len(), 1, "监听器应当只被通知一次：{seen:?}");
        assert!(
            matches!(seen[0], Status::Expired { .. }),
            "通知的应当是刚生效的状态：{:?}",
            seen[0]
        );
    }

    /// 体检：库里写着「登录已过期」、但凭证存储里其实有令牌时，不能一启动就判死。
    ///
    /// 以前 `Local::open` 先看 `AuthState::Expired`、后看 `token_present`，状态直接是
    /// `Expired`；而 `refresh()` 只处理 `is_connected` 的状态，于是一次请求都不发，
    /// 横幅永远显示「登录已过期」。有令牌时应当按 `Connected` 启动，交给刷新去验证：
    /// 验证成功写回 `Authorized`，真 401 再写回 `Expired`。
    #[test]
    fn an_expired_record_with_a_token_starts_connected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let server_id = "srv-selfheal";
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            let mut server = info(server_id);
            server.auth = AuthState::Expired {
                user_name: "阿明".to_owned(),
            };
            db.upsert_server(&server, 1).expect("写服务器");
        }

        // 没有令牌：保持原来的语义（过期就是过期）。
        let local = Local::open(&env, None).expect("打开状态目录");
        assert!(matches!(local.status(), Status::Expired { .. }));
        drop(local);

        // 有令牌：按已连接启动，交给 refresh 验证。
        let store = crate::env::open_credentials(CredentialMode::Memory).store;
        store
            .set(
                &format!("jellyfin/{server_id}/token"),
                &nova_secrets::SecretString::new("token-selfheal"),
            )
            .expect("写令牌");
        let local = Local::open(&env, None).expect("重新打开状态目录");
        assert!(
            local.status().is_connected(),
            "有令牌就该按 Connected 启动（交给刷新验证），实际：{:?}",
            local.status()
        );
    }

    #[test]
    fn cached_items_are_browsable_without_a_token() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&info("srv-a"), 1).expect("写服务器");
            db.upsert_items(&ServerId::new("srv-a"), &[movie("m1", "缓存电影")], 1)
                .expect("写条目");
        }
        let local = Local::open(&env, None).expect("打开状态目录");
        assert!(matches!(local.status(), Status::MissingToken { .. }));
        // 「首页」+「全部」
        assert_eq!(local.tabs().len(), 2, "离线时应是 首页 + 全部 两个标签");
        let home = local.home(None).expect("构建首页");
        assert_eq!(home.recently_added.len(), 1);
        assert_eq!(home.recently_added[0].title, "缓存电影");
        let library = local
            .library("", None, views::LibrarySort::default())
            .expect("构建媒体库");
        assert_eq!(library.cards.len(), 1);
        assert_eq!(library.title, "全部");
    }

    /// 把一部电影写进库 `lib-anime`（列表刷新建立的库归属），并返回打开的 `Local`。
    fn local_with_library_movie(dir: &tempfile::TempDir) -> Local {
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&info("srv-a"), 1).expect("写服务器");
            let mut item = movie("m1", "五等分的新娘＊");
            item.parent_id = Some(nova_core::ItemId::new("lib-anime"));
            db.upsert_items(&ServerId::new("srv-a"), &[item], 1)
                .expect("写条目");
        }
        Local::open(&env, None).expect("打开状态目录")
    }

    /// 体检 R4-3：详情刷新回来的 `ParentId` 是物理父项（Emby 里可能是合集 / 子
    /// 文件夹，本地根本没有这条），不能把列表刷新建立的库归属覆盖掉——否则条目
    /// 从库里消失，返回时同一下标换成另一部片。
    #[test]
    fn detail_refresh_keeps_the_library_membership() {
        let dir = tempfile::tempdir().expect("临时目录");
        let local = local_with_library_movie(&dir);
        let before = local
            .library("lib-anime", None, views::LibrarySort::TitleAsc)
            .expect("构建媒体库");
        assert_eq!(before.cards.len(), 1);

        let mut fresh = movie("m1", "五等分的新娘＊");
        fresh.parent_id = Some(nova_core::ItemId::new("boxset-12675"));
        local
            .cache_item_and_sources(&fresh, &[])
            .expect("写详情缓存");

        let after = local
            .library("lib-anime", None, views::LibrarySort::TitleAsc)
            .expect("重建媒体库");
        assert_eq!(
            after.cards.len(),
            1,
            "详情刷新不能把条目踢出媒体库（R4-3 的同一下标换片）"
        );
        assert_eq!(after.cards[0].item_id, "m1");
        assert_eq!(after.cards[0].title, "五等分的新娘＊");
        let stored = local
            .db
            .item(&ServerId::new("srv-a"), &nova_core::ItemId::new("m1"))
            .expect("读条目")
            .expect("条目还在");
        assert_eq!(
            stored.parent_id.map(|id| id.as_str().to_owned()),
            Some("lib-anime".to_owned()),
            "服务端的物理父项不能覆盖库归属"
        );
    }

    /// 同一族：搜索结果不带 `ParentId`（或带物理父项）时，也不能把库归属写成空 / 别的 id。
    #[test]
    fn search_results_keep_the_library_membership() {
        let dir = tempfile::tempdir().expect("临时目录");
        let local = local_with_library_movie(&dir);

        // Emby 的搜索响应不带 `ParentId`：映射后 `parent_id` 是 `None`。
        let hit = movie("m1", "五等分的新娘＊");
        local.cache_search_items(&[hit]).expect("写搜索缓存");

        // 直接看缓存：写成 NULL 时库查询会因为“库为空”退化为顶层列表，
        // 卡片数仍然可能是 1——那会掩盖真正的归属丢失。
        let stored = local
            .db
            .item(&ServerId::new("srv-a"), &nova_core::ItemId::new("m1"))
            .expect("读条目")
            .expect("条目还在");
        assert_eq!(
            stored.parent_id.map(|id| id.as_str().to_owned()),
            Some("lib-anime".to_owned()),
            "搜索不能清空列表刷新建立的库归属"
        );

        let after = local
            .library("lib-anime", None, views::LibrarySort::TitleAsc)
            .expect("重建媒体库");
        assert_eq!(after.cards.len(), 1, "搜索不能把条目踢出媒体库");
        assert_eq!(after.cards[0].item_id, "m1");
    }

    fn library(id: &str, name: &str, kind: MediaKind) -> Library {
        let collection_type = match kind {
            MediaKind::Movie => "movies",
            MediaKind::Series => "tvshows",
            MediaKind::Season | MediaKind::Episode => "",
        };
        Library {
            id: nova_core::ItemId::new(id),
            name: name.to_owned(),
            kind,
            collection_type: Some(collection_type.to_owned()),
            item_count: None,
        }
    }

    /// 离线启动时标签要来自**持久化**的媒体库列表，而不是靠“缓存里有条目就合成
    /// 一个「全部」”。
    ///
    /// 修复前：`Local::set_libraries` 只写内存，重启后 `libraries` 是空的，标签
    /// 退化成 `首页,全部`——库里真实的名字（“动画电影”）丢了。
    #[test]
    fn offline_start_keeps_persisted_library_tabs() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&info("srv-a"), 1).expect("写服务器");
            let mut movie = movie("m1", "五等分的新娘＊");
            movie.parent_id = Some(nova_core::ItemId::new("lib-anime"));
            db.upsert_items(&ServerId::new("srv-a"), &[movie], 1)
                .expect("写条目");
        }

        // 第一次启动：刷新把媒体库列表写进本层（内存 + SQLite）。
        // 顺序故意不是显示名序（“儿童节目”在“动画电影”前），锁住 R6-8：
        // 读回必须保持服务端顺序，而不是重新按 name 排序。
        {
            let local = Local::open(&env, None).expect("打开状态目录");
            local.set_libraries(vec![
                library("lib-anime", "动画电影", MediaKind::Movie),
                library("lib-kids", "儿童节目", MediaKind::Movie),
                library("lib-movie", "电影", MediaKind::Movie),
            ]);
            assert_eq!(
                local
                    .tabs()
                    .iter()
                    .map(|tab| tab.title.as_str())
                    .collect::<Vec<_>>(),
                vec!["首页", "动画电影", "儿童节目", "电影"]
            );
        }

        // 第二次启动（离线）：标签仍是真实库名 + 服务端顺序，库页也能按库列出条目。
        let local = Local::open(&env, None).expect("重新打开状态目录");
        let titles: Vec<String> = local.tabs().into_iter().map(|tab| tab.title).collect();
        assert_eq!(
            titles,
            vec![
                "首页".to_owned(),
                "动画电影".to_owned(),
                "儿童节目".to_owned(),
                "电影".to_owned()
            ],
            "离线启动必须按服务端顺序读回持久化的媒体库标签"
        );
        let library = local
            .library("lib-anime", None, views::LibrarySort::default())
            .expect("构建媒体库");
        assert_eq!(library.title, "动画电影");
        assert_eq!(library.cards.len(), 1);
        assert_eq!(library.cards[0].title, "五等分的新娘＊");
    }

    /// 旧缓存（媒体库列表还没持久化）离线启动时只剩合成的「全部」标签：
    /// 这一页也必须是内容，而不是空的顶层列表。
    #[test]
    fn offline_all_tab_lists_library_owned_items() {
        let dir = tempfile::tempdir().expect("临时目录");
        let local = local_with_library_movie(&dir);

        let titles: Vec<String> = local.tabs().into_iter().map(|tab| tab.title).collect();
        assert_eq!(titles, vec!["首页".to_owned(), "全部".to_owned()]);

        let all = local
            .library("", None, views::LibrarySort::default())
            .expect("构建「全部」");
        assert_eq!(all.cards.len(), 1, "「全部」不能是空的：{all:?}");
        assert_eq!(all.cards[0].item_id, "m1");
    }

    /// 层级（季 / 集）仍以服务端为准：这条规则只保电影 / 剧集的库归属。
    #[test]
    fn episode_parent_still_follows_the_server() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&info("srv-a"), 1).expect("写服务器");
            let mut episode = movie("e1", "第 1 集");
            episode.kind = MediaKind::Episode;
            episode.parent_id = Some(nova_core::ItemId::new("season-1"));
            db.upsert_items(&ServerId::new("srv-a"), &[episode], 1)
                .expect("写条目");
        }
        let local = Local::open(&env, None).expect("打开状态目录");

        let mut fresh = movie("e1", "第 1 集");
        fresh.kind = MediaKind::Episode;
        fresh.parent_id = Some(nova_core::ItemId::new("season-2"));
        local
            .cache_item_and_sources(&fresh, &[])
            .expect("写详情缓存");

        let stored = local
            .db
            .item(&ServerId::new("srv-a"), &nova_core::ItemId::new("e1"))
            .expect("读条目")
            .expect("条目还在");
        assert_eq!(
            stored.parent_id.map(|id| id.as_str().to_owned()),
            Some("season-2".to_owned()),
            "集的父项必须跟随服务端"
        );
    }
}
