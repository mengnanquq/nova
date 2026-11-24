// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 端到端命令行测试：`--print-view` 与 `--snapshot`。
//!
//! 这些测试**不联网**：要么是空数据目录（降级路径），要么是预置了 SQLite 记录的
//! 临时目录（缓存路径）。真实服务端的用例见 `jellyfin_live.rs`。

use std::path::Path;
use std::process::Command;

use nova_core::{
    AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo, ServerKind,
    UserData,
};
use nova_storage::Database;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_nova-desktop")
}

fn run(data_dir: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(binary())
        .args(args)
        .env("NOVA_DATA_DIR", data_dir)
        .env("NOVA_CREDENTIALS", "memory")
        .env("NOVA_LOG", "off")
        .output()
        .expect("应当能启动 nova-desktop");
    assert!(
        output.status.success(),
        "命令失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn server_info() -> ServerInfo {
    ServerInfo {
        id: ServerId::new("srv-test"),
        kind: ServerKind::Jellyfin,
        name: "测试服务器".to_owned(),
        base_url: "http://127.0.0.1:1".to_owned(),
        version: None,
        user_id: None,
        auth: AuthState::Unauthorized,
        metadata_language: Language::undetermined(),
    }
}

fn movie(id: &str, title: &str) -> MediaItem {
    MediaItem {
        id: ItemId::new(id),
        kind: MediaKind::Movie,
        title: title.to_owned(),
        sort_title: title.to_owned(),
        original_title: None,
        year: Some(2024),
        overview: Some("缓存里的简介".to_owned()),
        genres: vec!["科幻".to_owned()],
        runtime: Some(std::time::Duration::from_secs(2 * 3600)),
        parent_id: None,
        index_number: None,
        index_number_end: None,
        child_count: None,
        images: ImageSet::default(),
        user_data: UserData::default(),
    }
}

/// 预置一份"已经同步过"的缓存。
fn seed(data_dir: &Path) -> ServerInfo {
    let db = Database::open(data_dir.join("nova.db")).expect("建库");
    let info = server_info();
    db.upsert_server(&info, 1).expect("写服务器");
    db.upsert_items(&info.id, &[movie("m1", "Alpha"), movie("m2", "Beta")], 1)
        .expect("写条目");
    info
}

#[test]
fn print_view_without_server_is_offline_and_empty() {
    let dir = tempfile::tempdir().expect("临时目录");
    let output = run(dir.path(), &["--print-view"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("status: 未连接服务器"), "{stdout}");
    assert!(stdout.contains("hint: 先在终端运行 nova login"), "{stdout}");
    // 状态不再占用 hero 位：hero 为空（“-”），状态由顶部横幅表达。
    assert!(stdout.contains("hero: -"), "{stdout}");
    assert!(stdout.contains("recent: 0"), "{stdout}");
    assert!(stdout.contains("continue: 0"), "{stdout}");
    // 空库启动也不该创建快照之外的东西；数据库与图片目录都已就绪。
    assert!(dir.path().join("nova.db").is_file());
    assert!(dir.path().join("images").is_dir());
}

#[test]
fn print_view_reads_preseeded_cache_without_network() {
    let dir = tempfile::tempdir().expect("临时目录");
    seed(dir.path());
    let output = run(dir.path(), &["--print-view"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 服务器存在但内存凭证里没有令牌 → 明确的"未登录"状态。
    assert!(stdout.contains("status: 未登录"), "{stdout}");
    // 「首页」标签始终在第一位，其后才是库标签（离线时是合成的「全部」）。
    assert!(stdout.contains("tabs: 首页,全部"), "{stdout}");
    assert!(stdout.contains("recent: 2"), "{stdout}");
    assert!(stdout.contains("recent[0]: Alpha"), "{stdout}");
    assert!(stdout.contains("recent[1]: Beta"), "{stdout}");
}

#[test]
fn print_view_search_filters_local_cache() {
    let dir = tempfile::tempdir().expect("临时目录");
    seed(dir.path());
    let output = run(dir.path(), &["--print-view", "--search", "bet"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("recent: 1"), "{stdout}");
    assert!(stdout.contains("recent[0]: Beta"), "{stdout}");
    assert!(!stdout.contains("recent[0]: Alpha"), "{stdout}");
}

#[test]
fn snapshot_renders_headless_without_a_compositor() {
    let dir = tempfile::tempdir().expect("临时目录");
    seed(dir.path());
    let png = dir.path().join("home.png");
    let output = run(
        dir.path(),
        &[
            "--snapshot",
            png.to_str().expect("临时路径是 UTF-8"),
            "--no-refresh",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot: 1280x800"), "{stdout}");
    assert!(png.is_file(), "快照文件应当存在");
    let bytes = std::fs::read(&png).expect("读快照");
    assert!(bytes.len() > 1024, "快照不应是空文件");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "应当是 PNG");
}

#[test]
fn snapshot_detail_route_uses_selected_item() {
    let dir = tempfile::tempdir().expect("临时目录");
    seed(dir.path());
    let png = dir.path().join("detail.png");
    let output = run(
        dir.path(),
        &[
            "--snapshot",
            png.to_str().expect("临时路径是 UTF-8"),
            "--route",
            "detail",
            "--item",
            "m2",
            "--no-refresh",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot: 1280x800"), "{stdout}");
    assert!(png.is_file());
}
