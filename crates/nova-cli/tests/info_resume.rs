// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova info` 的「观看」行必须与桌面端读同一套续播判断。
//!
//! 修复前这一行是 `played` 优先：服务端把一部看完的片子重新标成未看完、或给
//! `Played = true` 的条目保留一个中段续播点时，CLI 显示「已看完」，而桌面端
//! 的详情页按钮/起播位置都从续播点开始（`resume_position_for_item`）。
//! 这里只读本地缓存，不发任何网络请求。

use std::time::Duration;

use nova_cli::{App, CredentialMode, Env, run_in};
use nova_core::{
    AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo, ServerKind,
    UserData, UserPreferences,
};

const SERVER_ID: &str = "srv-cli-info";
const ITEM_ID: &str = "played-with-mid-point";

fn env(dir: &std::path::Path) -> Env {
    Env {
        data_dir: dir.to_path_buf(),
        credentials: CredentialMode::Memory,
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn server_info() -> ServerInfo {
    ServerInfo {
        id: ServerId::new(SERVER_ID),
        kind: ServerKind::Jellyfin,
        name: "测试服务器".to_owned(),
        base_url: "http://127.0.0.1:1".to_owned(),
        version: None,
        user_id: Some("user-cli".to_owned()),
        auth: AuthState::Authorized {
            user_name: "nova".to_owned(),
            credential_key: "jellyfin/srv-cli-info/token".to_owned(),
        },
        metadata_language: Language::undetermined(),
    }
}

/// `played = true` + 45:47 的中段续播点（片长 91:23，离片尾还很远）。
fn played_item_with_mid_resume_point() -> MediaItem {
    MediaItem {
        id: ItemId::new(ITEM_ID),
        kind: MediaKind::Movie,
        title: "看完又拖回中段的片子".to_owned(),
        sort_title: "看完又拖回中段的片子".to_owned(),
        original_title: None,
        year: Some(2025),
        overview: None,
        genres: Vec::new(),
        runtime: Some(Duration::from_secs(5483)),
        parent_id: None,
        index_number: None,
        index_number_end: None,
        child_count: None,
        images: ImageSet::default(),
        user_data: UserData {
            played: true,
            resume_position: Duration::from_secs(2747),
            play_count: 1,
            favorite: false,
        },
    }
}

#[tokio::test]
async fn info_watch_line_uses_the_same_resume_rule_as_the_desktop_app() {
    let dir = tempfile::tempdir().expect("临时目录");
    let env = env(dir.path());
    {
        let app = App::open(&env).expect("打开状态目录");
        app.database()
            .upsert_server(&server_info(), 1)
            .expect("写服务器");
        app.database()
            .upsert_items(
                &ServerId::new(SERVER_ID),
                &[played_item_with_mid_resume_point()],
                1,
            )
            .expect("写条目");
    }

    let text = run_in(&env, argv(&["info", ITEM_ID]))
        .await
        .expect("info 应当成功");
    assert!(
        text.contains("观看    : 看到 45:47"),
        "played=true 且有中段续播点时，显示位置要和桌面端起播位置一致：\n{text}"
    );
    assert!(
        !text.contains("已看完"),
        "played=true 不代表已看完：续播点仍在有效区间内\n{text}"
    );

    // 偏好必须从本地库读，而不是写死默认值：把最小续播位置调到续播点之后，
    // 同一个条目就该落到「已看完」（`resume_position_for_item` 返回 0）。
    {
        let app = App::open(&env).expect("打开状态目录");
        let prefs = UserPreferences {
            resume_min_position: Duration::from_secs(3600),
            ..UserPreferences::default()
        };
        app.database().set_preferences(&prefs).expect("写偏好");
    }
    let text = run_in(&env, argv(&["info", ITEM_ID]))
        .await
        .expect("info 应当成功");
    assert!(
        text.contains("观看    : 已看完"),
        "续播点低于 resume_min_position 时应显示已看完（偏好来自本地库）：\n{text}"
    );
}
