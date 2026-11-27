// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova-storage` 的集成测试：只走公开 API（损坏数据的构造例外，直接用 rusqlite 改文件）。

use std::time::{Duration, Instant};

use nova_core::{
    AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, MediaSource, MediaStream,
    ServerId, ServerInfo, ServerKind, SubtitleMode, UserPreferences,
};
use nova_storage::{CachedImage, Database, StorageError, SyncState};
use rusqlite::Connection;

const SERVER: &str = "server-1";

fn server_id() -> ServerId {
    ServerId::new(SERVER)
}

fn server_info() -> ServerInfo {
    ServerInfo {
        id: server_id(),
        kind: ServerKind::Jellyfin,
        name: "Home".into(),
        base_url: "https://media.example.com:8926".into(),
        version: Some("10.9.0".into()),
        user_id: Some("user-1".into()),
        auth: AuthState::Authorized {
            user_name: "alice".into(),
            credential_key: "nova/jellyfin/server-1/token".into(),
        },
        metadata_language: Language::new("zh-CN"),
    }
}

fn item(id: &str, kind: MediaKind, parent: Option<&str>, index: Option<u32>) -> MediaItem {
    MediaItem {
        id: ItemId::new(id),
        kind,
        title: format!("Title {id}"),
        sort_title: format!("title {id}"),
        original_title: None,
        year: Some(2024),
        overview: Some("简介".into()),
        genres: vec!["剧情".into()],
        runtime: Some(Duration::from_secs(3600)),
        parent_id: parent.map(ItemId::new),
        index_number: index,
        index_number_end: None,
        child_count: None,
        images: ImageSet {
            poster: Some("https://media.example.com/Items/x/Images/Primary".into()),
            ..ImageSet::default()
        },
        user_data: Default::default(),
    }
}

fn movie(id: &str, resume: Duration, played: bool) -> MediaItem {
    let mut entry = item(id, MediaKind::Movie, None, None);
    entry.user_data.resume_position = resume;
    entry.user_data.played = played;
    entry
}

fn source(item_id: &str, source_id: &str) -> MediaSource {
    MediaSource {
        id: nova_core::SourceId::new(source_id),
        item_id: ItemId::new(item_id),
        name: Some("BluRay 2160p".into()),
        container: Some("mkv".into()),
        size: Some(42 * 1024 * 1024 * 1024),
        bitrate: Some(60_000_000),
        runtime: Some(std::time::Duration::from_secs(7245)),
        streams: vec![MediaStream::subtitle(2, "pgs", Language::new("zh-Hans"))],
    }
}

// ---------------------------------------------------------------------------
// 服务器
// ---------------------------------------------------------------------------

#[test]
fn server_crud_roundtrip_and_timestamps() {
    let db = Database::open_in_memory().expect("内存库");
    assert!(db.servers().expect("列表").is_empty());
    assert!(db.server(&server_id()).expect("查询").is_none());

    db.upsert_server(&server_info(), 100).expect("写入");
    let stored = db.server(&server_id()).expect("查询").expect("存在");
    assert_eq!(stored.info, server_info());
    assert_eq!(stored.created_at, 100);
    assert_eq!(stored.last_seen_at, 100);

    // 再次写入：元数据更新，created_at 不变，last_seen_at 前进。
    let mut updated = server_info();
    updated.name = "客厅".into();
    db.upsert_server(&updated, 200).expect("更新");
    let stored = db.server(&server_id()).expect("查询").expect("存在");
    assert_eq!(stored.info.name, "客厅");
    assert_eq!(stored.created_at, 100);
    assert_eq!(stored.last_seen_at, 200);

    // 只刷新时间。
    assert!(db.touch_server(&server_id(), 300).expect("touch"));
    assert_eq!(db.servers().expect("列表")[0].last_seen_at, 300);
    assert!(!db.touch_server(&ServerId::new("nope"), 300).expect("touch"));

    // 不同认证状态也能往返。
    let mut expired = server_info();
    expired.auth = AuthState::Expired {
        user_name: "alice".into(),
    };
    db.upsert_server(&expired, 400).expect("更新");
    assert_eq!(
        db.server(&server_id())
            .expect("查询")
            .expect("存在")
            .info
            .auth,
        AuthState::Expired {
            user_name: "alice".into()
        }
    );

    assert!(db.remove_server(&server_id()).expect("删除"));
    assert!(db.server(&server_id()).expect("查询").is_none());
    assert!(!db.remove_server(&server_id()).expect("重复删除"));
}

// ---------------------------------------------------------------------------
// 条目
// ---------------------------------------------------------------------------

#[test]
fn upsert_items_is_idempotent() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("服务器");

    let entries = vec![
        movie("a", Duration::ZERO, false),
        movie("b", Duration::ZERO, false),
    ];
    db.upsert_items(&server_id(), &entries, 10).expect("写入");
    db.upsert_items(&server_id(), &entries, 20)
        .expect("重复写入");
    assert_eq!(db.item_count(&server_id()).expect("计数"), 2);

    // 内容更新走覆盖。
    let mut changed = entries.clone();
    changed[0].title = "改过的标题".into();
    db.upsert_items(&server_id(), &changed, 30).expect("更新");
    assert_eq!(db.item_count(&server_id()).expect("计数"), 2);
    assert_eq!(
        db.item(&server_id(), &ItemId::new("a"))
            .expect("查询")
            .expect("存在")
            .title,
        "改过的标题"
    );

    // 空切片是合法空操作。
    db.upsert_items(&server_id(), &[], 40).expect("空写入");
    assert_eq!(db.item_count(&server_id()).expect("计数"), 2);
}

