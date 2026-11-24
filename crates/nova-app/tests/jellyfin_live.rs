// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 真实 Jellyfin 的可选用例：读到 `NOVA_JELLYFIN_*` 才跑（见 `docs/testing.md`）。
//!
//! 断言的是"装配链路"而不是服务端细节：
//! 1. 登录 + 刷新后 SQLite 条目数增加；
//! 2. 能构建出带真实标题的首页；
//! 3. 服务端提供海报时，图片文件落到 `<data>/images` 缓存目录。
//!
//! 开发用的 Jellyfin 没有配元数据源，`ImageTags` 可能为空（文档里明确写了），
//! 因此第 3 条在"一个条目都没有海报"时只打印说明，不做虚假断言。

use std::env;
use std::sync::Arc;

use nova_app::env::{CredentialMode, Env};
use nova_app::images::{ImageService, ProviderFetcher};
use nova_app::local::Local;
use nova_app::refresh::refresh;
use nova_app::status::Status;
use nova_app::status::Status::Connected;
use nova_core::{AuthState, Language, ServerId, ServerInfo, ServerKind};
use nova_provider::MediaProvider;
use nova_secrets::SecretString;
use nova_storage::Database;

struct Live {
    url: String,
    user: String,
    password: String,
}

fn live_env() -> Option<Live> {
    let url = env::var("NOVA_JELLYFIN_URL")
        .ok()
        .filter(|v| !v.is_empty())?;
    let user = env::var("NOVA_JELLYFIN_USER")
        .ok()
        .filter(|v| !v.is_empty())?;
    let password = env::var("NOVA_JELLYFIN_PASS")
        .ok()
        .filter(|v| !v.is_empty())?;
    Some(Live {
        url,
        user,
        password,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_persists_items_and_images() {
    let Some(live) = live_env() else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL / USER / PASS");
        return;
    };

    let dir = tempfile::tempdir().expect("临时目录");
    let info = ServerInfo {
        id: ServerId::new("srv-live"),
        kind: ServerKind::Jellyfin,
        name: "实时测试".to_owned(),
        base_url: live.url.clone(),
        version: None,
        user_id: None,
        auth: AuthState::Unauthorized,
        metadata_language: Language::undetermined(),
    };
    {
        let db = Database::open(dir.path().join("nova.db")).expect("建库");
        db.upsert_server(&info, 1).expect("写服务器");
    }
    let environment = Env {
        data_dir: dir.path().to_path_buf(),
        credentials: CredentialMode::Memory,
    };
    let local = Local::open(&environment, None).expect("打开状态目录");
    let provider = local
        .provider
        .clone()
        .expect("即便没令牌也应构造出 provider");
    provider
        .authenticate(&live.user, &SecretString::new(live.password))
        .await
        .expect("登录实时服务器");
    local.set_status(Connected {
        server: info.name.clone(),
        base_url: live.url.clone(),
    });
    let local = Arc::new(local);

    let before = local.db.item_count(&info.id).expect("统计刷新前条目");
    let report = refresh(Arc::clone(&local), local.status()).await;
    assert!(
        matches!(report.status, Status::Connected { .. }),
        "刷新应当保持已连接：{:?}",
        report.status
    );
    let after = local.db.item_count(&info.id).expect("统计刷新后条目");
    assert!(after > 0, "刷新后本地应有条目");
    assert!(after >= before, "条目数不应减少");

    let home = local.home(None).expect("构建首页");
    let titles: Vec<String> = home
        .continue_watching
        .iter()
        .chain(home.recently_added.iter())
        .map(|card| card.title.clone())
        .collect();
    assert!(!titles.is_empty(), "首页应出现真实条目名：{titles:?}");
    eprintln!("真实条目：{titles:?}");

    // 图片：找一个服务端真的给了海报的条目；开发服务器可能一个都没有。
    let items = local.db.children(&info.id, None).expect("读缓存条目");
    let with_poster = items
        .iter()
        .find(|item| item.images.poster.is_some())
        .map(|item| nova_ui::ImageKey::poster(item.id.as_str()));
    let Some(key) = with_poster else {
        eprintln!("跳过图片断言：本地 Jellyfin 没有为任何条目提供海报（未配置元数据源，符合预期）");
        return;
    };
    let spec = local.image_spec(&key).expect("应当能构造图片请求");
    let fetcher = Arc::new(ProviderFetcher::new(local.provider_dyn()));
    let service = ImageService::new(Arc::clone(&local.image_cache), fetcher);
    let loaded = service.load(&spec).await.expect("应当能下载并解码海报");
    assert_eq!(loaded.0, key);
    let cache_path = local
        .image_cache
        .path_for(&nova_images::cache_key(&spec.url, spec.tag.as_deref()));
    assert!(cache_path.is_file(), "图片应落到 {}", cache_path.display());
    eprintln!("图片缓存：{}", cache_path.display());
}
