// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 应用内“添加服务器”：探测服务器类型 → 认证 → 落盘。
//!
//! 流程与 `nova login`（`nova-cli`）保持同一套规则：
//!
//! 1. 地址本地校验（`nova_ui::validate_address`，与界面输入框同一份实现）；
//! 2. 同一地址复用已有服务器 id（凭证键名与会话保持稳定），否则按 FNV-1a 派生
//!    （**必须**与 `nova-cli::derive_server_id` 同算法，保证 CLI 登录过 = 桌面端已登录）；
//! 3. 先按 Jellyfin 建 provider，`public_info()` 用 `ProductName` 判类型，
//!    确认是 Emby 才用另一个 flavor 重建；
//! 4. `authenticate(user, &SecretString)`：令牌由 provider 写进凭证存储，
//!    密码只在这一次调用里存在，**不**进日志、错误串或 SQLite。
//!
//! 失败映射到 `nova_ui::AddServerErrorKind` 的四类文案；已知缺口：
//! `ProviderError::Network` 不区分 TLS 证书错误与普通连接失败
//! （provider 层把两者都折成“无法连接服务器”），所以 TLS 归入 Connect 类，
//! 专门的“证书校验失败”文案等 provider 提供区分手段后再接线。

use std::sync::Arc;

use nova_core::ServerId;
use nova_provider::{DeviceInfo, Flavor, MediaProvider, Provider, ProviderConfig, ProviderError};
use nova_secrets::SecretString;

use nova_ui::{AddServerError, AddServerErrorKind, validate_address};

use crate::error::AppError;
use crate::local::Local;

/// 一次成功连接的结果。
#[derive(Clone, Debug)]
pub struct ConnectedServer {
    /// 认证后的服务器信息（含用户名与用户标识；不含令牌）。
    pub info: nova_core::ServerInfo,
}

/// 连接一台服务器并把它写进本地状态。
///
/// 成功时 `local` 已持有可用 provider、SQLite 已有服务器记录、状态已置为
/// [`Status::Connected`](crate::status::Status)；失败时不留任何部分状态。
pub async fn connect(
    local: Arc<Local>,
    address: &str,
    user: &str,
    password: SecretString,
) -> Result<ConnectedServer, AddServerError> {
    let normalized = validate_address(address)
        .map_err(|error| AddServerError::with_detail(AddServerErrorKind::Connect, error.hint()))?;

    // 同一地址复用已有服务器 id；没有就按与 CLI 相同的规则派生。
    let existing = local
        .db
        .servers()
        .map_err(|error| storage_error(error.to_string()))?
        .into_iter()
        .find(|server| server.info.base_url.eq_ignore_ascii_case(&normalized));
    let server_id = match &existing {
        Some(server) => server.info.id.clone(),
        None => derive_server_id(&normalized),
    };

    let mut provider = build_provider(&local, server_id.clone(), &normalized, Flavor::Jellyfin)
        .map_err(|error| classify_connect(&error))?;

    // 公开信息不需要令牌：先用它判类型，也对“这个地址是不是媒体服务器”给出答案。
    match provider.public_info().await {
        Ok(public) => {
            if public.kind == nova_core::ServerKind::Emby {
                provider = build_provider(&local, server_id, &normalized, Flavor::Emby)
                    .map_err(|error| classify_connect(&error))?;
            }
        }
        Err(error) => return Err(classify_probe(&error)),
    }

    // 密码在这里被消费掉：provider 把令牌写进凭证存储，明文随 `SecretString` 丢弃。
    let info = provider
        .authenticate(user, &password)
        .await
        .map_err(|error| classify_auth(&error))?;

    // 落盘：SQLite 服务器记录 + 本地会话。阻塞操作放阻塞池。
    let adopt_local = Arc::clone(&local);
    let adopt_info = info.clone();
    tokio::task::spawn_blocking(move || adopt_local.adopt_server(provider, &adopt_info))
        .await
        .map_err(|error| storage_error(format!("落盘任务失败：{error}")))?
        .map_err(|error: AppError| classify_storage(&error))?;

    tracing::info!(
        server = info.id.as_str(),
        user = info.auth.user_name().unwrap_or(user),
        base_url = info.base_url,
        "应用内登录成功"
    );
    Ok(ConnectedServer { info })
}

/// 按当前凭证存储构造 provider（设备名与 CLI 同一套“设备”规则）。
fn build_provider(
    local: &Local,
    server_id: ServerId,
    base_url: &str,
    flavor: Flavor,
) -> Result<Provider, ProviderError> {
    let config = ProviderConfig::new(server_id.clone(), base_url).with_device(DeviceInfo::new(
        "Nova Desktop",
        format!("nova-desktop-{}", server_id.as_str()),
    ));
    Provider::new(flavor, config, Arc::clone(&local.credentials))
}