#[test]
fn children_are_sorted_by_index_number_with_missing_last() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("服务器");

    let series = item("series", MediaKind::Series, None, None);
    let season = item("season-1", MediaKind::Season, Some("series"), Some(1));
    let episodes = vec![
        item("ep-3", MediaKind::Episode, Some("season-1"), Some(3)),
        item("ep-1", MediaKind::Episode, Some("season-1"), Some(1)),
        item("ep-extra", MediaKind::Episode, Some("season-1"), None),
        item("ep-2", MediaKind::Episode, Some("season-1"), Some(2)),
        // 别的季不应混进来。
        item("ep-other", MediaKind::Episode, Some("season-2"), Some(1)),
    ];
    let mut all = vec![series, season];
    all.extend(episodes);
    db.upsert_items(&server_id(), &all, 1).expect("写入");

    let ids = |items: Vec<MediaItem>| -> Vec<String> {
        items
            .into_iter()
            .map(|entry| entry.id.as_str().to_owned())
            .collect()
    };

    assert_eq!(
        ids(db
            .children(&server_id(), Some(&ItemId::new("season-1")))
            .expect("子项")),
        vec!["ep-1", "ep-2", "ep-3", "ep-extra"]
    );
    // 顶层：只有父项为空的条目。
    assert_eq!(
        ids(db.children(&server_id(), None).expect("顶层")),
        vec!["series"]
    );
    // 季的子项按季号排。
    assert_eq!(
        ids(db
            .children(&server_id(), Some(&ItemId::new("series")))
            .expect("子项")),
        vec!["season-1"]
    );
    // 不存在的父项：空列表，不报错。
    assert!(
        db.children(&server_id(), Some(&ItemId::new("ghost")))
            .expect("子项")
            .is_empty()
    );
}

#[test]
fn continue_watching_matches_is_in_progress_semantics() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("服务器");

    let runtime = Duration::from_secs(3600);
    let cases = vec![
        movie("fresh", Duration::ZERO, false),
        movie("short", Duration::from_secs(10), false),
        movie("resumed", Duration::from_secs(600), false),
        movie("finished", Duration::from_secs(3500), false),
        movie("watched", Duration::from_secs(600), true),
        // 进度超过总时长：比例被 clamp 到 1.0，因此不算"继续观看"。
        movie("overdue", Duration::from_secs(7200), false),
    ];
    for entry in &cases {
        assert_eq!(entry.runtime, Some(runtime));
    }
    db.upsert_items(&server_id(), &cases, 1).expect("写入");

    let got: Vec<String> = db
        .continue_watching(&server_id(), 30)
        .expect("继续观看")
        .into_iter()
        .map(|entry| entry.id.into_inner())
        .collect();
    assert_eq!(got, vec!["resumed"]);

    let got_600: Vec<String> = db
        .continue_watching(&server_id(), 600)
        .expect("继续观看")
        .into_iter()
        .map(|entry| entry.id.into_inner())
        .collect();
    assert_eq!(got_600, vec!["resumed"]);

    // 与 core 的判定逐条对齐。
    for entry in &cases {
        assert_eq!(
            got.iter().any(|id| id == entry.id.as_str()),
            entry.is_in_progress(30),
            "条目 {} 阈值 30",
            entry.id
        );
        assert_eq!(
            got_600.iter().any(|id| id == entry.id.as_str()),
            entry.is_in_progress(600),
            "条目 {} 阈值 600",
            entry.id
        );
    }
}

#[test]
fn continue_watching_is_ordered_by_recent_update() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("服务器");

    // 分三次写入，updated_at 递增。
    db.upsert_items(
        &server_id(),
        &[movie("old", Duration::from_secs(600), false)],
        10,
    )
    .expect("写入");
    db.upsert_items(
        &server_id(),
        &[movie("new", Duration::from_secs(600), false)],
        20,
    )
    .expect("写入");
    let ids: Vec<String> = db
        .continue_watching(&server_id(), 30)
        .expect("继续观看")
        .into_iter()
        .map(|entry| entry.id.into_inner())
        .collect();
    assert_eq!(ids, vec!["new", "old"]);
}

// ---------------------------------------------------------------------------
// 资源 + 级联删除
// ---------------------------------------------------------------------------

#[test]
fn sources_roundtrip_and_cascade_delete() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("服务器");
    db.upsert_items(&server_id(), &[movie("a", Duration::ZERO, false)], 1)
        .expect("条目");

    let sources = vec![source("a", "s1"), source("a", "s2")];
    db.upsert_sources(&server_id(), &sources).expect("写入");
    db.upsert_sources(&server_id(), &sources).expect("重复写入");
    assert_eq!(db.source_count(&server_id()).expect("计数"), 2);

    let back = db.sources(&server_id(), &ItemId::new("a")).expect("读取");
    assert_eq!(back, sources);

    // 删服务器 → items / sources 全部级联删除。
    assert!(db.remove_server(&server_id()).expect("删除服务器"));
    assert_eq!(db.item_count(&server_id()).expect("计数"), 0);
    assert_eq!(db.source_count(&server_id()).expect("计数"), 0);
    assert!(
        db.sources(&server_id(), &ItemId::new("a"))
            .expect("读取")
            .is_empty()
    );

    // 外键真的开着：写资源前必须有条目。
    let written = db.upsert_sources(&server_id(), &[source("ghost", "s1")]);
    assert!(written.is_err(), "缺父条目时外键应拒绝写入");
}

