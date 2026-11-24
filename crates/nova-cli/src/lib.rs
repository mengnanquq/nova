// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova` 命令行：登录、浏览媒体库、取播放地址、上报进度。
//!
//! # 分层
//!
//! ```text
//! main.rs ──> run_in(&Env, argv) ──> args::parse ──> App（命令实现）
//!                                                      ├─ nova-storage（servers / prefs）
//!                                                      ├─ nova-secrets（令牌）
//!                                                      └─ nova-provider（登录 / 播放地址 / 进度）
//! ```
//!
//! # 状态放在哪
//!
//! - 服务器信息、偏好：`$XDG_DATA_HOME/nova/nova.db`（可用 `NOVA_DATA_DIR` 覆盖）；
//! - 访问令牌：系统凭证存储（`KeyringStore::new("nova")`），SQLite 里只有键名；
//! - 密钥环不可用时**不静默失败**：打印警告后退化为进程内 [`MemoryStore`]，
//!   并在后续命令里明确说明"为什么需要重新登录"。
//!
//! # 本地缓存
//!
//! `libraries` / `items` / `info` / `play` 拿到的 `MediaItem` 与 `MediaSource` 会写进
//! `nova.db`（`upsert_items` / `upsert_sources`），`items --cached` 与 `info` 优先读本地。
//! 缓存只是加速与离线兜底：条目、资源仍以服务端为准。
//!
//! 会话恢复完全交给 provider：`ProviderConfig::with_user_id(ServerInfo.user_id)` +
//! 惰性 `GET /Users/Me`，所以"进程 A `login`、进程 B `libraries`"可以直接工作。

#![warn(missing_docs)]

mod args;
mod commands;
mod error;
mod output;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, OnceLock};

use nova_core::{AuthState, MediaItem, MediaSource, ServerId};
use nova_provider::{DeviceInfo, Flavor, MediaProvider, Provider, ProviderConfig};
use nova_secrets::{CredentialStore, KeyringStore, MemoryStore};
use nova_storage::{Database, StoredServer};

pub use args::{Command, LoginArgs};
pub use error::CliError;
pub use output::CommandOutput;

/// 凭证存储选哪种后端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialMode {
    /// 优先系统密钥环，不可用时退化为进程内存储（默认）。
    Auto,
    /// 只用系统密钥环，不可用时同样退化（但警告里会说明是显式指定）。
    Keyring,
    /// 只用进程内存储（CI、容器、无 Secret Service 的机器）。
    Memory,
}

impl CredentialMode {
    /// 从 `NOVA_CREDENTIALS` 解析；未知取值回落到 [`Self::Auto`] 并记一条告警。
    pub fn from_env_value(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") | Some("auto") => Self::Auto,
            Some("keyring") => Self::Keyring,
            Some("memory") => Self::Memory,
            Some(other) => {
                tracing::warn!(value = other, "NOVA_CREDENTIALS 取值无法识别，按 auto 处理");
                Self::Auto
            }
        }
    }
}

/// 一次调用的运行环境（从进程环境解析，测试可以自己构造）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    /// 状态目录（数据库就放在里面）。
    pub data_dir: PathBuf,
    /// 凭证后端选择。
    pub credentials: CredentialMode,
}

