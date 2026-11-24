// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 运行环境：数据目录与凭证后端。
//!
//! 数据目录规则与 `nova`（CLI）**逐字一致**，这样 `nova login` 写下的
//! `nova.db`、服务器记录、凭证键名都能被桌面端直接复用：
//!
//! ```text
//! $NOVA_DATA_DIR > $XDG_DATA_HOME/nova > $HOME/.local/share/nova
//! ```
//!
//! 令牌仍然只经由 `nova-secrets` 进出：SQLite 里只有凭证键名，真正的令牌在系统
//! 密钥环里。密钥环不可用时与 CLI 一样退化为进程内 [`MemoryStore`]，并明确告知
//! "为什么需要重新登录"，而不是静默失败。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use nova_secrets::{CredentialStore, KeyringStore, MemoryStore};

use crate::error::AppError;

/// 凭证存储后端选择（`NOVA_CREDENTIALS`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CredentialMode {
    /// 优先系统密钥环，不可用时退化为进程内存储。
    #[default]
    Auto,
    /// 显式要求系统密钥环（不可用时同样退化，但说明里会注明）。
    Keyring,
    /// 只用进程内存储（CI、容器、无 Secret Service 的机器）。
    Memory,
}

impl CredentialMode {
    /// 从 `NOVA_CREDENTIALS` 的取值解析；未知取值回落到 [`Self::Auto`]。
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

/// 一次启动的运行环境。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    /// 状态目录（`nova.db` 与 `images/` 都在里面）。
    pub data_dir: PathBuf,
    /// 凭证后端选择。
    pub credentials: CredentialMode,
}

impl Env {
    /// 从进程环境解析 `NOVA_DATA_DIR` / `XDG_DATA_HOME` / `HOME` 与 `NOVA_CREDENTIALS`。
    pub fn from_process() -> Result<Self, AppError> {
        let data_dir = resolve_data_dir(
            std::env::var("NOVA_DATA_DIR").ok().as_deref(),
            std::env::var("XDG_DATA_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )?;
        Ok(Self {
            data_dir,
            credentials: CredentialMode::from_env_value(
                std::env::var("NOVA_CREDENTIALS").ok().as_deref(),
            ),
        })
    }
}

/// XDG 规则：`NOVA_DATA_DIR` > `XDG_DATA_HOME/nova` > `$HOME/.local/share/nova`。
pub fn resolve_data_dir(
    override_dir: Option<&str>,
    xdg_data_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, AppError> {
    if let Some(dir) = override_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = xdg_data_home.map(str::trim).filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("nova"));
    }
    if let Some(home) = home.map(str::trim).filter(|home| !home.is_empty()) {
        return Ok(PathBuf::from(home).join(".local/share/nova"));
    }
    Err(AppError::Config(
        "无法确定状态目录：请设置 NOVA_DATA_DIR（或 XDG_DATA_HOME / HOME）".to_owned(),
    ))
}

/// 打开后的凭证后端。
pub struct Credentials {
    /// 实际使用的存储。
    pub store: Arc<dyn CredentialStore>,
    /// 令牌是否会持久化（`false` 表示进程退出即丢）。
    pub persistent: bool,
    /// 退化为进程内存储时的说明文本。
    pub note: Option<String>,
}

/// 按环境选择凭证后端。
///
/// 与 `nova-cli` 共用同一套判断：`KeyringStore::new("nova")` + 一次真实读写探测；
/// 探测失败就退化为进程内存储，并把原因放进 [`Credentials::note`]。
pub fn open_credentials(mode: CredentialMode) -> Credentials {
    match mode {
        CredentialMode::Memory => Credentials {
            store: shared_memory_store(),
            persistent: false,
            note: Some(
                "已按 NOVA_CREDENTIALS=memory 使用进程内存储：令牌不会持久化，\
                 每次启动都需要重新 `nova login`"
                    .to_owned(),
            ),
        },
        CredentialMode::Auto | CredentialMode::Keyring => {
            let store = KeyringStore::new("nova");
            if store.is_available() {
                Credentials {
                    store: Arc::new(store),
                    persistent: true,
                    note: None,
                }
            } else {
                let reason = if mode == CredentialMode::Keyring {
                    "显式指定了 NOVA_CREDENTIALS=keyring，但系统密钥环不可用"
                } else {
                    "系统密钥环不可用（没有 Secret Service / Keychain，或未解锁）"
                };
                Credentials {
                    store: shared_memory_store(),
                    persistent: false,
                    note: Some(format!(
                        "{reason} → 已退化为进程内存储：令牌只在本次运行内有效，\
                         需要先在终端重新 `nova login`"
                    )),
                }
            }
        }
    }
}

/// 进程内存储：整个进程共享一份。
///
/// 桌面端与 CLI 不同，一个进程里只有一次会话，共享与否对外没有区别；
/// 共享一份是为了让"打开窗口 → 后台刷新 → 打开详情"看到同一个令牌状态。
fn shared_memory_store() -> Arc<MemoryStore> {
    static STORE: OnceLock<Arc<MemoryStore>> = OnceLock::new();
    Arc::clone(STORE.get_or_init(|| Arc::new(MemoryStore::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_follows_xdg_rules_like_the_cli() {
        assert_eq!(
            resolve_data_dir(Some("/tmp/x"), Some("/xdg"), Some("/home/u"))
                .expect("NOVA_DATA_DIR 优先"),
            PathBuf::from("/tmp/x")
        );
        assert_eq!(
            resolve_data_dir(None, Some("/xdg"), Some("/home/u")).expect("XDG 次之"),
            PathBuf::from("/xdg/nova")
        );
        assert_eq!(
            resolve_data_dir(None, None, Some("/home/u")).expect("HOME 兜底"),
            PathBuf::from("/home/u/.local/share/nova")
        );
        assert_eq!(
            resolve_data_dir(Some("  "), Some(""), Some("/home/u")).expect("空串等于没设置"),
            PathBuf::from("/home/u/.local/share/nova")
        );
        assert!(resolve_data_dir(None, None, None).is_err());
    }

    #[test]
    fn credential_mode_parses_known_values() {
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
            CredentialMode::from_env_value(Some("nonsense")),
            CredentialMode::Auto
        );
    }

    #[test]
    fn memory_mode_is_explicitly_non_persistent() {
        let credentials = open_credentials(CredentialMode::Memory);
        assert!(!credentials.persistent);
        assert!(
            credentials
                .note
                .as_deref()
                .is_some_and(|note| note.contains("NOVA_CREDENTIALS=memory"))
        );
    }
}
