// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova-core` 媒体模型 → `nova-ui` 视图模型。
//!
//! 这里是"装配"的核心，全部是纯函数/同步函数：只读 [`Database`]，不发网络请求。
//! 抽出来的好处是离线启动、`--print-view`、`--snapshot`、窗口模式跑的是同一套
//! 视图构建逻辑，测试可以直接断言卡片数量与顺序。

use std::time::Duration;

use nova_core::{ItemId, MediaItem, MediaKind, MediaSource, ServerId, UserPreferences};
use nova_storage::Database;
use nova_ui::viewmodel::{detail_meta_line, format_duration, meta_line};
use nova_ui::{
    CardView, DetailView, HomeView, LibraryRowView, LibraryTab, LibraryView, PlayerShellView,
    SearchView, SourceOption,
};

use crate::status::Status;

/// 播放页状态文案（阶段 3 只有外壳）。
pub const PLAYER_STATUS: &str = "播放引擎将在阶段 4 接入";

/// 媒体库页没有服务端库归属时的说明。
const FALLBACK_SORT_LABEL: &str = "本地缓存（服务端列表不含 ParentId）";

/// 默认排序条文案。
const DEFAULT_SORT_LABEL: &str = "排序：最近添加";

/// 用本地缓存构建首页。
///
/// 每个库行最多展示的条目数（Netflix 首页每行约 20 张卡）。
const LIBRARY_ROW_LIMIT: usize = 20;

