// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 凭证存储门面。
//!
//! 整个项目里访问令牌只允许经由本 crate 进出，规则如下：
//!
//! 1. 令牌被包在 [`SecretString`] 里，它**不实现** `Serialize` / `Display`，
//!    `Debug` 只打印长度。因此 `#[derive(Debug)]` 的结构体、`tracing::debug!`、
//!    错误类型都可以安全地带上它。
//! 2. 取明文必须显式调用 [`SecretString::expose`]，这一处在代码审查里可以被 grep 出来。
//! 3. 真正的落盘由操作系统凭证存储负责（macOS Keychain / Windows Credential Manager /
//!    Linux Secret Service），Nova 自己的 SQLite 里不存令牌，只存键名。

#![warn(missing_docs)]

use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use zeroize::Zeroizing;

/// 凭证存储错误。**不包含**任何明文内容。
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// 操作系统凭证存储拒绝访问（通常是没有解锁的密钥环）。
    #[error("凭证存储不可用：{0}")]
    Unavailable(String),
    /// 访问凭证存储失败。
    #[error("访问凭证存储失败：{0}")]
    Backend(String),
}

/// 一段不应出现在日志、数据库或错误信息里的字符串。
///
/// 关键字选 `Secret*` 而不是 `Sensitive*`：本仓库里出现这个词就代表"这里必须小心"。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    /// 包装一段明文。调用点应当已经知道它是敏感的。
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// 取出明文。
    ///
    /// 这是整个项目里唯一能拿到令牌明文的入口；每处调用都应该一眼能说出
    /// "为什么这里必须要有明文"（拼 Authorization 头、写凭证存储）。
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 明文长度（诊断用，不泄漏内容）。
    pub fn len(&self) -> usize {
        self.0.chars().count()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 连长度都只在 debug_alternate 下给：正常日志里它就是 <redacted>。
        if f.alternate() {
            write!(f, "SecretString(<redacted, {} chars>)", self.len())
        } else {
            f.write_str("SecretString(<redacted>)")
        }
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// 凭证存储抽象。
///
/// 同步接口：读写密钥环的耗时是毫秒级且只在登录/启动时发生，不值得为它引入异步复杂度。
pub trait CredentialStore: Send + Sync {
    /// 读取；不存在返回 `Ok(None)`（不是错误）。
    fn get(&self, key: &str) -> Result<Option<SecretString>, SecretError>;
    /// 写入或覆盖。
    fn set(&self, key: &str, value: &SecretString) -> Result<(), SecretError>;
    /// 删除；不存在视为成功（幂等，便于"登出"重试）。
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

/// 进程内存储，仅供测试与无密钥环的降级场景。
///
/// **不要**用它承载真实用户凭证：进程退出即丢，且明文在内存里常驻。
#[derive(Default)]
pub struct MemoryStore {
    entries: RwLock<HashMap<String, SecretString>>,
}

impl MemoryStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前条目数（测试用）。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, SecretString>> {
        self.entries.read().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_mut(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, SecretString>> {
        self.entries.write().unwrap_or_else(|e| e.into_inner())
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 故意不打印键名集合：键名里可能带服务器标识。
        f.debug_struct("MemoryStore")
            .field("entries", &format_args!("{} 条", self.len()))
            .finish()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self.lock().get(key).cloned())
    }

    fn set(&self, key: &str, value: &SecretString) -> Result<(), SecretError> {
        self.lock_mut().insert(key.to_owned(), value.clone());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.lock_mut().remove(key);
        Ok(())
    }
}

#[cfg(feature = "keyring")]
mod keyring_store {
    use super::{CredentialStore, SecretError, SecretString};

    /// 操作系统凭证存储。
    ///
    /// - macOS：Keychain
    /// - Windows：Credential Manager
    /// - Linux：Secret Service（GNOME Keyring / KWallet 的 ksecretd）
    #[derive(Debug, Clone)]
    pub struct KeyringStore {
        service: String,
    }

    impl KeyringStore {
        /// `service` 建议形如 `"nova"`，键名里再带服务器标识，例如 `jellyfin/<server-id>/token`。
        pub fn new(service: impl Into<String>) -> Self {
            Self {
                service: service.into(),
            }
        }

        fn entry(&self, key: &str) -> Result<keyring::Entry, SecretError> {
            keyring::Entry::new(&self.service, key)
                .map_err(|e| SecretError::Backend(format!("{e}")))
        }

        /// 探测后端是否真的可用。
        ///
        /// Linux 上没有解锁的密钥环时，`Entry::new` 会成功但读写失败，所以必须实测一次。
        pub fn is_available(&self) -> bool {
            let probe_key = "nova/availability-probe";
            match self.entry(probe_key) {
                Ok(entry) => {
                    let ok = entry.get_password().is_ok() || entry.set_password("probe").is_ok();
                    let _ = entry.delete_credential();
                    ok
                }
                Err(_) => false,
            }
        }
    }

    impl CredentialStore for KeyringStore {
        fn get(&self, key: &str) -> Result<Option<SecretString>, SecretError> {
            match self.entry(key)?.get_password() {
                Ok(text) => Ok(Some(SecretString::new(text))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(SecretError::Backend(format!("{e}"))),
            }
        }

        fn set(&self, key: &str, value: &SecretString) -> Result<(), SecretError> {
            self.entry(key)?
                .set_password(value.expose())
                .map_err(|e| SecretError::Backend(format!("{e}")))
        }

        fn delete(&self, key: &str) -> Result<(), SecretError> {
            match self.entry(key)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(SecretError::Backend(format!("{e}"))),
            }
        }
    }
}

#[cfg(feature = "keyring")]
pub use keyring_store::KeyringStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_the_secret() {
        let secret = SecretString::new("tok_live_SECRET_VALUE");
        let plain = format!("{secret:?}");
        assert!(!plain.contains("tok_live"), "明文泄漏到 Debug: {plain}");
        assert_eq!(plain, "SecretString(<redacted>)");

        let alternate = format!("{secret:#?}");
        assert!(!alternate.contains("tok_live"));
        assert!(alternate.contains(&format!("{} chars", secret.len())));
        assert!(alternate.contains("SecretString(<redacted,"));
    }

    #[test]
    fn debug_of_containing_struct_is_safe() {
        // 这条测试的价值在于：任何 #[derive(Debug)] 的结构体都可以放心打印。
        #[derive(Debug)]
        struct Fits {
            token: SecretString,
            note: &'static str,
        }
        let fits = Fits {
            token: SecretString::new("super-secret"),
            note: "ok",
        };
        let out = format!("{fits:?}");
        assert!(!out.contains("super-secret"));
        assert!(out.contains("ok"));
        // 顺手证明字段本身仍可正常访问（去掉 compiler 的 dead_code 噪声）。
        assert_eq!(fits.note, "ok");
        assert_eq!(fits.token.expose(), "super-secret");
    }

    #[test]
    fn expose_is_the_only_way_out() {
        let secret = SecretString::new("abc123");
        assert_eq!(secret.expose(), "abc123");
        assert_eq!(secret.len(), 6);
        assert!(!secret.is_empty());
        assert!(SecretString::new("").is_empty());
    }

    #[test]
    fn len_counts_chars_not_bytes() {
        // 用字符数：多字节口令不至于因为字节数在某些后端被截断。
        assert_eq!(SecretString::new("密码").len(), 2);
    }

    #[test]
    fn memory_store_roundtrip_is_idempotent() {
        let store = MemoryStore::new();
        assert!(store.get("a").unwrap().is_none());
        assert!(store.is_empty());

        store.set("a", &SecretString::new("one")).unwrap();
        store.set("a", &SecretString::new("two")).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("a").unwrap().unwrap().expose(), "two");

        store.delete("a").unwrap();
        assert!(store.get("a").unwrap().is_none());
        // 删除不存在的键是成功的（登出重试）。
        store.delete("a").unwrap();
    }

    #[test]
    fn store_error_messages_do_not_contain_the_value() {
        let err = SecretError::Backend("disk full".into());
        let rendered = format!("{err} {err:?}");
        assert!(rendered.contains("disk full"));
        // 错误类型本身没有承载明文的字段，这里断言的是"结构上不可能"。
        assert!(!rendered.contains("password"));
    }

    #[test]
    fn memory_store_debug_hides_keys() {
        let store = MemoryStore::new();
        store
            .set("jellyfin/abc/token", &SecretString::new("x"))
            .unwrap();
        let out = format!("{store:?}");
        assert!(!out.contains("jellyfin/abc"), "键名泄漏: {out}");
    }

    #[test]
    fn store_is_object_safe() {
        // 上层用 Arc<dyn CredentialStore> 注入，这条测试防止以后加泛型方法破坏 dyn 兼容性。
        fn takes(_: Box<dyn CredentialStore>) {}
        takes(Box::new(MemoryStore::new()));
    }

    #[cfg(feature = "keyring")]
    #[test]
    fn keyring_store_is_constructible_without_a_session() {
        // 只验证构造与对象安全；真实读写需要解锁的密钥环，见 available_keyring_roundtrip。
        let store = KeyringStore::new("nova-test");
        let _: &dyn CredentialStore = &store;
        assert_eq!(
            format!("{store:?}"),
            "KeyringStore { service: \"nova-test\" }"
        );
    }

    #[cfg(feature = "keyring")]
    #[test]
    #[ignore = "需要解锁的 Secret Service / Keychain；CI 与容器里不跑"]
    fn available_keyring_roundtrip() {
        let store = KeyringStore::new("nova-test");
        if !store.is_available() {
            eprintln!("跳过：本机没有可用的凭证存储后端");
            return;
        }
        let key = "nova-test/roundtrip";
        store.set(key, &SecretString::new("token-value")).unwrap();
        assert_eq!(store.get(key).unwrap().unwrap().expose(), "token-value");
        store.delete(key).unwrap();
        assert!(store.get(key).unwrap().is_none());
    }
}