impl Env {
    /// 按 `NOVA_DATA_DIR` / `XDG_DATA_HOME` / `HOME` 与 `NOVA_CREDENTIALS` 解析。
    pub fn from_process() -> Result<Self, CliError> {
        let data_dir = resolve_data_dir(
            std::env::var("NOVA_DATA_DIR").ok().as_deref(),
            std::env::var("XDG_DATA_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )?;
        let credentials =
            CredentialMode::from_env_value(std::env::var("NOVA_CREDENTIALS").ok().as_deref());
        Ok(Self {
            data_dir,
            credentials,
        })
    }
}

/// XDG 规则：`NOVA_DATA_DIR` > `XDG_DATA_HOME/nova` > `$HOME/.local/share/nova`。
pub fn resolve_data_dir(
    override_dir: Option<&str>,
    xdg_data_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, CliError> {
    if let Some(dir) = override_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = xdg_data_home.map(str::trim).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("nova"));
    }
    if let Some(home) = home.map(str::trim).filter(|home| !home.is_empty()) {
        return Ok(PathBuf::from(home).join(".local/share/nova"));
    }
    Err(CliError::Config(
        "无法确定状态目录：请设置 NOVA_DATA_DIR（或 XDG_DATA_HOME / HOME）".to_owned(),
    ))
}

/// 在当前环境里跑一条命令，返回应当打印的文本。
pub async fn run(argv: Vec<String>) -> Result<String, CliError> {
    let env = Env::from_process()?;
    run_in(&env, argv).await
}

/// 在指定环境里跑一条命令（测试与无环境变量场景用）。
pub async fn run_in(env: &Env, argv: Vec<String>) -> Result<String, CliError> {
    match args::parse(&argv)? {
        Command::Help(topic) => Ok(format!("{}\n", args::help(topic.as_deref()))),
        Command::Version => Ok(format!("nova {}\n", env!("CARGO_PKG_VERSION"))),
        Command::Login(options) => {
            let app = App::open(env)?;
            app.login(options).await.map(|output| output.render(false))
        }
        Command::Servers { json } => {
            let app = App::open(env)?;
            Ok(app.servers()?.render(json))
        }
        Command::Libraries { json, server } => {
            let app = App::open(env)?;
            Ok(app.libraries(server.as_deref()).await?.render(json))
        }
        Command::Items {
            json,
            server,
            library,
            search,
            limit,
            cached,
        } => {
            let app = App::open(env)?;
            let output = app
                .items(
                    server.as_deref(),
                    &library,
                    search.as_deref(),
                    limit,
                    cached,
                )
                .await?;
            Ok(output.render(json))
        }
        Command::Info { json, server, item } => {
            let app = App::open(env)?;
            Ok(app.info(server.as_deref(), &item).await?.render(json))
        }
        Command::Play {
            server,
            item,
            verify,
            show_url,
        } => {
            let app = App::open(env)?;
            let output = app.play(server.as_deref(), &item, verify).await?;
            if show_url {
                let url = output
                    .json()
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !url.is_empty() {
                    eprintln!(
                        "{}",
                        concat!(
                            "警告：下面这条地址的查询串里带着访问令牌，等同于凭证。\n",
                            "　　　不要贴进日志、issue、截图或聊天窗口；用完请清屏。",
                        )
                    );
                    eprintln!("{url}");
                }
            }
            Ok(output.render(false))
        }
        Command::Progress {
            server,
            item,
            position,
            paused,
            method,
        } => {
            let app = App::open(env)?;
            let output = app
                .progress(server.as_deref(), &item, position, paused, method)
                .await?;
            Ok(output.render(false))
        }
    }
}

/// 已打开的本地状态 + 凭证存储。
pub struct App {
    db: Database,
    data_dir: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    persistent: bool,
    credential_note: Option<String>,
}

impl App {
    /// 打开数据库并选定凭证后端。
    pub fn open(env: &Env) -> Result<Self, CliError> {
        std::fs::create_dir_all(&env.data_dir)?;
        let db_path = env.data_dir.join("nova.db");
        let db = Database::open(&db_path)?;
        let (credentials, persistent, credential_note) = open_credentials(env.credentials);
        if let Some(note) = &credential_note {
            warn_once(note);
        }
        Ok(Self {
            db,
            data_dir: env.data_dir.clone(),
            credentials,
            persistent,
            credential_note,
        })
    }

    /// 数据库路径。
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("nova.db")
    }

    /// 状态目录。
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 凭证存储（测试用：从里面取令牌断言"令牌不在 SQLite 里"）。
    pub fn credential_store(&self) -> &Arc<dyn CredentialStore> {
        &self.credentials
    }

    /// 令牌是否会被持久化（`false` 表示进程退出即丢）。
    pub fn credentials_persistent(&self) -> bool {
        self.persistent
    }

    /// 退化为内存存储时的说明文本。
    pub fn credential_note(&self) -> Option<&str> {
        self.credential_note.as_deref()
    }

