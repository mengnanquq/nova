// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova-core` 媒体模型 → `nova-ui` 视图模型。
//!
//! 这里是"装配"的核心，全部是纯函数/同步函数：只读 [`Database`]，不发网络请求。
//! 抽出来的好处是离线启动、`--print-view`、`--snapshot`、窗口模式跑的是同一套
//! 视图构建逻辑，测试可以直接断言卡片数量与顺序。

use nova_core::{ItemId, MediaItem, MediaKind, MediaSource, ServerId, UserPreferences};
use nova_playback::{resume_label, resume_position_for_item};
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
pub const DEFAULT_SORT_LABEL: &str = "排序：最近添加";

/// 媒体库排序方式：点「排序」按钮循环切换（顺序就是这个枚举的声明顺序）。
///
/// 排序只作用于**本地缓存**里的 `MediaItem` 列表，不改 provider 的查询参数
/// （那会引入网络依赖，排序按钮在离线时也必须立刻生效）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibrarySort {
    /// 最近添加：保持数据库顺序（服务端按加入时间倒序返回）。
    #[default]
    RecentlyAdded,
    /// 标题 A→Z。
    TitleAsc,
    /// 标题 Z→A。
    TitleDesc,
    /// 发行年份从新到旧（缺年份的排在最后）。
    YearDesc,
}

impl LibrarySort {
    /// 排序条上显示的文案（`LibraryView::sort_label`）。
    pub const fn label(self) -> &'static str {
        match self {
            Self::RecentlyAdded => DEFAULT_SORT_LABEL,
            Self::TitleAsc => "排序：标题 A→Z",
            Self::TitleDesc => "排序：标题 Z→A",
            Self::YearDesc => "排序：发行年份从新到旧",
        }
    }

    /// 下一次点击的排序方式（`最近添加 → 标题 A→Z → 标题 Z→A → 年份从新到旧 → …`）。
    pub const fn next(self) -> Self {
        match self {
            Self::RecentlyAdded => Self::TitleAsc,
            Self::TitleAsc => Self::TitleDesc,
            Self::TitleDesc => Self::YearDesc,
            Self::YearDesc => Self::RecentlyAdded,
        }
    }

    /// 就地排序；缺少排序键的条目也必须有**稳定**顺序。
    ///
    /// 每种排序的最后一级比较都是条目 id：标题相同、年份缺失（按 0 处理）时
    /// 顺序仍然确定，不会因为 `sort` 的不稳定性导致每次点击结果不同。
    pub fn apply(self, items: &mut [MediaItem]) {
        match self {
            // 查询已经按 `updated_at` 倒序取回（`Database::children_recent`），
            // 这里保持原样，不要再按标题洗一遍。
            Self::RecentlyAdded => {}
            Self::TitleAsc => items.sort_by(|left, right| {
                title_key(left)
                    .cmp(&title_key(right))
                    .then_with(|| left.id.as_str().cmp(right.id.as_str()))
            }),
            Self::TitleDesc => items.sort_by(|left, right| {
                title_key(right)
                    .cmp(&title_key(left))
                    .then_with(|| left.id.as_str().cmp(right.id.as_str()))
            }),
            Self::YearDesc => items.sort_by(|left, right| {
                let left_year = left.year.unwrap_or(0);
                let right_year = right.year.unwrap_or(0);
                right_year.cmp(&left_year).then_with(|| {
                    title_key(left)
                        .cmp(&title_key(right))
                        .then_with(|| left.id.as_str().cmp(right.id.as_str()))
                })
            }),
        }
    }
}

/// 标题排序键：优先 `sort_title`（服务端给的排序标题），缺失时退回显示标题。
fn title_key(item: &MediaItem) -> String {
    let key = item.sort_key().trim();
    if key.is_empty() {
        item.title.trim().to_lowercase()
    } else {
        key.to_lowercase()
    }
}

/// 用本地缓存构建首页。
///
/// 每个库行最多展示的条目数（Netflix 首页每行约 20 张卡）。
const LIBRARY_ROW_LIMIT: usize = 20;