/// 服务器 id 派生：FNV-1a over 规范化地址。
///
/// **必须**与 `nova-cli` 的 `derive_server_id` 逐字一致：这样“命令行登录过”
/// 与“界面里添加”指向同一条服务器记录、同一个凭证键。
fn derive_server_id(base_url: &str) -> ServerId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in base_url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ServerId::new(format!("srv-{hash:016x}"))
}

/// `public_info()` 失败 → 四类错误。
fn classify_probe(error: &ProviderError) -> AddServerError {
    match error {
        // 连不上：网络 / 超时 / 本机凭证存储打不开（读不到历史令牌也不该挡探测）。
        ProviderError::Network(_) | ProviderError::Credentials { .. } => {
            AddServerError::with_detail(AddServerErrorKind::Connect, error.to_string())
        }
        // 地址不合规（本地校验已拦，这里是兜底）。
        ProviderError::Unsupported(_) => {
            AddServerError::with_detail(AddServerErrorKind::Connect, error.to_string())
        }
        // 有响应但不像 Jellyfin/Emby：解析失败、结构不合规、服务端报错。
        _ => AddServerError::with_detail(AddServerErrorKind::UnknownServer, error.to_string()),
    }
}

/// `authenticate()` 失败 → 四类错误。
fn classify_auth(error: &ProviderError) -> AddServerError {
    match error {
        ProviderError::Unauthorized => AddServerError::new(AddServerErrorKind::Auth),
        ProviderError::Forbidden => AddServerError::new(AddServerErrorKind::Auth),
        ProviderError::Http { status: 401 | 403 } => AddServerError::new(AddServerErrorKind::Auth),
        // 本机没有令牌：这不是“服务器类型不对”（从前落到 `_` 分支会被说成
        // “这个地址看起来不像 Emby 或 Jellyfin”），可行动作是重新输入密码 /
        // 重建本地会话，与 `Unauthorized` 同一个归属。
        ProviderError::NoToken => {
            AddServerError::with_detail(AddServerErrorKind::Auth, error.to_string())
        }
        ProviderError::Network(_) | ProviderError::Unsupported(_) => {
            AddServerError::with_detail(AddServerErrorKind::Connect, error.to_string())
        }
        ProviderError::Credentials { .. } => {
            AddServerError::with_detail(AddServerErrorKind::Connect, error.to_string())
        }
        _ => AddServerError::with_detail(AddServerErrorKind::UnknownServer, error.to_string()),
    }
}

/// 其余 provider 错误（provider 构造阶段）→ Connect。
fn classify_connect(error: &ProviderError) -> AddServerError {
    AddServerError::with_detail(AddServerErrorKind::Connect, error.to_string())
}

/// SQLite / 落盘失败 → Connect（内容可读，重试才有意义）。
fn classify_storage(error: &AppError) -> AddServerError {
    AddServerError::with_detail(AddServerErrorKind::Connect, error.to_string())
}