#[test]
fn sync_state_roundtrip() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("服务器");

    assert!(
        db.sync_state(&server_id(), "lib-1")
            .expect("查询")
            .is_none()
    );
    let state = SyncState {
        server_id: server_id(),
        library_id: "lib-1".into(),
        last_sync_at: 1000,
        cursor: Some("cursor-abc".into()),
    };
    db.set_sync_state(&state).expect("写入");
    assert_eq!(
        db.sync_state(&server_id(), "lib-1").expect("查询"),
        Some(state.clone())
    );

    let updated = SyncState {
        last_sync_at: 2000,
        cursor: None,
        ..state.clone()
    };
    db.set_sync_state(&updated).expect("更新");
    assert_eq!(
        db.sync_state(&server_id(), "lib-1").expect("查询"),
        Some(updated)
    );

    // 服务器删掉后同步状态跟着走。
    db.remove_server(&server_id()).expect("删除");
    assert!(
        db.sync_state(&server_id(), "lib-1")
            .expect("查询")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// 偏好
// ---------------------------------------------------------------------------

#[test]
fn preferences_default_then_roundtrip_with_durations() {
    let db = Database::open_in_memory().expect("内存库");

    // 没有记录时返回默认值。
    assert_eq!(
        db.preferences().expect("默认偏好"),
        UserPreferences::default()
    );

    let prefs = UserPreferences {
        subtitle_languages: vec![Language::new("ja"), Language::new("zh-Hant")],
        subtitle_mode: SubtitleMode::ForeignAudioOnly,
        allow_hearing_impaired: true,
        audio_languages: vec![Language::new("ja")],
        prefer_surround_audio: false,
        // `Theme` 目前没有从 nova-core 重新导出，这里只能用默认值（见最终报告）。
        theme: Default::default(),
        start_fullscreen: true,
        // 带纳秒的 Duration 也必须完整往返。
        resume_min_position: Duration::new(45, 250_000_000),
        finished_threshold: Duration::new(120, 999_999_999),
        progress_report_interval: Duration::new(7, 1),
    };
    db.set_preferences(&prefs).expect("写入");
    assert_eq!(db.preferences().expect("读取"), prefs);
    assert_eq!(
        db.preferences().expect("读取").resume_min_position,
        Duration::new(45, 250_000_000)
    );

    // 覆盖写：仍是默认值。
    db.set_preferences(&UserPreferences::default())
        .expect("覆盖");
    assert_eq!(db.preferences().expect("读取"), UserPreferences::default());
}

// ---------------------------------------------------------------------------
// 图片缓存
// ---------------------------------------------------------------------------

#[test]
fn image_cache_lru_eviction() {
    let db = Database::open_in_memory().expect("内存库");
    db.record_image("hash-old", "images/old.jpg", 10)
        .expect("记录");
    db.record_image("hash-mid", "images/mid.jpg", 10)
        .expect("记录");
    db.record_image("hash-new", "images/new.jpg", 10)
        .expect("记录");
    assert_eq!(db.image_cache_bytes().expect("字节数"), 30);

    // 显式指定使用顺序：old 最久未用。
    assert!(db.touch_image("hash-old", 100).expect("touch"));
    assert!(db.touch_image("hash-mid", 200).expect("touch"));
    assert!(db.touch_image("hash-new", 300).expect("touch"));
    assert!(!db.touch_image("hash-missing", 400).expect("touch"));

    // 预算只够两条：淘汰最久未使用的 old。
    let evicted = db.evict_images_to_budget(20).expect("淘汰");
    assert_eq!(evicted, vec!["images/old.jpg".to_owned()]);
    assert_eq!(db.image_cache_bytes().expect("字节数"), 20);
    assert!(db.cached_image("hash-old").expect("查询").is_none());
    let mid: CachedImage = db
        .cached_image("hash-mid")
        .expect("查询")
        .expect("仍在缓存里");
    assert_eq!(mid.rel_path, "images/mid.jpg");
    assert_eq!(mid.bytes, 10);
    assert_eq!(mid.last_used, 200);

    // 已经达标：不再淘汰。
    assert!(db.evict_images_to_budget(20).expect("淘汰").is_empty());

    // 预算 0：全部淘汰，顺序为 LRU（mid 在 new 之前）。
    assert_eq!(
        db.evict_images_to_budget(0).expect("淘汰"),
        vec!["images/mid.jpg".to_owned(), "images/new.jpg".to_owned()]
    );
    assert_eq!(db.image_cache_bytes().expect("字节数"), 0);

    // 重新记录同一张图只更新，不产生第二行。
    db.record_image("hash-a", "images/a.jpg", 5).expect("记录");
    db.record_image("hash-a", "images/a2.jpg", 7).expect("重记");
    assert_eq!(db.image_cache_bytes().expect("字节数"), 7);
    assert_eq!(
        db.cached_image("hash-a")
            .expect("查询")
            .expect("存在")
            .rel_path,
        "images/a2.jpg"
    );
}

// ---------------------------------------------------------------------------
// 迁移与损坏文件
// ---------------------------------------------------------------------------

#[test]
fn migration_is_idempotent_across_opens() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");

    for _ in 0..3 {
        let db = Database::open(&path).expect("打开");
        assert_eq!(db.schema_version().expect("版本"), 4);
        db.upsert_server(&server_info(), 1).expect("写入");
    }

    // 直接看文件里的 user_version。
    let conn = Connection::open(&path).expect("原始连接");
    let version: i64 = conn
        .pragma_query_one(None, "user_version")
        .expect("读取版本");
    assert_eq!(version, 4);

    // 文件库必须走 WAL。
    let journal: String = conn
        .pragma_query_one(None, "journal_mode")
        .expect("读取日志模式");
    assert_eq!(journal.to_ascii_lowercase(), "wal");
}