    /// 本地数据库。
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// 选出要操作的那台服务器。
    pub(crate) fn select_server(&self, requested: Option<&str>) -> Result<StoredServer, CliError> {
        let mut servers = self.db.servers()?;
        if servers.is_empty() {
            return Err(CliError::NoServer);
        }
        match requested {
            Some(id) => servers
                .into_iter()
                .find(|server| server.info.id.as_str() == id)
                .ok_or_else(|| CliError::ServerNotFound(id.to_owned())),
            None if servers.len() == 1 => servers.pop().ok_or(CliError::NoServer),
            None => Err(CliError::AmbiguousServer(
                servers
                    .iter()
                    .map(|server| {
                        format!(
                            "{}（{} {}）",
                            server.info.id.as_str(),
                            server.info.name,
                            server.info.base_url
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("、"),
            )),
        }
    }

    /// 为一台服务器构造 provider。
    pub(crate) fn provider(&self, server: &StoredServer) -> Result<Provider, CliError> {
        let flavor = match server.info.kind {
            nova_core::ServerKind::Jellyfin => Flavor::Jellyfin,
            nova_core::ServerKind::Emby => Flavor::Emby,
        };
        let mut config = ProviderConfig::new(server.info.id.clone(), server.info.base_url.clone())
            .with_display_name(server.info.name.clone())
            .with_device(DeviceInfo::new(
                "Nova CLI",
                format!("nova-cli-{}", server.info.id.as_str()),
            ));
        // 上次登录时服务端给的用户标识：有它就不用每个进程都发一次 `GET /Users/Me`。
        if let Some(user_id) = server.info.user_id.clone().filter(|id| !id.is_empty()) {
            config = config.with_user_id(user_id);
        }
        Provider::new(flavor, config, Arc::clone(&self.credentials)).map_err(crate::error::provider)
    }

    /// provider + 令牌预检。
    ///
    /// 预检是为了把"令牌不在凭证存储里"翻译成可操作的错误（重新登录），
    /// 而不是让请求打出去再收一个 401。
    pub(crate) fn checked_provider(&self, server: &StoredServer) -> Result<Provider, CliError> {
        let provider = self.provider(server)?;
        self.token(server, &provider)?;
        Ok(provider)
    }

    /// 选服务器 + 构造带令牌的 provider。
    pub(crate) fn session(
        &self,
        requested: Option<&str>,
    ) -> Result<(StoredServer, Provider), CliError> {
        let server = self.select_server(requested)?;
        let provider = self.checked_provider(&server)?;
        Ok((server, provider))
    }

    /// 把 provider 当前看到的服务器信息写回本地库。
    ///
    /// provider 会在需要时惰性解析 `user_id`、并在 `authenticate()` 里补上服务端名与版本；
    /// 这些值只有写回 SQLite，下一条命令才能带着 `user_id` 直接开工。
    /// 只在有变化时写，避免每条命令都产生一次无意义的提交。
    pub(crate) fn persist_info(&self, provider: &Provider) -> Result<(), CliError> {
        let mut info = provider.server_info();
        let stored = self.db.server(&info.id)?;
        if let Some(stored) = &stored {
            // 从令牌恢复的会话里没有用户名（`AuthState::Authorized { user_name: "" }`，
            // 令牌本身不带用户名）：不能用空串覆盖登录时存下的名字，否则 `nova servers`
            // 会从"已认证（nova）"退化成"已认证"。
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

    /// 条目写缓存（`upsert_items` 是幂等的单事务批量写）。
    pub(crate) fn cache_items(
        &self,
        server: &StoredServer,
        items: &[MediaItem],
    ) -> Result<(), CliError> {
        self.db
            .upsert_items(&server.info.id, items, nova_storage::unix_now())?;
        if !items.is_empty() {
            tracing::debug!(count = items.len(), "条目已写入本地缓存");
        }
        Ok(())
    }

    /// 资源写缓存。调用方必须先保证条目已经在库里（`sources` 有外键）。
    pub(crate) fn cache_sources(
        &self,
        server: &StoredServer,
        sources: &[MediaSource],
    ) -> Result<(), CliError> {
        self.db.upsert_sources(&server.info.id, sources)?;
        if !sources.is_empty() {
            tracing::debug!(count = sources.len(), "资源已写入本地缓存");
        }
        Ok(())
    }

    /// 取出该服务器的令牌；没有就给出"为什么要重新登录"。
    pub(crate) fn token(
        &self,
        server: &StoredServer,
        provider: &Provider,
    ) -> Result<nova_secrets::SecretString, CliError> {
        match self.credentials.get(provider.credential_key())? {
            Some(token) if !token.is_empty() => Ok(token),
            _ => Err(self.no_session(server, "访问令牌不在凭证存储里")),
        }
    }

    /// 构造"没有会话"的错误，并把凭证后端的情况写进去。
    pub(crate) fn no_session(&self, server: &StoredServer, reason: &str) -> CliError {
        let reason = match &self.credential_note {
            Some(note) => format!("{reason}（{note}）"),
            None => reason.to_owned(),
        };
        CliError::NoSession {
            url: server.info.base_url.clone(),
            reason,
        }
    }

    /// 认证状态的人类可读描述。
    pub(crate) fn auth_text(auth: &AuthState) -> String {
        match auth {
            AuthState::Authorized { user_name, .. } if user_name.is_empty() => "已认证".to_owned(),
            AuthState::Authorized { user_name, .. } => format!("已认证（{user_name}）"),
            AuthState::Expired { user_name } if user_name.is_empty() => {
                "令牌失效，需要重新登录".to_owned()
            }
            AuthState::Expired { user_name } => {
                format!("令牌失效（上次用户 {user_name}），需要重新登录")
            }
            AuthState::Unauthorized => "未登录".to_owned(),
        }
    }
}

/// 进程内存储：整个进程共享一份。
///
/// 命令行的每条命令都是新进程，共享与否对外没有区别；但测试可以在同一进程里
/// 连续调用 [`run_in`] 模拟"多条命令"，这时共享一份才符合"令牌存着"的语义。
fn shared_memory_store() -> Arc<MemoryStore> {
    static STORE: OnceLock<Arc<MemoryStore>> = OnceLock::new();
    Arc::clone(STORE.get_or_init(|| Arc::new(MemoryStore::new())))
}

/// 选凭证后端，返回（存储, 是否持久, 退化说明）。
fn open_credentials(mode: CredentialMode) -> (Arc<dyn CredentialStore>, bool, Option<String>) {
    match mode {
        CredentialMode::Memory => (
            shared_memory_store(),
            false,
            Some(
                "已按 NOVA_CREDENTIALS=memory 使用进程内存储：令牌不会持久化，\
                 每条新命令都需要重新登录"
                    .to_owned(),
            ),
        ),
        CredentialMode::Auto | CredentialMode::Keyring => {
            let store = KeyringStore::new("nova");
            if store.is_available() {
                (Arc::new(store), true, None)
            } else {
                let reason = if mode == CredentialMode::Keyring {
                    "显式指定了 NOVA_CREDENTIALS=keyring，但系统密钥环不可用"
                } else {
                    "系统密钥环不可用（没有 Secret Service / Keychain，或未解锁）"
                };
                (
                    shared_memory_store(),
                    false,
                    Some(format!(
                        "{reason} → 已退化为进程内存储：令牌只在本进程内有效、退出即丢，\
                         下一条命令需要重新 `nova login`"
                    )),
                )
            }
        }
    }
}

fn warn_once(note: &str) {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::debug!(note, "凭证存储退化");
        eprintln!("警告：{note}");
    });
}

/// 服务器 id 的本地生成规则：由规范化后的基址派生（不需要 uuid 依赖）。
///
/// 同一个地址重复 `login` 会得到同一个 id，因此服务器记录与凭证键名保持稳定；
/// 不同地址得到不同 id。这里只求"本机稳定"，不追求全局唯一。
pub(crate) fn derive_server_id(base_url: &str) -> ServerId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in base_url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ServerId::new(format!("srv-{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_follows_xdg_rules() {
        assert_eq!(
            resolve_data_dir(Some("/tmp/x"), Some("/xdg"), Some("/home/u")).unwrap(),
            PathBuf::from("/tmp/x")
        );
        assert_eq!(
            resolve_data_dir(None, Some("/xdg"), Some("/home/u")).unwrap(),
            PathBuf::from("/xdg/nova")
        );
        assert_eq!(
            resolve_data_dir(None, None, Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.local/share/nova")
        );
        // 空字符串等于没设置。
        assert_eq!(
            resolve_data_dir(Some("  "), Some(""), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.local/share/nova")
        );
        assert!(resolve_data_dir(None, None, None).is_err());
    }

    #[test]
    fn credential_mode_parsing() {
        assert_eq!(CredentialMode::from_env_value(None), CredentialMode::Auto);
        assert_eq!(
            CredentialMode::from_env_value(Some("memory")),
            CredentialMode::Memory
        );
        assert_eq!(
            CredentialMode::from_env_value(Some("keyring")),
            CredentialMode::Keyring
        );
        assert_eq!(
            CredentialMode::from_env_value(Some("auto")),
            CredentialMode::Auto
        );
        assert_eq!(
            CredentialMode::from_env_value(Some("x")),
            CredentialMode::Auto
        );
    }

    #[test]
    fn server_id_is_stable_and_distinct() {
        let a = derive_server_id("http://127.0.0.1:18096");
        assert_eq!(a, derive_server_id("http://127.0.0.1:18096"));
        assert_ne!(a, derive_server_id("http://127.0.0.1:8096"));
        assert!(a.as_str().starts_with("srv-"));
        assert_eq!(a.as_str().len(), 20);
    }

    fn sample_server(id: &str, base_url: &str) -> nova_core::ServerInfo {
        nova_core::ServerInfo {
            id: ServerId::new(id),
            kind: nova_core::ServerKind::Jellyfin,
            name: format!("{id} 的服务器"),
            base_url: base_url.to_owned(),
            version: None,
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: nova_core::Language::undetermined(),
        }
    }

    #[test]
    fn server_selection_handles_zero_one_and_many() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let app = App::open(&env).expect("打开状态目录");

        assert!(matches!(app.select_server(None), Err(CliError::NoServer)));

        app.database()
            .upsert_server(&sample_server("srv-a", "http://a:8096"), 1)
            .expect("写服务器");
        let only = app.select_server(None).expect("只有一台服务器时不该有歧义");
        assert_eq!(only.info.id.as_str(), "srv-a");

        app.database()
            .upsert_server(&sample_server("srv-b", "http://b:8096"), 1)
            .expect("写服务器");
        let error = app
            .select_server(None)
            .expect_err("两台服务器时必须要求 --server");
        assert!(matches!(error, CliError::AmbiguousServer(_)), "{error:?}");
        assert!(error.to_string().contains("srv-a") && error.to_string().contains("srv-b"));
        assert_eq!(
            app.select_server(Some("srv-b"))
                .expect("按 id 选应当成功")
                .info
                .base_url,
            "http://b:8096"
        );
        assert!(matches!(
            app.select_server(Some("srv-x")),
            Err(CliError::ServerNotFound(_))
        ));
    }

    #[tokio::test]
    async fn persist_info_keeps_the_user_name_from_login() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let app = App::open(&env).expect("打开状态目录");

        // 模拟"上一次 login 写下的记录"。
        let credential_key = "jellyfin/srv-a/token";
        let mut info = sample_server("srv-a", "http://127.0.0.1:1");
        info.name = "家庭服务器".to_owned();
        info.version = Some("12.4.0".to_owned());
        info.user_id = Some("uid-1".to_owned());
        info.auth = AuthState::Authorized {
            user_name: "nova".to_owned(),
            credential_key: credential_key.to_owned(),
        };
        app.database().upsert_server(&info, 1).expect("写服务器");
        app.credential_store()
            .set(credential_key, &nova_secrets::SecretString::new("tok"))
            .expect("写令牌");

        // 新进程用令牌恢复会话：`user_name` 是空的。
        let server = app.select_server(None).expect("选服务器");
        let provider = app.provider(&server).expect("构造 provider");
        assert_eq!(
            provider.server_info().auth.user_name(),
            Some(""),
            "从令牌恢复的会话不带用户名"
        );

        app.persist_info(&provider).expect("写回服务器信息");
        let servers = app.database().servers().expect("读服务器");
        assert_eq!(
            servers[0].info.auth.user_name(),
            Some("nova"),
            "空用户名不能覆盖登录时存下的名字"
        );
        assert_eq!(servers[0].info.user_id.as_deref(), Some("uid-1"));
        assert_eq!(servers[0].info.name, "家庭服务器");
    }

    #[test]
    fn missing_session_error_explains_the_credential_backend() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let app = App::open(&env).expect("打开状态目录");
        let server = nova_storage::StoredServer {
            info: sample_server("srv-a", "http://a:8096"),
            created_at: 1,
            last_seen_at: 1,
        };
        let error = app.no_session(&server, "访问令牌不在凭证存储里");
        let text = error.to_string();
        assert!(text.contains("访问令牌不在凭证存储里"), "{text}");
        assert!(text.contains("NOVA_CREDENTIALS=memory"), "{text}");
        assert!(text.contains("nova login"), "{text}");
    }

    #[test]
    fn auth_state_text_never_claims_more_than_it_knows() {
        assert_eq!(App::auth_text(&AuthState::Unauthorized), "未登录");
        assert_eq!(
            App::auth_text(&AuthState::Authorized {
                user_name: "nova".to_owned(),
                credential_key: "jellyfin/x/token".to_owned(),
            }),
            "已认证（nova）"
        );
        assert_eq!(
            App::auth_text(&AuthState::Authorized {
                user_name: String::new(),
                credential_key: "jellyfin/x/token".to_owned(),
            }),
            "已认证"
        );
        assert!(
            App::auth_text(&AuthState::Expired {
                user_name: "nova".to_owned()
            })
            .contains("重新登录")
        );
    }
}
