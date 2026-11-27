// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 视图模型 → Slint 属性结构。只做字段搬运与格式化，不做业务判断。

use crate::pages::assets::ImageStore;
use crate::viewmodel::{
    CardView, DetailView, HomeView, LibraryTab, LibraryView, PlayerView, SearchView, SourceOption,
    TrackItem,
};
use crate::{
    CardData, DetailData, HomeData, LibraryData, PlayerData, RowData, SearchData, SourceData,
    TabData, TrackData,
};

fn image_or_empty(images: &ImageStore, key: &crate::viewmodel::ImageKey) -> slint::Image {
    images.get(key)
}

fn model<T: Clone + 'static>(items: Vec<T>) -> slint::ModelRc<T> {
    slint::ModelRc::new(slint::VecModel::from(items))
}

fn hero_image(card: &CardView, images: &ImageStore) -> slint::Image {
    let backdrop = images.get(&crate::viewmodel::ImageKey::backdrop(card.item_id.clone()));
    if backdrop.size().width > 0 {
        backdrop
    } else {
        image_or_empty(images, &card.poster_key())
    }
}

/// 单张卡片的 Slint 数据。
pub fn card_data(card: &CardView, images: &ImageStore) -> CardData {
    CardData {
        id: card.item_id.clone().into(),
        title: card.title.clone().into(),
        subtitle: card.subtitle.clone().unwrap_or_default().into(),
        overview: card.overview.clone().unwrap_or_default().into(),
        badge: card.badge.clone().unwrap_or_default().into(),
        progress: card.progress.unwrap_or(-1.0),
        progress_text: card.progress_percent().into(),
        initial: card.initial().into(),
        image: image_or_empty(images, &card.poster_key()),
    }
}

fn source_data(source: &SourceOption) -> SourceData {
    SourceData {
        id: source.source_id.clone().into(),
        title: source.title.clone().into(),
        subtitle: source.subtitle.clone().unwrap_or_default().into(),
        selected: source.selected,
    }
}

fn tab_data(tab: &LibraryTab) -> TabData {
    TabData {
        id: tab.id.clone().into(),
        title: tab.title.clone().into(),
    }
}

/// 首页数据。
pub fn home_data(view: &HomeView, images: &ImageStore) -> HomeData {
    let hero = view.hero.as_ref().map(|hero| CardData {
        image: hero_image(hero, images),
        ..card_data(hero, images)
    });
    let has_hero = hero.is_some();
    let library_rows: Vec<RowData> = view
        .library_rows
        .iter()
        .map(|row| RowData {
            library_id: row.library_id.clone().into(),
            title: row.title.clone().into(),
            cards: model(
                row.items
                    .iter()
                    .map(|card| card_data(card, images))
                    .collect(),
            ),
        })
        .collect();
    // 库行在扁平焦点索引里的起点（`.slint` 用它把页面级焦点换算成行内下标）。
    // 与 `library_rows` 同一个来源，长度永远一致。
    let library_row_starts: Vec<i32> = view
        .library_row_starts()
        .into_iter()
        .map(|start| start as i32)
        .collect();
    HomeData {
        hero: hero.unwrap_or_default(),
        has_hero,
        continue_watching: model(
            view.continue_watching
                .iter()
                .map(|card| card_data(card, images))
                .collect(),
        ),
        recently_added: model(
            view.recently_added
                .iter()
                .map(|card| card_data(card, images))
                .collect(),
        ),
        library_rows: model(library_rows),
        library_row_starts: model(library_row_starts),
        tabs: model(view.library_tabs.iter().map(tab_data).collect()),
        // 首页顶栏常驻高亮第一个标签（「首页」）；搜索页不属于任何库，
        // `search_data` 的 `current_tab` 固定传 `-1`（无高亮）。
        current_tab: 0,
        empty: view.is_empty(),
    }
}