#[test]
fn schema_newer_than_supported_is_rejected() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");
    drop(Database::open(&path).expect("打开"));
    {
        let conn = Connection::open(&path).expect("原始连接");
        conn.pragma_update(None, "user_version", 99_i64)
            .expect("写入高版本号");
    }

    let error = Database::open(&path).expect_err("高版本应拒绝");
    assert!(
        matches!(
            error,
            StorageError::SchemaTooNew {
                found: 99,
                supported: 4
            }
        ),
        "错误的变体：{error:?}"
    );
}

#[test]
fn migration_failure_rolls_back() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");

    // 造一个"用户手上有旧结构、user_version 还是 0"的库：v1 迁移里的 items 会冲突。
    {
        let conn = Connection::open(&path).expect("原始连接");
        conn.execute_batch("CREATE TABLE items (whatever TEXT) STRICT;")
            .expect("预置冲突表");
    }

    let error = Database::open(&path).expect_err("迁移应失败");
    assert!(
        matches!(error, StorageError::Migrate { from: 0, to: 4, .. }),
        "错误的变体：{error:?}"
    );

    // 回滚：版本没前进，迁移里先建出来的 servers 表也不该留下。
    let conn = Connection::open(&path).expect("原始连接");
    let version: i64 = conn
        .pragma_query_one(None, "user_version")
        .expect("读取版本");
    assert_eq!(version, 0);
    let leftovers: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'servers'",
            [],
            |row| row.get(0),
        )
        .expect("统计");
    assert_eq!(leftovers, 0);
}

#[test]
fn opening_non_sqlite_file_reports_clear_error() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"this is definitely not a sqlite database\n").expect("写文件");

    let error = Database::open(&path).expect_err("应报错而不是 panic");
    match &error {
        StorageError::Open { path: reported, .. } => {
            assert_eq!(reported, &path.display().to_string());
        }
        other => panic!("期望 Open 变体，实际是 {other:?}"),
    }
    assert!(error.to_string().contains("打开 SQLite 数据库失败"));
}

#[test]
fn opening_truncated_database_reports_clear_error() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");
    {
        let db = Database::open(&path).expect("打开");
        db.upsert_server(&server_info(), 1).expect("服务器");
        let entries: Vec<MediaItem> = (0..300)
            .map(|index| movie(&format!("m{index}"), Duration::from_secs(60), false))
            .collect();
        db.upsert_items(&server_id(), &entries, 1).expect("写入");
    }

    let bytes = std::fs::read(&path).expect("读文件");
    assert!(bytes.len() > 8192);
    std::fs::write(&path, &bytes[..1000]).expect("截断");

    let error = Database::open(&path).expect_err("截断的库应报错");
    assert!(
        matches!(error, StorageError::Open { .. }),
        "错误的变体：{error:?}"
    );
}

#[test]
fn corrupt_payload_is_reported_not_skipped() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");
    {
        let db = Database::open(&path).expect("打开");
        db.upsert_server(&server_info(), 1).expect("服务器");
        // 进度 60 秒：既落在"继续观看"的条件里，也能被按父项/按 id 读出来。
        db.upsert_items(
            &server_id(),
            &[movie("a", Duration::from_secs(60), false)],
            1,
        )
        .expect("写入");
    }
    {
        let conn = Connection::open(&path).expect("原始连接");
        conn.execute("UPDATE items SET payload = '{'", [])
            .expect("破坏");
        conn.execute("UPDATE servers SET auth_state = 'nope'", [])
            .expect("破坏");
    }

    let db = Database::open(&path).expect("打开");
    let error = db
        .item(&server_id(), &ItemId::new("a"))
        .expect_err("损坏的 payload 应报错");
    match &error {
        StorageError::CorruptPayload { table, key, .. } => {
            assert_eq!(*table, "items");
            assert!(key.contains("server=server-1"), "key = {key}");
            assert!(key.contains("item=a"), "key = {key}");
        }
        other => panic!("期望 CorruptPayload，实际是 {other:?}"),
    }
    // 列表类查询也不能悄悄跳过。
    assert!(db.children(&server_id(), None).is_err());
    assert!(db.continue_watching(&server_id(), 30).is_err());

    // 服务器行的 JSON 同理。
    assert!(matches!(
        db.server(&server_id()).expect_err("应报错"),
        StorageError::CorruptPayload {
            table: "servers",
            ..
        }
    ));
}

#[test]
fn payload_with_future_fields_is_tolerated() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");
    {
        let db = Database::open(&path).expect("打开");
        db.upsert_server(&server_info(), 1).expect("服务器");
        db.upsert_items(&server_id(), &[movie("a", Duration::ZERO, false)], 1)
            .expect("写入");
        db.set_preferences(&UserPreferences::default())
            .expect("偏好");
    }
    {
        let conn = Connection::open(&path).expect("原始连接");
        let payload: String = conn
            .query_row("SELECT payload FROM items WHERE id = 'a'", [], |row| {
                row.get(0)
            })
            .expect("读 payload");
        let mut value: serde_json::Value = serde_json::from_str(&payload).expect("解析");
        value["future_field"] = serde_json::json!({"added_in": "0.2"});
        assert!(
            value["user_data"]
                .as_object_mut()
                .expect("user_data 是对象")
                .insert("future_nested".into(), serde_json::json!(7))
                .is_none()
        );
        conn.execute("UPDATE items SET payload = ?1", [value.to_string()])
            .expect("写回");

        // 偏好里也有未知字段。
        let prefs: String = conn
            .query_row("SELECT value FROM preferences", [], |row| row.get(0))
            .expect("读偏好");
        let mut value: serde_json::Value = serde_json::from_str(&prefs).expect("解析");
        value["future_pref"] = serde_json::json!(true);
        conn.execute("UPDATE preferences SET value = ?1", [value.to_string()])
            .expect("写回");
    }

    let db = Database::open(&path).expect("打开");
    let read = db
        .item(&server_id(), &ItemId::new("a"))
        .expect("读取")
        .expect("存在");
    assert_eq!(read.id.as_str(), "a");
    assert_eq!(read.title, "Title a");
    assert_eq!(db.preferences().expect("偏好"), UserPreferences::default());
}

