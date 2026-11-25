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

use nova_core::{AuthState, MediaItem, MediaSource, ServerId, ServerKind, UserPreferences};
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
    libraries: Mutex<Vec<Library>>,
}

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
                    status = if matches!(stored.info.auth, AuthState::Expired { .. }) {
                        Status::Expired {
                            url: stored.info.base_url.clone(),
                        }
                    } else if token_present {
                        Status::Connected {
                            server: stored.info.name.clone(),
                            base_url: stored.info.base_url.clone(),
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
            libraries: Mutex::new(Vec::new()),
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
    pub fn set_status(&self, status: Status) {
        *lock(&self.status) = status;
    }

    /// 当前已知的服务端媒体库（刷新后才有）。
    pub fn libraries(&self) -> Vec<Library> {
        lock(&self.libraries).clone()
    }

    /// 记录刷新拿到的媒体库列表。
    pub fn set_libraries(&self, libraries: Vec<Library>) {
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
    pub fn library(&self, library_id: &str, search: Option<&str>) -> Result<LibraryView, AppError> {
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
        )?)
    }

    /// 用缓存构建搜索结果页（离线可用；输入时的即时反馈也走这里）。
    pub fn search_view(&self, term: &str, limit: usize) -> Result<SearchView, AppError> {
        match self.server() {
            Some(server) => Ok(views::search_view(&self.db, &server.info.id, term, limit)?),
            None => Ok(SearchView::from_cards(term, Vec::new())),
        }
    }

    /// 用缓存构建详情页。
    pub fn detail(&self, item_id: &str) -> Result<Option<DetailView>, AppError> {
        let Some(server) = self.server() else {
            return Ok(None);
        };
        Ok(views::detail_view(&self.db, &server.info.id, item_id)?)
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

    /// 写单条条目与它的可播放版本（详情页按需刷新用）。
    pub fn cache_item_and_sources(
        &self,
        item: &MediaItem,
        sources: &[MediaSource],
    ) -> Result<(), AppError> {
        let Some(server) = self.server() else {
            return Ok(());
        };
        self.db.upsert_items(
            &server.info.id,
            std::slice::from_ref(item),
            nova_storage::unix_now(),
        )?;
        self.db.upsert_sources(&server.info.id, sources)?;
        Ok(())
    }

    /// 把 provider 当前看到的服务器信息写回本地库（与 CLI 的 `persist_info` 同规则）。
    ///
    /// 关键点：从令牌恢复的会话里 `user_name` 是空串，不能用它覆盖登录时存下的名字。
    pub fn persist_info(&self, provider: &Provider) -> Result<(), AppError> {
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
        let library = local.library("", None).expect("构建媒体库");
        assert_eq!(library.cards.len(), 1);
        assert_eq!(library.title, "全部");
    }
}