/// 本地库故障的统一包装。
fn storage_error(message: String) -> AddServerError {
    AddServerError::with_detail(AddServerErrorKind::Connect, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_id_matches_cli_derivation() {
        // 与 nova-cli 的 FNV-1a 同算法：同一地址两次派生一致，不同地址不同。
        let a = derive_server_id("http://127.0.0.1:18096");
        let b = derive_server_id("http://127.0.0.1:18096");
        let c = derive_server_id("http://127.0.0.1:8096");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.as_str().starts_with("srv-"));
        assert_eq!(a.as_str().len(), 20);
    }

    #[test]
    fn auth_failures_map_to_wrong_credentials() {
        assert_eq!(
            classify_auth(&ProviderError::Unauthorized).kind,
            AddServerErrorKind::Auth
        );
        assert_eq!(
            classify_auth(&ProviderError::Http { status: 401 }).kind,
            AddServerErrorKind::Auth
        );
        assert_eq!(
            classify_auth(&ProviderError::Http { status: 403 }).kind,
            AddServerErrorKind::Auth
        );
        // 403 的新形态（provider 不再把它折成 `Unauthorized`）也要落在「凭据」类，
        // 而不是被说成“这个地址不像 Emby / Jellyfin”。
        assert_eq!(
            classify_auth(&ProviderError::Forbidden).kind,
            AddServerErrorKind::Auth
        );
    }

    /// 本机没有令牌（不是服务端拒绝）：属于凭据问题，不能报成“服务器类型未知”。
    #[test]
    fn a_missing_local_token_is_not_an_unknown_server() {
        let error = classify_auth(&ProviderError::NoToken);
        assert_eq!(
            error.kind,
            AddServerErrorKind::Auth,
            "本机没有令牌时唯一可做的是重新登录"
        );
        assert_eq!(error.detail.as_deref(), Some("本机没有访问令牌"));
    }

    #[test]
    fn network_failures_map_to_connect() {
        let network = ProviderError::Network("请求超时".to_owned());
        assert_eq!(classify_auth(&network).kind, AddServerErrorKind::Connect);
        assert_eq!(classify_probe(&network).kind, AddServerErrorKind::Connect);
    }

    #[test]
    fn response_anomalies_map_to_unknown_server() {
        let decode = ProviderError::Decode("not json".to_owned());
        assert_eq!(
            classify_probe(&decode).kind,
            AddServerErrorKind::UnknownServer
        );
        let http = ProviderError::Http { status: 500 };
        assert_eq!(
            classify_probe(&http).kind,
            AddServerErrorKind::UnknownServer
        );
        let server = ProviderError::Server {
            message: " Bad Request ".to_owned(),
        };
        assert_eq!(
            classify_auth(&server).kind,
            AddServerErrorKind::UnknownServer
        );
    }

    /// 空地址在**本地校验**就被拦下：不会构造 provider、不会发任何请求、不会落盘。
    ///
    /// 体检 R2-6 声称“空地址时点「连接」发起了真实请求并得到「用户名或密码不对」”。
    /// 界面层（`add_server.slint` 的禁用态）与 `AddServerView::can_submit()` 已经拦了
    /// 两道，这里是 `connect()` 自己的兜底：`validate_address` 是第一件事。
    #[tokio::test]
    async fn empty_address_is_rejected_before_any_request() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(Local::open(&env, None).expect("打开状态目录"));

        let error = connect(
            Arc::clone(&local),
            "   ",
            "user",
            SecretString::new("secret".to_owned()),
        )
        .await
        .expect_err("空地址必须失败");

        // 文案：“请输入服务器地址”在补充说明里，主文案是四类连接失败之一。
        assert_eq!(error.kind, AddServerErrorKind::Connect);
        assert_eq!(
            error.detail.as_deref(),
            Some("请输入服务器地址"),
            "空地址要给出“需要服务器地址”的说明，而不是网络错误"
        );
        // 没有留下任何部分状态：没有服务器记录，密码也不在任何地方出现。
        assert!(
            local.db.servers().expect("读服务器").is_empty(),
            "失败的连接不能写服务器记录"
        );
        assert!(!error.detail.unwrap_or_default().contains("secret"));
    }

    #[test]
    fn errors_never_carry_password() {
        // 错误串的来源是 ProviderError（内部已脱敏）与静态文案；这里验证
        // classify 系列不会把调用方传入的任何明文带进 detail。
        let network = ProviderError::Network("无法连接服务器".to_owned());
        let error = classify_probe(&network);
        assert!(!error.detail.unwrap_or_default().contains("secret-password"));
        assert_eq!(classify_auth(&network).kind, AddServerErrorKind::Connect);
    }

    // ---- 真机集成（可选）：NOVA_JELLYFIN_URL / NOVA_JELLYFIN_USER / NOVA_JELLYFIN_PASSWORD ----
    // 需要一台真实的 Jellyfin；跑通后断言 SQLite 里出现服务器，且数据库文件里 grep 不到密码。
    #[tokio::test]
    async fn real_jellyfin_login_persists_without_password() {
        let url = std::env::var("NOVA_JELLYFIN_URL")
            .ok()
            .filter(|v| !v.is_empty());
        let user = std::env::var("NOVA_JELLYFIN_USER")
            .ok()
            .filter(|v| !v.is_empty());
        let password = std::env::var("NOVA_JELLYFIN_PASSWORD")
            .ok()
            .filter(|v| !v.is_empty());
        let (Some(url), Some(user), Some(password)) = (url, user, password) else {
            eprintln!(
                "跳过：未设置 NOVA_JELLYFIN_URL / NOVA_JELLYFIN_USER / NOVA_JELLYFIN_PASSWORD"
            );
            return;
        };

        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = std::sync::Arc::new(Local::open(&env, None).expect("打开状态目录"));
        let outcome = connect(
            Arc::clone(&local),
            &url,
            &user,
            SecretString::new(password.clone()),
        )
        .await;
        let connected = match outcome {
            Ok(connected) => connected,
            Err(error) => panic!("登录失败：{} ({:?})", error.message(), error.kind),
        };

        // SQLite 里出现该服务器。
        let servers = local.db.servers().expect("读服务器");
        assert!(
            servers
                .iter()
                .any(|server| server.info.base_url == connected.info.base_url),
            "数据库里应有这台服务器"
        );

        // 数据库文件里 grep 不到密码。
        let db_path = local.db_path();
        let bytes = std::fs::read(&db_path).expect("读数据库文件");
        assert!(
            !bytes
                .windows(password.len())
                .any(|w| w == password.as_bytes()),
            "数据库文件里不能出现密码明文"
        );
    }
}