// ---------------------------------------------------------------------------
// schema 守卫
// ---------------------------------------------------------------------------

#[test]
fn schema_has_no_credential_columns() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("nova.db");
    drop(Database::open(&path).expect("打开"));

    let conn = Connection::open(&path).expect("原始连接");
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .expect("语句");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("查询")
            .collect::<Result<Vec<_>, _>>()
            .expect("收集")
    };
    assert!(!tables.is_empty());

    let forbidden = ["token", "password", "passwd", "api_key", "apikey", "secret"];
    let mut columns = Vec::new();
    for table in &tables {
        conn.pragma(None, "table_info", table, |row| {
            columns.push(format!("{}.{}", table, row.get::<_, String>(1)?));
            Ok(())
        })
        .expect("table_info");
    }
    assert!(!columns.is_empty());
    for column in &columns {
        let lowered = column.to_ascii_lowercase();
        for word in forbidden {
            assert!(
                !lowered.contains(word),
                "列 {column} 命中敏感词 {word}；令牌只能进系统凭证存储"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 粗测：批量写入
// ---------------------------------------------------------------------------

#[test]
fn insert_5000_items_single_transaction_prints_duration() {
    let entries: Vec<MediaItem> = (0..5000)
        .map(|index| {
            let mut entry = movie(&format!("item-{index}"), Duration::from_secs(60), false);
            entry.user_data.resume_position = Duration::from_secs((index % 3600) as u64);
            entry
        })
        .collect();

    // 内存储与文件库各跑一次：前者是纯 CPU/SQL 成本，后者包含 WAL 写入，
    // 更接近真实首次同步。只打印不断言，便于以后肉眼回归。
    let dir = tempfile::tempdir().expect("临时目录");
    let file_path = dir.path().join("perf.db");
    let targets = [("内存", None), ("文件", Some(file_path.clone()))];
    for (label, path) in targets {
        let db = match &path {
            Some(path) => Database::open(path).expect("打开"),
            None => Database::open_in_memory().expect("内存库"),
        };
        db.upsert_server(&server_info(), 1).expect("服务器");

        let started = Instant::now();
        db.upsert_items(&server_id(), &entries, 1).expect("写入");
        let elapsed = started.elapsed();

        assert_eq!(db.item_count(&server_id()).expect("计数"), 5000);
        eprintln!(
            "[nova-storage] 单事务插入 5000 条 item（{label}）：{:.1} ms（{:.2} µs/条）",
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1_000_000.0 / 5000.0
        );
    }
}

/// 刷新后的清理：按库删除过期条目（含递归到季 / 集），以及"父项不可用"的判定。
#[test]
fn library_cleanup_queries_and_tree_delete() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("写服务器");
    let items = vec![
        item("s1", MediaKind::Series, Some("lib"), None),
        item("s1-1", MediaKind::Season, Some("s1"), Some(1)),
        item("s1-1-1", MediaKind::Episode, Some("s1-1"), Some(1)),
        item("gone", MediaKind::Movie, Some("lib"), None),
        item("keep", MediaKind::Movie, Some("lib"), None),
        // 归属坏掉：父项既不是库也不是本地条目。
        item("dangling", MediaKind::Movie, Some("no-such-parent"), None),
        // 层级断掉：季 / 集没有父项。
        item("stray-episode", MediaKind::Episode, None, Some(2)),
        // 顶层条目（父项 NULL）是合法状态。
        item("top", MediaKind::Movie, None, None),
    ];
    db.upsert_items(&server_id(), &items, 1).expect("写条目");
    let library = ItemId::new("lib");

    // 库内条目：只有直接子项，按类型过滤。
    let ids = db
        .child_ids(&server_id(), &library, &[MediaKind::Movie])
        .expect("列子项");
    let mut ids: Vec<&str> = ids.iter().map(ItemId::as_str).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["gone", "keep"]);

    // 不可达条目：坏归属的电影 + 层级断掉的集；顶层电影与库归属条目不动。
    let libraries = [library.clone()];
    let unreachable = db
        .unreachable_item_ids(&server_id(), &libraries)
        .expect("列不可达");
    let mut unreachable: Vec<&str> = unreachable.iter().map(ItemId::as_str).collect();
    unreachable.sort_unstable();
    assert_eq!(unreachable, ["dangling", "stray-episode"]);

    // 删除剧集要连带它的季与集；其它条目一条不能少。
    let removed = db
        .delete_item_trees(&server_id(), &[ItemId::new("s1"), ItemId::new("gone")])
        .expect("删除子树");
    assert_eq!(removed, 4, "剧集 + 季 + 集 + 电影");
    assert!(
        db.item(&server_id(), &ItemId::new("s1-1-1"))
            .expect("读")
            .is_none()
    );
    assert!(
        db.item(&server_id(), &ItemId::new("s1-1"))
            .expect("读")
            .is_none()
    );
    assert!(
        db.item(&server_id(), &ItemId::new("keep"))
            .expect("读")
            .is_some()
    );
    assert!(
        db.item(&server_id(), &ItemId::new("top"))
            .expect("读")
            .is_some()
    );

    // 删完之后临时表不残留：再删一批只影响这一批。
    let again = db
        .delete_item_trees(&server_id(), &[ItemId::new("dangling")])
        .expect("再删一批");
    assert_eq!(again, 1, "上一批种子不能残留");
}

/// 分批删除期间，另一个线程的读必须能在有界时间内返回。
///
/// `Database` 是 `Mutex<Connection>`：一次删几万行会让界面线程的 `children` /
/// 本地搜索全部堵在锁上（实测：整批删除 3.3 万行期间读要等几分钟）。刷新后的清理
/// 因此按 200 行一批、批间释放锁；这里断言单次读的等待上限。
#[test]
fn chunked_deletes_keep_reads_responsive() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("临时目录");
    let db = Arc::new(Database::open(dir.path().join("nova.db")).expect("建库"));
    db.upsert_server(&server_info(), 1).expect("写服务器");
    let junk: Vec<MediaItem> = (0..4000)
        .map(|index| {
            item(
                &format!("junk-{index}"),
                MediaKind::Episode,
                None,
                Some(index),
            )
        })
        .collect();
    db.upsert_items(&server_id(), &junk, 1).expect("写垃圾行");

    let writer = {
        let db = Arc::clone(&db);
        let sid = server_id();
        let ids: Vec<ItemId> = junk.iter().map(|item| item.id.clone()).collect();
        std::thread::spawn(move || {
            let tree = db.item_tree_ids(&sid, &ids).expect("展开后");
            let mut removed = 0_u64;
            for chunk in tree.chunks(200) {
                removed = removed.saturating_add(db.delete_items(&sid, chunk).expect("删除"));
            }
            removed
        })
    };

    let mut worst = Duration::ZERO;
    let mut reads = 0_u32;
    while !writer.is_finished() {
        let started = Instant::now();
        let _ = db.item_count(&server_id()).expect("读条目数");
        worst = worst.max(started.elapsed());
        reads += 1;
    }
    let removed = writer.join().expect("写线程");
    println!("分批删除期间读 {reads} 次，单次最长 {worst:?}");
    assert_eq!(removed, 4000, "垃圾行全部删掉");
    assert!(reads > 0, "读线程至少跑过一次");
    assert!(
        worst < Duration::from_millis(500),
        "清理期间单次读最长等了 {worst:?}（分批删除应当让读最多等一批）"
    );
    assert_eq!(db.item_count(&server_id()).expect("读条目数"), 0);
}