/// 顺序：
/// 1. “继续观看”来自 [`Database::continue_watching`]（SQL 侧过滤）；
/// 2. “最近添加”来自顶层条目（按 `sort_title`，服务端列表不返回 `ParentId` 时也成立）；
/// 3. 各媒体库前 [`LIBRARY_ROW_LIMIT`] 条（标题 = 库名；缓存里没有该库的条目时整行略过）；
/// 4. hero 取继续观看第一条，否则最近添加第一条；
/// 5. 离线/过期状态不再占用 hero 位：hero 始终是真实内容，状态由顶部横幅表达
///    （`Status::banner_data`，窗口模式在 `ui.rs` 里接入）。
pub fn home_view(
    db: &Database,
    server_id: &ServerId,
    preferences: &UserPreferences,
    _status: &Status,
    tabs: Vec<LibraryTab>,
    search: Option<&str>,
) -> Result<HomeView, nova_storage::StorageError> {
    let mut view = HomeView::empty(tabs);
    let in_progress = db.continue_watching(server_id, preferences.resume_min_position.as_secs())?;
    let recent = db.children(server_id, None)?;

    view.continue_watching = in_progress.iter().map(card_view).collect();
    view.recently_added = recent.iter().map(card_view).collect();
    view.library_rows = view
        .library_tabs
        .iter()
        .filter(|tab| !tab.id.is_empty())
        .map(
            |tab| -> Result<LibraryRowView, nova_storage::StorageError> {
                let parent = ItemId::new(tab.id.clone());
                let items = db
                    .children(server_id, Some(&parent))?
                    .into_iter()
                    .take(LIBRARY_ROW_LIMIT)
                    .map(|item| card_view(&item))
                    .collect();
                Ok(LibraryRowView {
                    title: tab.title.clone(),
                    items,
                })
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    view.hero = view
        .continue_watching
        .first()
        .cloned()
        .or_else(|| view.recently_added.first().cloned());

    apply_home_search(&mut view, search);
    Ok(view)
}

/// 用本地缓存构建媒体库页。
///
/// Jellyfin 12.4 的列表响应不带 `ParentId`（见 `docs/testing.md`），所以按库查缓存
/// 可能为空；此时与 CLI 的 `--cached` 一样退化为"按顶层列出"，并在排序条上说明原因。
pub fn library_view(
    db: &Database,
    server_id: &ServerId,
    tab: &LibraryTab,
    tabs: Vec<LibraryTab>,
    search: Option<&str>,
) -> Result<LibraryView, nova_storage::StorageError> {
    let parent = if tab.id.is_empty() {
        None
    } else {
        Some(ItemId::new(tab.id.clone()))
    };
    let mut items = db.children(server_id, parent.as_ref())?;
    let mut sort_label = DEFAULT_SORT_LABEL.to_owned();
    if items.is_empty() && parent.is_some() {
        let top_level = db.children(server_id, None)?;
        if !top_level.is_empty() {
            items = top_level;
            sort_label = FALLBACK_SORT_LABEL.to_owned();
        }
    }

    let mut cards: Vec<CardView> = items.iter().map(card_view).collect();
    if let Some(term) = search {
        retain_matching(&mut cards, term);
    }

    let current_tab = tabs
        .iter()
        .position(|candidate| candidate.id == tab.id)
        .unwrap_or(0);
    Ok(LibraryView {
        id: tab.id.clone(),
        title: tab.title.clone(),
        sort_label,
        cards,
        library_tabs: tabs,
        current_tab,
    })
}

/// 搜索页一次最多展示的结果数（与服务端的 `limit` 一致）。
pub const SEARCH_LIMIT: usize = 60;

/// 「找片」页只收这两种类型。
///
/// 集（Episode）/ 季（Season）是播放层级，只应出现在详情页的选集里；
/// 混进搜索结果会让"找片"变成"找某一集"，所以这里明确过滤掉。
pub fn is_search_kind(kind: MediaKind) -> bool {
    matches!(kind, MediaKind::Movie | MediaKind::Series)
}

/// 媒体条目 → 搜索结果卡片。
///
/// 复用 [`card_view`]（首页/媒体库同一个映射函数），只额外过滤类型。
pub fn search_cards(items: &[MediaItem]) -> Vec<CardView> {
    items
        .iter()
        .filter(|item| is_search_kind(item.kind))
        .map(card_view)
        .collect()
}

/// 由已经拿到的条目构建搜索结果视图（服务端搜索回来后用这个）。
pub fn search_view_from_items(term: &str, items: &[MediaItem]) -> SearchView {
    SearchView::from_cards(term, search_cards(items))
}

/// 用本地缓存构建搜索结果页（离线可用，也是输入时的即时反馈）。
///
/// 缓存里没有服务端的搜索索引，这里按"标题包含关键字"做与首页/媒体库一致的
/// 本地过滤；联网结果回来后界面会被服务端结果替换。
pub fn search_view(
    db: &Database,
    server_id: &ServerId,
    term: &str,
    limit: usize,
) -> Result<SearchView, nova_storage::StorageError> {
    let mut items = db.children(server_id, None)?;
    let needle = term.trim().to_lowercase();
    if !needle.is_empty() {
        items.retain(|item| matches(&item.title, &needle));
    }
    items.retain(|item| is_search_kind(item.kind));
    items.truncate(limit);
    Ok(search_view_from_items(term, &items))
}

/// 用本地缓存构建详情页；条目不在缓存里时返回 `None`。
pub fn detail_view(
    db: &Database,
    server_id: &ServerId,
    item_id: &str,
) -> Result<Option<DetailView>, nova_storage::StorageError> {
    let id = ItemId::new(item_id);
    let Some(item) = db.item(server_id, &id)? else {
        return Ok(None);
    };
    let sources = db.sources(server_id, &id)?;
    Ok(Some(detail_from_item(&item, &sources)))
}

/// 用本地缓存构建播放外壳；条目不在缓存里时返回 `None`。
pub fn player_view(
    db: &Database,
    server_id: &ServerId,
    item_id: &str,
) -> Result<Option<PlayerShellView>, nova_storage::StorageError> {
    let Some(item) = db.item(server_id, &ItemId::new(item_id))? else {
        return Ok(None);
    };
    Ok(Some(player_from_item(&item)))
}

/// 把一条媒体条目压成卡片。
pub fn card_view(item: &MediaItem) -> CardView {
    let progress = if item.user_data.played {
        None
    } else {
        item.progress_fraction().filter(|fraction| *fraction > 0.0)
    };
    let mut card = CardView::new(item.id.as_str(), item.title.clone());
    if let Some(subtitle) = subtitle_for(item) {
        card = card.with_subtitle(subtitle);
    }
    if let Some(overview) = &item.overview {
        // billboard 最多 3 行简介；这里不截断，交给界面裁剪。
        card = card.with_overview(overview.clone());
    }
    if let Some(progress) = progress {
        card = card.with_progress(progress);
    }
    card
}

/// 把条目 + 可播放版本压成详情页视图模型。
pub fn detail_from_item(item: &MediaItem, sources: &[MediaSource]) -> DetailView {
    let mut view = DetailView::new(item.id.as_str(), item.title.clone());
    view.original_title = item.original_title.clone();
    view.meta_line = detail_meta_line(item.year, item.runtime, &item.genres);
    view.overview = item.overview.clone();
    view.play_label = play_label(item.user_data.resume_position);
    view.genres = item.genres.clone();
    view.sources = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let title = source
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("版本 {}", index + 1));
            let mut option = SourceOption::new(source.id.as_str(), title).with_selected(index == 0);
            if let Some(subtitle) = source_subtitle(source) {
                option = option.with_subtitle(subtitle);
            }
            option
        })
        .collect();
    view
}