/// 「最近添加」一次最多取多少条（跨库；够几屏横向滚动）。
///
/// 以前这一行取的是"`parent_id` 为 NULL 的条目"，列表刷新把库归属写进
/// 电影 / 剧集后它就永远是空的了（只有列表里漏掉父项的季 / 集会落进去，
/// 标题还是"第 N 集"）。现在按**类型**挑顶层条目并跨库排序。
const HOME_RECENT_LIMIT: usize = 200;

/// 顺序：
/// 1. “继续观看”来自 [`Database::continue_watching`]（SQL 侧过滤）；
/// 2. “最近添加”来自所有库的电影 / 剧集（按 `updated_at` 倒序，见 [`HOME_RECENT_LIMIT`]）；
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
    // 「最近添加」= 所有库的电影 / 剧集，按加入时间倒序（不限 `parent_id`，
    // 理由见 `HOME_RECENT_LIMIT`）。
    let recent = db.browse_recent(
        server_id,
        &[MediaKind::Movie, MediaKind::Series],
        HOME_RECENT_LIMIT,
    )?;

    view.continue_watching = in_progress.iter().map(card_view).collect();
    view.recently_added = recent.iter().map(card_view).collect();
    // 只为**真正的媒体库**建行：「首页」是导航哨兵（`local::HOME_TAB_ID`），用它查子项
    // 永远是空的。空行会渲染成一整行空白（`visible: false` 的行在纵向布局里照样占位），
    // 用户滑到底会看到一大块空白和它后面的空标题。
    view.library_rows = view
        .library_tabs
        .iter()
        .filter(|tab| !tab.id.is_empty() && tab.id != crate::local::HOME_TAB_ID)
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
                    library_id: tab.id.clone(),
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
    // 同一条理由：缓存里没有该库的条目、或搜索把所有卡都过滤掉了，这一行就不给界面
    // （否则渲染成一整行空白）。
    view.library_rows.retain(|row| !row.items.is_empty());
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
    sort: LibrarySort,
) -> Result<LibraryView, nova_storage::StorageError> {
    let parent = if tab.id.is_empty() {
        None
    } else {
        Some(ItemId::new(tab.id.clone()))
    };
    // 「最近添加」要真的按加入时间排（`updated_at`），不能用 `children` 的顺序
    // ——那是 `index_number, sort_title`，电影没有集号时就退化成字母序。
    // 合成的「全部」标签（空 id，老缓存 / 媒体库列表还没持久化时才有）
    // 按类型跨库列电影 / 剧集：库归属写对之后 `parent_id IS NULL` 的顶层行几乎不存在。
    let mut items = if tab.id.is_empty() {
        db.browse_recent(server_id, &SEARCH_KINDS, usize::MAX)?
    } else if sort == LibrarySort::RecentlyAdded {
        db.children_recent(server_id, parent.as_ref())?
    } else {
        db.children(server_id, parent.as_ref())?
    };
    let mut sort_label = sort.label().to_owned();
    if items.is_empty() && parent.is_some() {
        let top_level = db.children(server_id, None)?;
        if !top_level.is_empty() {
            items = top_level;
            // 退化列表 + 默认排序保持原来的文案（快照/文档都与它对齐）；
            // 用户切换排序后显示新排序，退化说明让位给排序状态。
            if sort == LibrarySort::RecentlyAdded {
                sort_label = FALLBACK_SORT_LABEL.to_owned();
            }
        }
    }
    sort.apply(&mut items);

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

/// 搜索页只收这两种类型（`is_search_kind` 的口径）：
/// 集（Episode）/ 季（Season）是播放层级，只应出现在详情页的选集里；
/// 混进搜索结果会让"找片"变成"找某一集"，所以这里明确过滤掉。
///
/// 定义成常量是因为本地搜索要把同一份类型下推到 `Database::search_items`
/// （在 SQL 游标上先过滤类型、再计 `limit`，季 / 集不会占掉名额）。
const SEARCH_KINDS: [MediaKind; 2] = [MediaKind::Movie, MediaKind::Series];

/// 是否属于「找片」页收录的类型。
pub fn is_search_kind(kind: MediaKind) -> bool {
    SEARCH_KINDS.contains(&kind)
}

/// 搜索页收录的类型（本地计数 / 搜索共用同一份）。
pub fn search_kinds() -> &'static [MediaKind] {
    &SEARCH_KINDS
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
///
/// 标题匹配在 SQL 侧做（[`Database::search_items`]）：列表刷新把电影 / 剧集的
/// 父项写成所属媒体库后，按"顶层行"（`parent_id IS NULL`）取条目几乎什么也
/// 取不到，本地搜索会整页落空。
pub fn search_view(
    db: &Database,
    server_id: &ServerId,
    term: &str,
    limit: usize,
) -> Result<SearchView, nova_storage::StorageError> {
    // 单次遍历同时拿到前 `limit` 条与精确总数：以前命中数 ≥ limit 时还要再跑
    // 一次 `search_item_count`，宽泛词（“的”/“大”/“a”）在真实库上要多扫一遍全表。
    let (items, total) = db.search_items_with_count(server_id, &SEARCH_KINDS, term, limit)?;
    // 结果头必须能说出真实总数，而不是本页卡片数：否则会显示“共 60 部”，
    // 用户不知道后面还有（R6-10）。没被截断时 `total == items.len()`。
    Ok(search_view_from_items(term, &items).with_total(total))
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
    let prefs = db.preferences()?;
    let mut view = detail_from_item(&item, &sources, &prefs);
    view.in_my_list = db.in_mylist(server_id, &id)?;
    Ok(Some(view))
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
    let prefs = db.preferences()?;
    Ok(Some(player_from_item(&item, &prefs)))
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
pub fn detail_from_item(
    item: &MediaItem,
    sources: &[MediaSource],
    prefs: &UserPreferences,
) -> DetailView {
    let mut view = DetailView::new(item.id.as_str(), item.title.clone());
    view.original_title = item.original_title.clone();
    view.meta_line = detail_meta_line(item.year, item.runtime, &item.genres);
    view.overview = item.overview.clone();
    // 文案与起播位置**必须**走同一套判断（`resume_position_for_item`）：
    // 以前这里直接用原始 `resume_position`，`Played = true` 的片中段续播点会
    // 显示成「从 45:47 继续」而实际从头播（体检 R7-D2）。
    view.play_label = resume_label(resume_position_for_item(item, prefs));
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
///
/// 初始位置与详情页的播放文案同源（[`resume_position_for_item`]）：以前这里直接
/// 用原始 `resume_position`，已看完 / 太靠前的续播点会显示成“从 45:47 继续”，
/// 而真正起播是 0（体检 R7-D2）。
pub fn player_from_item(item: &MediaItem, prefs: &UserPreferences) -> PlayerShellView {
    let mut view = PlayerShellView::new(item.title.clone())
        .with_position(resume_position_for_item(item, prefs))
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

    fn movie_with(id: &str, title: &str, sort_title: &str, year: Option<u16>) -> MediaItem {
        let mut item = item(id, title, MediaKind::Movie, Duration::ZERO, Some(7200));
        item.sort_title = sort_title.to_owned();
        item.year = year;
        item
    }

    #[test]
    fn sort_cycles_through_the_documented_order() {
        use LibrarySort::{RecentlyAdded, TitleAsc, TitleDesc, YearDesc};
        // 顺序写进文档与 `docs/ui-architecture.md`：循环一次回到原点。
        assert_eq!(RecentlyAdded.next(), TitleAsc);
        assert_eq!(TitleAsc.next(), TitleDesc);
        assert_eq!(TitleDesc.next(), YearDesc);
        assert_eq!(YearDesc.next(), RecentlyAdded);
        // 每种排序都有自己的文案（排序条要能看出当前状态）。
        for sort in [RecentlyAdded, TitleAsc, TitleDesc, YearDesc] {
            assert!(!sort.label().is_empty());
        }
        assert_eq!(LibrarySort::default(), RecentlyAdded);
    }

    #[test]
    fn title_sort_is_case_insensitive_and_stable_without_sort_keys() {
        let mut items = vec![
            // 缺标题、缺排序标题的条目：键为空串，必须靠 id 稳定排序（不能随机）。
            movie_with("b", "", "", None),
            movie_with("a", "", "", None),
            movie_with("c", "Beta", "beta", Some(2020)),
            movie_with("d", "alpha", "", Some(2021)),
        ];
        LibrarySort::TitleAsc.apply(&mut items);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "d", "c"]);

        LibrarySort::TitleDesc.apply(&mut items);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["c", "d", "a", "b"]);

        // 再排一次结果不变（幂等 + 稳定）。
        let before: Vec<String> = items.iter().map(|item| item.id.to_string()).collect();
        LibrarySort::TitleDesc.apply(&mut items);
        let after: Vec<String> = items.iter().map(|item| item.id.to_string()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn year_sort_puts_missing_years_last_and_ties_break_by_title() {
        let mut items = vec![
            movie_with("old", "Old", "old", Some(1999)),
            movie_with("none", "NoYear", "noyear", None),
            movie_with("new", "New", "new", Some(2023)),
            movie_with("new-b", "Another", "another", Some(2023)),
        ];
        LibrarySort::YearDesc.apply(&mut items);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        // 2023 两条按标题（another < new），然后 1999，最后是没有年份的。
        assert_eq!(ids, ["new-b", "new", "old", "none"]);
    }

    /// 体检 R4-3 的前半：同一排序下，结果只能取决于**条目集合**，不能取决于
    /// 输入顺序（SQL 返回顺序、刷新前后）。平局（同标题 / 缺年份）由 id 决出。
    #[test]
    fn sort_is_a_pure_function_of_the_item_set() {
        let base = vec![
            movie_with("b", "", "", None),
            movie_with("a", "", "", None),
            movie_with("c", "Same", "same", Some(2020)),
            movie_with("d", "Same", "same", Some(2020)),
            movie_with("e", "alpha", "", None),
            movie_with("f", "Beta", "beta", Some(1999)),
        ];
        for sort in [
            LibrarySort::TitleAsc,
            LibrarySort::TitleDesc,
            LibrarySort::YearDesc,
        ] {
            let ids = |items: &[MediaItem]| -> Vec<String> {
                let mut items = items.to_vec();
                sort.apply(&mut items);
                items.into_iter().map(|item| item.id.to_string()).collect()
            };
            let forward = ids(&base);
            let mut reversed = base.clone();
            reversed.reverse();
            assert_eq!(ids(&reversed), forward, "{sort:?} 不能依赖输入顺序");
            let mut rotated = base.clone();
            rotated.rotate_left(2);
            assert_eq!(ids(&rotated), forward, "{sort:?} 不能依赖输入顺序");
        }
    }

    /// 列表刷新把库归属写进电影 / 剧集后，「最近添加」不能再靠
    /// `parent_id IS NULL` 挑条目——那样首页这一行会永远是空的。
    #[test]
    fn recently_added_spans_libraries_by_kind_not_by_missing_parent() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        let mut older = movie_with("a", "旧片", "旧片", Some(1999));
        older.parent_id = Some(ItemId::new("lib-a"));
        let mut newer = movie_with("b", "新片", "新片", Some(2024));
        newer.parent_id = Some(ItemId::new("lib-b"));
        // 季 / 集是播放层级，不进「最近添加」。
        let mut season = item("s", "第 1 季", MediaKind::Season, Duration::ZERO, None);
        season.parent_id = Some(ItemId::new("series"));
        db.upsert_items(&server.info.id, &[older], 10)
            .expect("写旧片");
        db.upsert_items(&server.info.id, &[newer], 20)
            .expect("写新片");
        db.upsert_items(&server.info.id, &[season], 30)
            .expect("写季");

        let home = home_view(
            &db,
            &server.info.id,
            &UserPreferences::default(),
            &Status::NoServer,
            vec![LibraryTab::new("lib-a", "A 库")],
            None,
        )
        .expect("构建首页");

        let ids: Vec<&str> = home
            .recently_added
            .iter()
            .map(|card| card.item_id.as_str())
            .collect();
        assert_eq!(ids, ["b", "a"], "跨库按加入时间倒序，且不含季");
    }

    /// 体检 R7-1：首页的库行只能是**真的有内容的媒体库**。
    ///
    /// 回归背景：`home_view` 以前把「首页」导航哨兵（`local::HOME_TAB_ID`）也当库建行，
    /// 它下面没有条目 → 空行。`.slint` 的纵向布局不会剔除 `visible: false` 的行，
    /// 空行会渲染成一整行空白（R7-1：真机滑到底能看到一大块空白与空标题）。
    #[test]
    fn home_rows_skip_the_home_tab_and_empty_libraries() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        let mut movie = movie_with("a", "沙丘", "沙丘", Some(2024));
        movie.parent_id = Some(ItemId::new("lib-a"));
        db.upsert_items(&server.info.id, &[movie], 1)
            .expect("写条目");

        let home = home_view(
            &db,
            &server.info.id,
            &UserPreferences::default(),
            &Status::NoServer,
            vec![
                LibraryTab::new(crate::local::HOME_TAB_ID, "首页"),
                LibraryTab::new("lib-a", "A 库"),
                LibraryTab::new("lib-b", "B 库（缓存里还没有条目）"),
            ],
            None,
        )
        .expect("构建首页");

        let titles: Vec<&str> = home
            .library_rows
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(titles, ["A 库"], "「首页」哨兵与空库都不能变成库行");
        assert!(
            home.library_rows.iter().all(|row| !row.items.is_empty()),
            "库行不能空着进界面（会渲染成一整行空白）"
        );
        // 焦点行：最近添加 1 张 + A 库 1 张。
        assert_eq!(home.focus_rows(), vec![1, 1]);
    }

    #[test]
    fn recently_added_keeps_the_database_order() {
        let mut items = vec![
            movie_with("z", "Zulu", "zulu", Some(2001)),
            movie_with("a", "Alpha", "alpha", Some(2020)),
        ];
        LibrarySort::RecentlyAdded.apply(&mut items);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["z", "a"]);
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

    /// 播放页初始位置与详情页文案必须读同一份偏好（都是 `db.preferences()`）：
    /// 以前详情读 DB、播放页直接用原始 `resume_position`，改高
    /// `resume_min_position` 后会出现“详情写播放、播放页却停在 12:34”。
    #[test]
    fn player_position_uses_the_same_preferences_as_the_detail_label() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-prefs");
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

        // 运行中改高续播阈值：754s 不再算有效进度。
        let prefs = UserPreferences {
            resume_min_position: Duration::from_secs(800),
            ..UserPreferences::default()
        };
        db.set_preferences(&prefs).expect("写偏好");

        let detail = detail_view(&db, &server.info.id, "a")
            .expect("读详情")
            .expect("条目存在");
        assert_eq!(detail.play_label, "播放");
        let player = player_view(&db, &server.info.id, "a")
            .expect("读播放页")
            .expect("条目存在");
        assert_eq!(
            player.position,
            Duration::ZERO,
            "播放页初始位置必须与详情文案同源（同一份数据库偏好）"
        );
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

    /// 本地搜索结果被 `limit` 截断时，`total` 仍是真实命中数（R6-10）。
    ///
    /// 修复前：`search_view_from_items` 的 `total` = 本页卡片数，界面会把
    /// “共 60 部”当成真实总数，用户不知道后面还有。
    #[test]
    fn cached_search_total_is_not_truncated_by_the_limit() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        let movies: Vec<MediaItem> = (1..=7)
            .map(|index| {
                movie_with(
                    &format!("m{index}"),
                    &format!("沙丘 {index}"),
                    &format!("沙丘 {index}"),
                    Some(2021),
                )
            })
            .collect();
        db.upsert_items(&server.info.id, &movies, 1)
            .expect("写条目");

        let limited = search_view(&db, &server.info.id, "沙丘", 2).expect("本地搜索");
        assert_eq!(limited.cards.len(), 2, "本页只给 limit 条");
        assert_eq!(limited.total, 7, "总数不能被 limit 截成 2");

        // 没触发截断时总数就是卡片数（不多跑一次计数）。
        let full = search_view(&db, &server.info.id, "沙丘", SEARCH_LIMIT).expect("本地搜索");
        assert_eq!(full.cards.len(), 7);
        assert_eq!(full.total, 7);
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

        // limit 生效；`total` 仍是真实命中数（2 条命中，不是本页的 1 条）。
        let limited = search_view(&db, &server.info.id, "dune", 1).expect("本地搜索");
        assert_eq!(limited.cards.len(), 1);
        assert_eq!(limited.total, 2);

        // 没有命中 → Empty（不是 Error）。
        let none = search_view(&db, &server.info.id, "zzz", SEARCH_LIMIT).expect("本地搜索");
        assert_eq!(none.state, nova_ui::SearchState::Empty);
    }

    /// 列表刷新把库归属写进电影 / 剧集之后（`parent_id` = 库 id），本地搜索
    /// 再按“顶层行”（`parent_id IS NULL`）取条目就基本一条都搜不到。
    ///
    /// 修复前：`children(server, None)` 只返回顶层行，这个用例的条目全都挂在库 /
    /// 剧集下，`search_view` 命中 0 条。修复后按类型跨库做 SQL 标题匹配。
    #[test]
    fn cached_search_finds_library_owned_movies_and_series() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        let mut movie = movie_with("m1", "沙丘", "沙丘", Some(2021));
        movie.parent_id = Some(ItemId::new("lib-movies"));
        let mut series = item("s1", "沙丘：预言", MediaKind::Series, Duration::ZERO, None);
        series.parent_id = Some(ItemId::new("lib-series"));
        let mut season = item(
            "se1",
            "沙丘 第 1 季",
            MediaKind::Season,
            Duration::ZERO,
            None,
        );
        season.parent_id = Some(ItemId::new("s1"));
        let mut episode = item(
            "e1",
            "沙丘 第 1 集",
            MediaKind::Episode,
            Duration::ZERO,
            None,
        );
        episode.parent_id = Some(ItemId::new("se1"));
        db.upsert_items(&server.info.id, &[movie, series, season, episode], 1)
            .expect("写条目");

        let view = search_view(&db, &server.info.id, "沙丘", SEARCH_LIMIT).expect("本地搜索");
        assert_eq!(view.state, nova_ui::SearchState::Ready);
        let ids: Vec<&str> = view
            .cards
            .iter()
            .map(|card| card.item_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["m1", "s1"],
            "库归属的电影 / 剧集都要搜到，季 / 集不要"
        );

        // limit 在 SQL / 迭代上生效（不是先全取再截断）；`total` 仍是真实命中数。
        let limited = search_view(&db, &server.info.id, "沙丘", 1).expect("本地搜索");
        assert_eq!(limited.cards.len(), 1);
        assert_eq!(limited.total, 2);
        assert_eq!(limited.cards[0].item_id, "m1");

        let none = search_view(&db, &server.info.id, "不存在的片", SEARCH_LIMIT).expect("本地搜索");
        assert_eq!(none.state, nova_ui::SearchState::Empty);
    }

    /// `%` / `_` 是 SQL `LIKE` 的通配符，必须按字面量匹配（转义）：
    /// 搜 `%` 不能把所有电影都匹配出来，搜 `_` 不能匹配任意单字符。
    #[test]
    fn cached_search_treats_like_wildcards_as_literals() {
        let db = Database::open_in_memory().expect("内存库");
        let server = server("srv-1");
        db.upsert_server(&server.info, 1).expect("写服务器");
        let with = |id: &str, title: &str, parent: &str| {
            let mut item = movie_with(id, title, title, Some(2020));
            item.parent_id = Some(ItemId::new(parent));
            item
        };
        db.upsert_items(
            &server.info.id,
            &[
                with("pct", "100% 纯爱", "lib-a"),
                with("under", "A_B 计划", "lib-a"),
                with("plain", "普通片", "lib-a"),
            ],
            1,
        )
        .expect("写条目");

        let percent = search_view(&db, &server.info.id, "%", SEARCH_LIMIT).expect("本地搜索");
        let ids: Vec<&str> = percent
            .cards
            .iter()
            .map(|card| card.item_id.as_str())
            .collect();
        assert_eq!(ids, vec!["pct"], "`%` 只能匹配标题里真的含 % 的条目");

        let underscore = search_view(&db, &server.info.id, "_", SEARCH_LIMIT).expect("本地搜索");
        let ids: Vec<&str> = underscore
            .cards
            .iter()
            .map(|card| card.item_id.as_str())
            .collect();
        assert_eq!(ids, vec!["under"], "`_` 不能匹配任意单字符");
    }

    #[test]
    fn missing_item_is_not_an_error() {
        let db = Database::open_in_memory().expect("内存库");
        let id = ServerId::new("srv-x");
        assert!(detail_view(&db, &id, "nope").expect("读缓存").is_none());
        assert!(player_view(&db, &id, "nope").expect("读缓存").is_none());
    }
}