/// 媒体库数据。
pub fn library_data(view: &LibraryView, images: &ImageStore) -> LibraryData {
    LibraryData {
        id: view.id.clone().into(),
        title: view.title.clone().into(),
        count_text: format!("{} 部", view.cards.len()).into(),
        sort_label: view.sort_label.clone().into(),
        tabs: model(view.library_tabs.iter().map(tab_data).collect()),
        current_tab: view.current_tab as i32,
        cards: model(
            view.cards
                .iter()
                .map(|card| card_data(card, images))
                .collect(),
        ),
        empty: view.cards.is_empty(),
    }
}

/// 搜索页数据。
///
/// 结果头的两行文案在 Rust 侧拼好（`「沙丘」的搜索结果` / `共 5 部`），
/// 与媒体库页的 `title + count-text` 同构。
///
/// `current_tab` 是顶栏高亮下标：**搜索页不属于任何库**，调用方传 `-1`
/// （`Navbar` 用 `i == current-index` 判断选中，`-1` 永不命中 → 无高亮）。
///
/// 命中数（`view.total`）可能大于本页卡片数（搜索上限 60）：这时必须把
/// 「显示前 N 部」写出来，否则「共 60 部」会让用户以为真的只有 60 条。
pub fn search_data(
    view: &SearchView,
    tabs: &[LibraryTab],
    current_tab: i32,
    images: &ImageStore,
) -> SearchData {
    // 没有查询词时不显示引号（空搜索框的 Idle 态）。
    let title = if view.term.is_empty() {
        "搜索结果".to_string()
    } else {
        format!("「{}」的搜索结果", view.term)
    };
    SearchData {
        tabs: model(tabs.iter().map(tab_data).collect()),
        current_tab,
        term: view.term.clone().into(),
        title: title.into(),
        count_text: search_count_text(view.total, view.cards.len()).into(),
        cards: model(
            view.cards
                .iter()
                .map(|card| card_data(card, images))
                .collect(),
        ),
        state: view.state.index(),
        error: view.state.error_message().unwrap_or_default().into(),
    }
}

/// 搜索结果头的命中数文案。
///
/// 命中数大于本页卡片数（搜索上限 60）时补一句「显示前 N 部」，
/// 否则用户看到「共 60 部」会以为真的只有 60 条（真机 R6-10）。
fn search_count_text(total: usize, shown: usize) -> String {
    if total > shown {
        format!("共 {total} 部 · 显示前 {shown} 部")
    } else {
        format!("共 {total} 部")
    }
}

/// 详情页数据。
pub fn detail_data(view: &DetailView, images: &ImageStore) -> DetailData {
    DetailData {
        item_id: view.item_id.clone().into(),
        in_my_list: view.in_my_list,
        title: view.title.clone().into(),
        original_title: view.original_title.clone().unwrap_or_default().into(),
        meta_line: view.meta_line.clone().into(),
        overview: view.overview.clone().unwrap_or_default().into(),
        genres: model::<slint::SharedString>(
            view.genres
                .iter()
                .map(|genre| genre.as_str().into())
                .collect(),
        ),
        play_label: view.play_label.clone().into(),
        badge: view.badge.clone().unwrap_or_default().into(),
        initial: CardView::new(view.item_id.clone(), view.title.clone())
            .initial()
            .into(),
        poster: image_or_empty(images, &view.poster_key()),
        backdrop: image_or_empty(images, &view.backdrop_key()),
        sources: model(view.sources.iter().map(source_data).collect()),
        selected_source_label: view
            .sources
            .iter()
            .find(|source| source.selected)
            .map(|source| source.title.clone())
            .unwrap_or_default()
            .into(),
        genres_line: view
            .genres
            .iter()
            .map(|genre| genre.as_str())
            .collect::<Vec<&str>>()
            .join(" · ")
            .into(),
    }
}