/// 把条目压成播放页视图模型。
pub fn player_from_item(item: &MediaItem) -> PlayerShellView {
    let mut view = PlayerShellView::new(item.title.clone())
        .with_position(item.user_data.resume_position)
        .with_playing(false)
        .with_engine_line(PLAYER_STATUS);
    if let Some(runtime) = item.runtime {
        view = view.with_duration(runtime);
    }
    view
}

/// `--print-view` 的稳定摘要格式（脚本/CI 断言用）。
pub fn summary(status: &Status, home: &HomeView) -> String {
    let mut lines = status.summary_lines();
    lines.push(format!(
        "hero: {}",
        home.hero
            .as_ref()
            .map(|hero| hero.title.as_str())
            .unwrap_or("-")
    ));
    lines.push(format!(
        "tabs: {}",
        home.library_tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>()
            .join(",")
    ));
    lines.push(format!("continue: {}", home.continue_watching.len()));
    lines.push(format!("recent: {}", home.recently_added.len()));
    for (index, card) in home.continue_watching.iter().enumerate() {
        lines.push(format!("continue[{index}]: {}", card.title));
    }
    for (index, card) in home.recently_added.iter().enumerate() {
        lines.push(format!("recent[{index}]: {}", card.title));
    }
    lines.join("\n")
}

pub(crate) fn subtitle_for(item: &MediaItem) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match item.kind {
        MediaKind::Episode => {
            if let Some(index) = item.index_number {
                parts.push(format!("第 {index} 集"));
            }
        }
        MediaKind::Season => {
            if let Some(index) = item.index_number {
                parts.push(format!("第 {index} 季"));
            }
        }
        MediaKind::Movie | MediaKind::Series => {}
    }
    if let Some(year) = item.year {
        parts.push(year.to_string());
    }
    if !item.genres.is_empty() {
        parts.push(
            item.genres
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" / "),
        );
    }
    non_empty(meta_line(&refs(&parts)))
}

fn source_subtitle(source: &MediaSource) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(container) = source
        .container
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        parts.push(container.to_owned());
    }
    if let Some(video) = source.video_stream() {
        parts.push(video.codec.clone());
    }
    if let Some(bitrate) = source.bitrate.filter(|value| *value > 0) {
        #[allow(clippy::cast_precision_loss)]
        let mbps = bitrate as f64 / 1_000_000.0;
        parts.push(format!("{mbps:.1} Mbps"));
    }
    if let Some(runtime) = source.runtime.filter(|value| !value.is_zero()) {
        parts.push(format_duration(runtime));
    }
    non_empty(meta_line(&refs(&parts)))
}

fn play_label(resume: Duration) -> String {
    if resume.as_secs() == 0 {
        "播放".to_owned()
    } else {
        format!("从 {} 继续", clock_text(resume))
    }
}