/// 「最近添加」：跨库按加入时间倒序，只含电影 / 剧集。
#[test]
fn browse_recent_spans_libraries_and_filters_kinds() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("写服务器");
    let mut older = movie("older", Duration::ZERO, false);
    older.parent_id = Some(ItemId::new("lib-a"));
    let mut newer = movie("newer", Duration::ZERO, false);
    newer.parent_id = Some(ItemId::new("lib-b"));
    let mut season = item("season", MediaKind::Season, Some("series"), Some(1));
    season.parent_id = Some(ItemId::new("series"));
    db.upsert_items(&server_id(), &[older], 10).expect("写旧片");
    db.upsert_items(&server_id(), &[newer], 20).expect("写新片");
    db.upsert_items(&server_id(), &[season], 30).expect("写季");

    let kinds = [MediaKind::Movie, MediaKind::Series];
    let recent = db
        .browse_recent(&server_id(), &kinds, 10)
        .expect("最近添加");
    let ids: Vec<&str> = recent.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["newer", "older"], "按 updated_at 倒序，且不含季");

    let limited = db.browse_recent(&server_id(), &kinds, 1).expect("取一条");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id.as_str(), "newer");
}

/// 本地搜索：跨库按标题匹配（库归属已写好的数据），只回电影 / 剧集，`limit` 生效。
///
/// 修复前 `views::search_view` 用的是 `children(None)`（`parent_id IS NULL`），
/// 库归属写对之后这条查询几乎取不到任何条目。
#[test]
fn search_matches_across_libraries_and_skips_hierarchy() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("写服务器");
    let mut dune = movie("dune", Duration::ZERO, false);
    dune.title = "Dune".into();
    dune.sort_title = "Dune".into();
    dune.parent_id = Some(ItemId::new("lib-movies"));
    let mut prophecy = item("prophecy", MediaKind::Series, Some("lib-series"), None);
    prophecy.title = "Dune Prophecy".into();
    prophecy.sort_title = "Dune Prophecy".into();
    let mut episode = item("ep", MediaKind::Episode, Some("prophecy"), Some(1));
    episode.title = "Dune 第 1 集".into();
    episode.sort_title = episode.title.clone();
    let mut season = item("season", MediaKind::Season, Some("prophecy"), Some(1));
    season.title = "Dune 第 1 季".into();
    season.sort_title = season.title.clone();
    db.upsert_items(&server_id(), &[dune, prophecy, episode, season], 10)
        .expect("写条目");

    let kinds = [MediaKind::Movie, MediaKind::Series];
    let hits = db
        .search_items(&server_id(), &kinds, "dune", 10)
        .expect("搜索");
    let ids: Vec<&str> = hits.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["dune", "prophecy"], "大小写不敏感，且不含季 / 集");

    // `limit` 在迭代时生效，且类型过滤先于计数（季 / 集不占名额）。
    let limited = db
        .search_items(&server_id(), &kinds, "dune", 1)
        .expect("搜索");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id.as_str(), "dune");

    // 只要电影时，剧集不出现。
    let movies = db
        .search_items(&server_id(), &[MediaKind::Movie], "dune", 10)
        .expect("搜索");
    assert_eq!(movies.len(), 1);

    // 中文标题按字节包含匹配。
    let cn = db
        .search_items(&server_id(), &kinds, "第 1 季", 10)
        .expect("搜索");
    assert!(cn.is_empty(), "季不在搜索类型里");
}

