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
use slint::Model as _;

fn image_or_empty(images: &ImageStore, key: &crate::viewmodel::ImageKey) -> slint::Image {
    images.get(key)
}

fn model<T: Clone + 'static>(items: Vec<T>) -> slint::ModelRc<T> {
    slint::ModelRc::new(slint::VecModel::from(items))
}

/// 把 `items` 写进 Slint 侧**已有**的 model，并返回可以继续挂在属性上的 model。
///
/// **不能每次新建 `VecModel`**：Slint 的重复元素按 model 指针判断有没有换数据源
/// （`ModelRc` 的 `PartialEq` 比的是内层 model 的地址），新建 model 会把整行
/// `PosterCard` 实例全部重建——实例上的 `TouchArea.has-hover` 随之清零，而它是
/// **边沿事件**，指针不动就不会再重报。首页每批海报到达都会重贴同一份视图，
/// 于是指针底下的放大效果会莫名其妙熄灭，`CardRow.hover-gen`（让仍有 `has-hover`
/// 的卡重新点亮）也永远没有机会生效。
///
/// 也不能用 `VecModel::set_vec`：它走 `ModelNotify::reset()`，实例同样全销毁。
/// 所以这里逐行写 + 尾部增删：`row_changed` 只让 Slint 原地 `update` 那一行的实例，
/// 实例身份、悬停状态、行内局部状态全都保留。
fn reuse_model<T: Clone + PartialEq + 'static>(
    existing: Option<&slint::ModelRc<T>>,
    items: Vec<T>,
) -> slint::ModelRc<T> {
    let Some(existing) = existing else {
        // 首次填充：没有可复用的 model。
        return model(items);
    };
    let Some(vec_model) = existing.as_any().downcast_ref::<slint::VecModel<T>>() else {
        // 不是我们建的 `VecModel`（外部塞进来的 / `ModelRc::default()`）：只能新建。
        return model(items);
    };
    // 多的从尾部删（保留下标稳定的那些实例），少的补在尾部。
    while vec_model.row_count() > items.len() {
        vec_model.remove(vec_model.row_count() - 1);
    }
    for (index, item) in items.into_iter().enumerate() {
        // 没变的行不写：每批海报只有少数几张卡的 `image` 变化，其余行连
        // `row_changed` 都不发，重贴一次的代价就只是比较一遍结构体。
        if vec_model.row_data(index).as_ref() == Some(&item) {
            continue;
        }
        if index < vec_model.row_count() {
            vec_model.set_row_data(index, item);
        } else {
            vec_model.push(item);
        }
    }
    existing.clone()
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
///
/// `previous` 是 Slint 侧当前挂着的同一份 `HomeData`：传进来就复用里面的 model
/// （见 [`reuse_model`]），传 `None` 则全部新建（首次填充）。
pub fn home_data(view: &HomeView, images: &ImageStore, previous: Option<&HomeData>) -> HomeData {
    let hero = view.hero.as_ref().map(|hero| CardData {
        image: hero_image(hero, images),
        ..card_data(hero, images)
    });
    let has_hero = hero.is_some();
    // 库行自己的 `cards` model 也要复用：`RowData` 是外层重复元素的数据，`cards`
    // 换成新 model 会让该行的卡片实例整体重建。按 `library_id` 对应上一轮的模型：
    // 库增删 / 重排时行号会跟着挪，按行号借会把**别的库**的实例状态（悬停、行内
    // 滚动）错接到这一行上。找不到同一个库就新建。
    let previous_rows: Vec<RowData> = previous
        .map(|previous| {
            (0..previous.library_rows.row_count())
                .filter_map(|index| previous.library_rows.row_data(index))
                .collect()
        })
        .unwrap_or_default();
    let library_rows: Vec<RowData> = view
        .library_rows
        .iter()
        .map(|row| {
            let previous_cards = previous_rows
                .iter()
                .find(|previous| previous.library_id.as_str() == row.library_id.as_str())
                .map(|previous| &previous.cards);
            RowData {
                library_id: row.library_id.clone().into(),
                title: row.title.clone().into(),
                cards: reuse_model(
                    previous_cards,
                    row.items
                        .iter()
                        .map(|card| card_data(card, images))
                        .collect(),
                ),
            }
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
        hero_playable: view.hero_playable(),
        continue_watching: reuse_model(
            previous.map(|previous| &previous.continue_watching),
            view.continue_watching
                .iter()
                .map(|card| card_data(card, images))
                .collect(),
        ),
        recently_added: reuse_model(
            previous.map(|previous| &previous.recently_added),
            view.recently_added
                .iter()
                .map(|card| card_data(card, images))
                .collect(),
        ),
        library_rows: reuse_model(
            previous.map(|previous| &previous.library_rows),
            library_rows,
        ),
        library_row_starts: reuse_model(
            previous.map(|previous| &previous.library_row_starts),
            library_row_starts,
        ),
        tabs: reuse_model(
            previous.map(|previous| &previous.tabs),
            view.library_tabs.iter().map(tab_data).collect(),
        ),
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
        playable: view.playable,
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
    use crate::viewmodel::{CardView, LibraryRowView, SearchView};

    /// `ModelRc` 里的条目（测试里读回来比对）。
    fn rows<T: Clone + 'static>(model: &slint::ModelRc<T>) -> Vec<T> {
        (0..model.row_count())
            .filter_map(|index| model.row_data(index))
            .collect()
    }

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

    // ---- `reuse_model`：model 实例身份必须保留（否则整行卡片实例重建）----

    /// 0 → n：空 model 也要原地补满，实例身份不变。
    #[test]
    fn reuse_model_fills_an_empty_model_in_place() {
        let previous = model(Vec::<i32>::new());
        let next = reuse_model(Some(&previous), vec![1, 2, 3]);
        assert_eq!(next, previous, "空 model 也要原地复用（实例身份不变）");
        assert_eq!(rows(&next), vec![1, 2, 3]);
    }

    /// n → 0：多余的行从尾部删掉，仍是同一个实例。
    #[test]
    fn reuse_model_shrinks_to_empty_in_place() {
        let previous = model(vec![1, 2, 3]);
        let next = reuse_model(Some(&previous), Vec::new());
        assert_eq!(next, previous, "清空也要原地复用");
        assert!(rows(&next).is_empty());
    }

    /// n → m：变短、变长都原地写，实例身份不变。
    #[test]
    fn reuse_model_rewrites_in_place_when_the_length_changes() {
        let previous = model(vec![1, 2, 3, 4, 5]);

        let shorter = reuse_model(Some(&previous), vec![1, 2, 3]);
        assert_eq!(shorter, previous, "变短也必须原地复用");
        assert_eq!(rows(&shorter), vec![1, 2, 3]);

        let longer = reuse_model(Some(&shorter), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(longer, previous, "变长也必须原地复用");
        assert_eq!(rows(&longer), vec![1, 2, 3, 4, 5, 6]);
    }

    /// 条数不变、内容整体替换：仍返回同一个 model（Slint 才会原地 `update` 实例）。
    #[test]
    fn reuse_model_keeps_the_instance_when_the_content_is_replaced() {
        let previous = model(vec![1, 2, 3]);
        let next = reuse_model(Some(&previous), vec![4, 5, 6]);
        assert_eq!(next, previous, "内容替换不代表换 model");
        assert_eq!(rows(&next), vec![4, 5, 6]);
    }

    /// 传进来的不是我们建的 `VecModel` 时回退新建，且不动原 model。
    #[test]
    fn reuse_model_falls_back_to_a_fresh_model_for_foreign_models() {
        // `ModelRc::default()`（里面没有 model）。
        let empty = slint::ModelRc::<i32>::default();
        let next = reuse_model(Some(&empty), vec![7, 8]);
        assert_ne!(next, empty, "无内层 model 时必须新建一个");
        assert_eq!(rows(&next), vec![7, 8]);
        assert!(rows(&empty).is_empty());

        // 自定义（非 `VecModel`）model：同样新建，原 model 保留自己的内容。
        struct Fixed(Vec<i32>);
        impl slint::Model for Fixed {
            type Data = i32;
            fn row_count(&self) -> usize {
                self.0.len()
            }
            fn row_data(&self, row: usize) -> Option<i32> {
                self.0.get(row).copied()
            }
            fn model_tracker(&self) -> &dyn slint::ModelTracker {
                &()
            }
        }
        let foreign = slint::ModelRc::new(Fixed(vec![1]));
        let next = reuse_model(Some(&foreign), vec![2, 3]);
        assert_ne!(next, foreign, "非 VecModel 时必须新建");
        assert_eq!(rows(&next), vec![2, 3]);
        assert_eq!(rows(&foreign), vec![1], "原 model 不能被写");
    }

    // ---- 库行的 `cards` model：按 `library_id` 复用 ----

    fn home_with_rows(library_ids: &[&str]) -> HomeView {
        HomeView {
            library_rows: library_ids
                .iter()
                .map(|library_id| LibraryRowView {
                    library_id: (*library_id).to_string(),
                    title: (*library_id).to_string(),
                    items: vec![CardView::new(format!("{library_id}-1"), "卡片")],
                })
                .collect(),
            ..HomeView::new()
        }
    }

    fn cards_of(data: &HomeData, library_id: &str) -> slint::ModelRc<CardData> {
        (0..data.library_rows.row_count())
            .filter_map(|index| data.library_rows.row_data(index))
            .find(|row| row.library_id.as_str() == library_id)
            .unwrap_or_else(|| panic!("没有库行 {library_id}"))
            .cards
    }

    /// 中间删掉一个库：后面那个库必须拿到**自己**原来的 `cards` model。
    ///
    /// 按行号复用会把被删库的 model 借给下一个库（实例状态串行）。
    #[test]
    fn library_rows_reuse_their_cards_model_by_library_id() {
        let images = ImageStore::new();
        let before = home_data(&home_with_rows(&["lib-a", "lib-b", "lib-c"]), &images, None);
        let c_before = cards_of(&before, "lib-c");
        let b_before = cards_of(&before, "lib-b");
        assert_ne!(c_before, b_before, "前提：两个库原本各用自己的 model");

        // 删掉中间的 lib-b：lib-c 从第 3 行变成第 2 行。
        let after = home_data(&home_with_rows(&["lib-a", "lib-c"]), &images, Some(&before));
        let c_after = cards_of(&after, "lib-c");
        assert_ne!(c_after, b_before, "lib-c 不能借到被删掉的 lib-b 的 model");
        assert_eq!(c_after, c_before, "lib-c 必须继续用自己原来的 model");
        // 顺序也按 `library_id` 对应：重排后仍各自复用。
        let reordered = home_data(&home_with_rows(&["lib-c", "lib-a"]), &images, Some(&after));
        assert_eq!(cards_of(&reordered, "lib-c"), c_before);
        assert_eq!(cards_of(&reordered, "lib-a"), cards_of(&after, "lib-a"));
    }
}