fn track_data(track: &TrackItem) -> TrackData {
    TrackData {
        index: track.index as i32,
        label: track.label.clone().into(),
        detail: track.detail.clone().unwrap_or_default().into(),
        selected: track.selected,
        bitmap: track.is_bitmap_subtitle,
    }
}

/// 播放页数据。
///
/// 进度 / 缓冲比例在这里就压成 `0.0..=1.0`：视图模型对"时长未知"返回 `None`，
/// 界面拿到的是 `progress = 0` 且 `has-progress = false`，不会有 `NaN` 传到布局里。
///
/// `item_id` 用来取背景图（引擎首帧到达前铺底，避免黑底上只有控件）。
pub fn player_data(view: &PlayerView, images: &ImageStore, item_id: &str) -> PlayerData {
    let progress = view.progress_fraction();
    let buffered = view.buffered_fraction();
    let status_line = view.status_line();
    let frame = image_or_empty(
        images,
        &crate::viewmodel::ImageKey::backdrop(item_id.to_owned()),
    );
    PlayerData {
        item_id: "".into(),
        title: view.title.clone().into(),
        subtitle: view.subtitle.clone().unwrap_or_default().into(),
        time_line: view.time_line().into(),
        remaining_text: view.remaining_text().unwrap_or_default().into(),
        time_text: view.time_text().into(),
        progress: progress.unwrap_or(0.0),
        buffered: buffered.unwrap_or(0.0),
        has_progress: progress.is_some(),
        playing: view.playing,
        buffering: view.buffering,
        volume: view.volume_fraction(),
        volume_text: view.volume_text().into(),
        volume_hint: view.volume_hint_visible(),
        muted: view.muted,
        rate_text: view.rate_text().into(),
        subtitle_delay_text: view.subtitle_delay_text().into(),
        audio_tracks: model(view.audio_tracks.iter().map(track_data).collect()),
        subtitle_tracks: model(view.subtitle_tracks.iter().map(track_data).collect()),
        subtitles_off: view.selected_subtitle_track().is_none(),
        audio_menu_open: view.audio_menu_open,
        subtitle_menu_open: view.subtitle_menu_open,
        error: view.error.clone().unwrap_or_default().into(),
        has_error: view.error.is_some(),
        error_action: view.error_action.clone().unwrap_or_default().into(),
        status_line: status_line.unwrap_or_default().into(),
        has_status_line: status_line.is_some(),
        status_warning: view.hwdec_notice.is_some(),
        transport_mode: view.transport_mode().unwrap_or_default().into(),
        engine_line: view.engine_line.clone().unwrap_or_default().into(),
        show_engine_detail: view.show_engine_detail,
        hwdec_notice: view.hwdec_notice.clone().unwrap_or_default().into(),
        controls_visible: view.controls_visible,
        has_frame: frame.size().width > 0,
        frame,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewmodel::{CardView, SearchView};

    /// 搜索页命中数被上限截断时，文案必须说出「显示前 N 部」（R6-10）。
    #[test]
    fn search_count_text_mentions_the_visible_prefix_when_truncated() {
        assert_eq!(search_count_text(5, 5), "共 5 部");
        assert_eq!(search_count_text(0, 0), "共 0 部");
        assert_eq!(search_count_text(348, 60), "共 348 部 · 显示前 60 部");
    }

    /// 搜索页顶栏不选中任何库标签：数据层收到 `-1` 就原样写进 `current-tab`。
    #[test]
    fn search_data_passes_the_no_highlight_sentinel() {
        let view = SearchView::from_cards(
            "沙丘",
            vec![
                CardView::new("s01", "沙丘"),
                CardView::new("s02", "沙丘：第二部"),
            ],
        );
        let images = ImageStore::new();
        let data = search_data(&view, &[], -1, &images);
        assert_eq!(data.current_tab, -1, "−1 = 顶栏无高亮");
        assert_eq!(data.count_text.as_str(), "共 2 部");
    }
}