/// `Duration` → `"1:12:34"` / `"12:34"`。
fn clock_text(duration: Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn refs(parts: &[String]) -> Vec<&str> {
    parts.iter().map(String::as_str).collect()
}

fn non_empty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

fn apply_home_search(view: &mut HomeView, search: Option<&str>) {
    let Some(term) = search else { return };
    let needle = term.to_lowercase();
    view.continue_watching
        .retain(|card| matches(&card.title, &needle));
    view.recently_added
        .retain(|card| matches(&card.title, &needle));
    for row in &mut view.library_rows {
        row.items.retain(|card| matches(&card.title, &needle));
    }
    let hero_matches = view.hero.as_ref().is_some_and(|hero| {
        hero.item_id == crate::status::STATUS_ITEM_ID || matches(&hero.title, &needle)
    });
    if !hero_matches {
        view.hero = view
            .continue_watching
            .first()
            .cloned()
            .or_else(|| view.recently_added.first().cloned());
    }
}

fn retain_matching(cards: &mut Vec<CardView>, term: &str) {
    let needle = term.to_lowercase();
    cards.retain(|card| matches(&card.title, &needle));
}

fn matches(title: &str, needle: &str) -> bool {
    title.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nova_core::{
        AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo,
        ServerKind, UserData, UserPreferences,
    };
    use nova_storage::{Database, StoredServer};

    use super::*;

    fn item(
        id: &str,
        title: &str,
        kind: MediaKind,
        resume: Duration,
        runtime: Option<u64>,
    ) -> MediaItem {
        MediaItem {
            id: ItemId::new(id),
            kind,
            title: title.to_owned(),
            sort_title: title.to_owned(),
            original_title: None,
            year: Some(2024),
            overview: Some("简介".to_owned()),
            genres: vec!["科幻".to_owned()],
            runtime: runtime.map(Duration::from_secs),
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet::default(),
            user_data: UserData {
                played: false,
                resume_position: resume,
                play_count: 0,
                favorite: false,
            },
        }
    }

    fn server(id: &str) -> StoredServer {
        StoredServer {
            info: ServerInfo {
                id: ServerId::new(id),
                kind: ServerKind::Jellyfin,
                name: "测试服务器".to_owned(),
                base_url: "http://127.0.0.1:8096".to_owned(),
                version: None,
                user_id: None,
                auth: AuthState::Unauthorized,
                metadata_language: Language::undetermined(),
            },
            created_at: 1,
            last_seen_at: 1,
        }
    }

    #[test]
    fn home_orders_continue_first_and_maps_cards() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        db.upsert_items(
            &server.info.id,
            &[
                item(
                    "a",
                    "A 片",
                    MediaKind::Movie,
                    Duration::from_secs(0),
                    Some(6000),
                ),
                item(
                    "b",
                    "B 片",
                    MediaKind::Movie,
                    Duration::from_secs(1200),
                    Some(6000),
                ),
            ],
            1,
        )
        .expect("写条目");

        let home = home_view(
            &db,
            &server.info.id,
            &UserPreferences::default(),
            &Status::Connected {
                server: "测试服务器".to_owned(),
                base_url: "http://127.0.0.1:8096".to_owned(),
            },
            vec![LibraryTab::new("lib", "电影")],
            None,
        )
        .expect("构建首页");

        assert_eq!(home.continue_watching.len(), 1);
        assert_eq!(home.continue_watching[0].title, "B 片");
        assert_eq!(home.recently_added.len(), 2);
        assert_eq!(
            home.hero.as_ref().map(|hero| hero.item_id.as_str()),
            Some("b")
        );
        assert_eq!(home.card_count(), 3);
    }

    #[test]
    fn offline_keeps_real_hero_and_cached_cards() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        db.upsert_items(
            &server.info.id,
            &[item(
                "a",
                "缓存里的片子",
                MediaKind::Movie,
                Duration::from_secs(0),
                Some(6000),
            )],
            1,
        )
        .expect("写条目");

        let home = home_view(
            &db,
            &server.info.id,
            &UserPreferences::default(),
            &Status::NoServer,
            Vec::new(),
            None,
        )
        .expect("构建首页");

        // 状态不再占用 hero 位：hero 恢复为真实内容，状态由顶部横幅表达。
        assert_eq!(
            home.hero.as_ref().map(|hero| hero.title.as_str()),
            Some("缓存里的片子")
        );
        assert_eq!(home.recently_added.len(), 1);
        let summary = summary(&Status::NoServer, &home);
        assert!(summary.contains("status: 未连接服务器"), "{summary}");
        assert!(summary.contains("recent[0]: 缓存里的片子"), "{summary}");
    }

    #[test]
    fn search_filters_local_cards_and_hero() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        db.upsert_items(
            &server.info.id,
            &[
                item(
                    "a",
                    "Dune",
                    MediaKind::Movie,
                    Duration::from_secs(0),
                    Some(6000),
                ),
                item(
                    "b",
                    "Arrival",
                    MediaKind::Movie,
                    Duration::from_secs(0),
                    Some(6000),
                ),
            ],
            1,
        )
        .expect("写条目");

        let home = home_view(
            &db,
            &server.info.id,
            &UserPreferences::default(),
            &Status::Connected {
                server: "测试服务器".to_owned(),
                base_url: "http://127.0.0.1:8096".to_owned(),
            },
            Vec::new(),
            Some("arr"),
        )
        .expect("构建首页");
        assert_eq!(home.recently_added.len(), 1);
        assert_eq!(home.recently_added[0].title, "Arrival");
        assert_eq!(
            home.hero.as_ref().map(|hero| hero.title.as_str()),
            Some("Arrival")
        );
    }

    #[test]
    fn detail_and_player_use_real_item_data() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        let movie = item(
            "a",
            "沙丘",
            MediaKind::Movie,
            Duration::from_secs(754),
            Some(8280),
        );
        db.upsert_items(&server.info.id, &[movie], 1)
            .expect("写条目");

        let detail = detail_view(&db, &server.info.id, "a")
            .expect("读缓存")
            .expect("条目存在");
        assert_eq!(detail.title, "沙丘");
        assert_eq!(detail.play_label, "从 12:34 继续");
        assert_eq!(detail.meta_line, "2024 · 2 小时 18 分 · 科幻");

        let player = player_view(&db, &server.info.id, "a")
            .expect("读缓存")
            .expect("条目存在");
        assert_eq!(player.title, "沙丘");
        assert_eq!(player.engine_line.as_deref(), Some(PLAYER_STATUS));
        assert_eq!(player.position, Duration::from_secs(754));
        assert_eq!(player.duration, Some(Duration::from_secs(8280)));
    }

    #[test]
    fn search_keeps_only_movies_and_series() {
        let items = vec![
            item(
                "mv",
                "沙丘",
                MediaKind::Movie,
                Duration::from_secs(0),
                Some(6000),
            ),
            item(
                "sr",
                "沙丘：预言",
                MediaKind::Series,
                Duration::from_secs(0),
                None,
            ),
            // 集 / 季是播放层级，不能出现在"找片"结果里。
            item(
                "ep",
                "沙丘 第 1 集",
                MediaKind::Episode,
                Duration::from_secs(0),
                None,
            ),
            item(
                "se",
                "沙丘 第 1 季",
                MediaKind::Season,
                Duration::from_secs(0),
                None,
            ),
        ];
        let view = search_view_from_items("沙丘", &items);
        assert_eq!(view.state, nova_ui::SearchState::Ready);
        assert_eq!(view.total, 2, "命中数只算电影 / 剧集");
        let ids: Vec<&str> = view
            .cards
            .iter()
            .map(|card| card.item_id.as_str())
            .collect();
        assert_eq!(ids, vec!["mv", "sr"]);
        // 卡片映射与首页/媒体库共用 `card_view`：副标题同样带年份/类型。
        assert_eq!(view.cards[0].subtitle.as_deref(), Some("2024 · 科幻"));
    }

    #[test]
    fn search_without_hits_is_empty_not_error() {
        let view = search_view_from_items("没有这部片", &[]);
        assert_eq!(view.state, nova_ui::SearchState::Empty);
        assert_eq!(view.total, 0);
        assert!(view.is_empty());
    }

    #[test]
    fn cached_search_matches_titles_and_skips_episodes() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        db.upsert_items(
            &server.info.id,
            &[
                item(
                    "a",
                    "Dune",
                    MediaKind::Movie,
                    Duration::from_secs(0),
                    Some(6000),
                ),
                item(
                    "b",
                    "Arrival",
                    MediaKind::Movie,
                    Duration::from_secs(0),
                    Some(6000),
                ),
                item(
                    "c",
                    "Dune Prophecy",
                    MediaKind::Series,
                    Duration::from_secs(0),
                    None,
                ),
                item(
                    "d",
                    "Dune 第 1 集",
                    MediaKind::Episode,
                    Duration::from_secs(0),
                    None,
                ),
            ],
            1,
        )
        .expect("写条目");

        let view = search_view(&db, &server.info.id, "dune", SEARCH_LIMIT).expect("本地搜索");
        assert_eq!(view.state, nova_ui::SearchState::Ready);
        let ids: Vec<&str> = view
            .cards
            .iter()
            .map(|card| card.item_id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "c"], "大小写不敏感，且不含集");

        // limit 生效。
        let limited = search_view(&db, &server.info.id, "dune", 1).expect("本地搜索");
        assert_eq!(limited.total, 1);

        // 没有命中 → Empty（不是 Error）。
        let none = search_view(&db, &server.info.id, "zzz", SEARCH_LIMIT).expect("本地搜索");
        assert_eq!(none.state, nova_ui::SearchState::Empty);
    }

    #[test]
    fn missing_item_is_not_an_error() {
        let db = Database::open_in_memory().expect("内存库");
        let id = ServerId::new("srv-x");
        assert!(detail_view(&db, &id, "nope").expect("读缓存").is_none());
        assert!(player_view(&db, &id, "nope").expect("读缓存").is_none());
    }
}