/// `%` / `_` 必须按字面量匹配，不能当通配符。
#[test]
fn search_escapes_like_wildcards() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("写服务器");
    let mut percent = movie("pct", Duration::ZERO, false);
    percent.title = "100% 纯爱".into();
    percent.sort_title = percent.title.clone();
    let mut underscore = movie("under", Duration::ZERO, false);
    underscore.title = "A_B 计划".into();
    underscore.sort_title = underscore.title.clone();
    let mut plain = movie("plain", Duration::ZERO, false);
    plain.title = "普通片".into();
    plain.sort_title = plain.title.clone();
    db.upsert_items(&server_id(), &[percent, underscore, plain], 10)
        .expect("写条目");

    let kinds = [MediaKind::Movie, MediaKind::Series];
    let hits = db
        .search_items(&server_id(), &kinds, "%", 10)
        .expect("搜索");
    let ids: Vec<&str> = hits.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["pct"]);

    let hits = db
        .search_items(&server_id(), &kinds, "_", 10)
        .expect("搜索");
    let ids: Vec<&str> = hits.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["under"]);

    // 反斜杠本身也是转义符，不能把后面那个字符吃掉。
    let mut slash = movie("slash", Duration::ZERO, false);
    slash.title = r"路径 C:\media".into();
    slash.sort_title = slash.title.clone();
    db.upsert_items(&server_id(), &[slash], 10).expect("写条目");
    let hits = db
        .search_items(&server_id(), &kinds, r"C:\media", 10)
        .expect("搜索");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id.as_str(), "slash");
}

/// `search_items` 与 `search_item_count` 的匹配口径必须完全一致（R6-10）：
/// 两者现在是同一个扫描函数的两半（`search_scan`），超出 `limit` 的部分只计数、
/// 不再解码 payload，所以总数与命中条目不能对不上。
#[test]
fn search_items_and_count_share_the_same_match_semantics() {
    let db = Database::open_in_memory().expect("内存库");
    db.upsert_server(&server_info(), 1).expect("写服务器");
    let mut entries = Vec::new();
    for (index, title) in [
        "Dune",
        "DUNE 第二部",
        "dune 的传说",
        "100% 纯爱",
        "A_B 计划",
        "大饭店",
        "普通片",
    ]
    .into_iter()
    .enumerate()
    {
        let mut hit = movie(&format!("m{index}"), Duration::ZERO, false);
        hit.title = title.into();
        hit.sort_title = hit.title.clone();
        entries.push(hit);
    }
    // 季 / 集永远不参与搜索：计数与条目都要把它们排除。
    let mut episode = item("ep", MediaKind::Episode, Some("show"), Some(1));
    episode.title = "Dune 第 1 集".into();
    episode.sort_title = episode.title.clone();
    entries.push(episode);
    db.upsert_items(&server_id(), &entries, 10).expect("写条目");

    let kinds = [MediaKind::Movie, MediaKind::Series];
    for term in ["dune", "DUNE", "山", "%", "_", "第 1 集", "不存在"] {
        let all = db
            .search_items(&server_id(), &kinds, term, 1000)
            .expect("搜索");
        let count = db
            .search_item_count(&server_id(), &kinds, term)
            .expect("计数");
        assert_eq!(all.len(), count, "term={term:?}: 条目数与计数必须一致");

        let (limited, counted) = db
            .search_items_with_count(&server_id(), &kinds, term, 1)
            .expect("单次扫描");
        assert_eq!(
            limited.len(),
            count.min(1),
            "term={term:?}: limit=1 只回 1 条"
        );
        assert_eq!(counted, count, "term={term:?}: 计数不受 limit 截断");
        assert_eq!(
            limited.first().map(|item| item.id.clone()),
            all.first().map(|item| item.id.clone()),
            "term={term:?}: 截断时返回的必须是同一批的前几条"
        );
    }
}

/// 真机基准（不跑在 CI）：宽泛词的单次遍历 vs 旧的两遍扫描。
///
/// 先备份一份临时副本库，**不要直接读写真实库**：
///
/// ```text
/// sqlite3 ~/.local/share/nova/nova.db ".backup /tmp/nova-bench.db"
/// NOVA_BENCH_DB=/tmp/nova-bench.db cargo test -p nova-storage --test storage \
///     -- --ignored bench_broad_search --nocapture
/// ```
#[test]
#[ignore = "需要 NOVA_BENCH_DB 指向真实库的临时副本"]
fn bench_broad_search_against_a_real_library() {
    let Some(path) = std::env::var("NOVA_BENCH_DB").ok() else {
        eprintln!("跳过：未设置 NOVA_BENCH_DB");
        return;
    };
    let db = Database::open(std::path::Path::new(&path)).expect("打开基准库");
    let server = db
        .servers()
        .expect("读服务器")
        .into_iter()
        .next()
        .expect("基准库里没有服务器")
        .info
        .id;
    // 旧实现（修复前）的两遍扫描用原始 SQL 复现，否则 `search_items` 已经被改成
    // 单次遍历，拿它当“旧遍一”会把旧成本算高（它现在也会数到底）。
    let conn = Connection::open(&path).expect("打开基准库（旧实现复现）");
    let kinds = [MediaKind::Movie, MediaKind::Series];
    for term in ["的", "大", "a"] {
        // 预热（文件系统 / SQLite 页缓存）。
        let _ = db.search_items(&server, &kinds, term, 60).expect("预热");
        let _ = db.search_item_count(&server, &kinds, term).expect("预热");

        let started = Instant::now();
        let (items, total) = db
            .search_items_with_count(&server, &kinds, term, 60)
            .expect("单次遍历");
        let single = started.elapsed();

        // 旧实现 = `search_items`（解码到底、命中 60 就 break）+ `search_item_count`
        // （只读 kind/title 再扫一遍）。
        let started = Instant::now();
        let old_items = old_search_items(&conn, &server, &kinds, term, 60);
        let old_items_time = started.elapsed();
        let started = Instant::now();
        let old_total = old_search_item_count(&conn, &server, &kinds, term);
        let old_count_time = started.elapsed();
        let old_total_time = old_items_time + old_count_time;

        eprintln!(
            "term={term:?} 命中={total}（旧计数={old_total}） 返回={}（旧返回={old_items}） \
             单次={single:?} 旧两遍={old_total_time:?}（items={old_items_time:?} + \
             count={old_count_time:?}）",
            items.len()
        );
    }
}

/// 修复前 `Database::search_items` 的等价复现（解码 payload，命中 `limit` 就 break）。
fn old_search_items(
    conn: &Connection,
    server: &ServerId,
    kinds: &[MediaKind],
    term: &str,
    limit: usize,
) -> usize {
    let needle = term.trim().to_lowercase();
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, payload FROM items \
             WHERE server_id = ?1 AND title LIKE ?2 ESCAPE '\\' \
             ORDER BY updated_at DESC, sort_title, id",
        )
        .expect("准备旧搜索语句");
    let mut rows = stmt
        .query(rusqlite::params![server.as_str(), like_pattern(&needle)])
        .expect("查询旧搜索");
    let mut count = 0usize;
    while let Some(row) = rows.next().expect("旧搜索结果行") {
        let kind: String = row.get(1).expect("kind");
        if !kinds.iter().any(|candidate| {
            let text = match candidate {
                MediaKind::Movie => "movie",
                MediaKind::Series => "series",
                _ => "",
            };
            text == kind
        }) {
            continue;
        }
        let id: String = row.get(0).expect("id");
        let payload: String = row.get(2).expect("payload");
        let item: MediaItem = serde_json::from_str(&payload).expect("payload 解码");
        assert!(item.id.as_str() == id);
        if !item.title.to_lowercase().contains(&needle) {
            continue;
        }
        count += 1;
        if count >= limit {
            break;
        }
    }
    count
}

/// 修复前 `Database::search_item_count` 的等价复现（只读 kind/title）。
fn old_search_item_count(
    conn: &Connection,
    server: &ServerId,
    kinds: &[MediaKind],
    term: &str,
) -> usize {
    let needle = term.trim().to_lowercase();
    let mut stmt = conn
        .prepare(
            "SELECT kind, title FROM items \
             WHERE server_id = ?1 AND title LIKE ?2 ESCAPE '\\'",
        )
        .expect("准备旧计数语句");
    let mut rows = stmt
        .query(rusqlite::params![server.as_str(), like_pattern(&needle)])
        .expect("查询旧计数");
    let mut count = 0usize;
    while let Some(row) = rows.next().expect("旧计数行") {
        let kind: String = row.get(0).expect("kind");
        if !kinds.iter().any(|candidate| {
            let text = match candidate {
                MediaKind::Movie => "movie",
                MediaKind::Series => "series",
                _ => "",
            };
            text == kind
        }) {
            continue;
        }
        let title: String = row.get(1).expect("title");
        if title.to_lowercase().contains(&needle) {
            count += 1;
        }
    }
    count
}

fn like_pattern(needle: &str) -> String {
    let mut pattern = String::with_capacity(needle.len() + 2);
    pattern.push('%');
    for character in needle.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// 库归属规则只有一份（`nova_storage::library_items`）：电影 / 剧集的父项一律
/// 写成库，季 / 集丢弃。CLI 与桌面端刷新都走它。
#[test]
fn library_items_force_membership_and_drop_hierarchy() {
    let mut in_folder = movie("in-folder", Duration::ZERO, false);
    in_folder.parent_id = Some(ItemId::new("folder-physical"));
    let mut no_parent = movie("no-parent", Duration::ZERO, false);
    no_parent.parent_id = None;
    let mut series = item("series", MediaKind::Series, None, None);
    series.parent_id = Some(ItemId::new("lib"));
    let season = item("season", MediaKind::Season, Some("series"), Some(1));
    let episode = item("episode", MediaKind::Episode, Some("season"), Some(1));

    let library = ItemId::new("lib");
    let normalized = nova_storage::library_items(
        vec![in_folder, no_parent, series, season, episode],
        &library,
    );
    let ids: Vec<&str> = normalized.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["in-folder", "no-parent", "series"]);
    assert!(
        normalized
            .iter()
            .all(|item| item.parent_id.as_ref() == Some(&library)),
        "列表刷新 / CLI 同步是库归属的权威来源"
    );
}
