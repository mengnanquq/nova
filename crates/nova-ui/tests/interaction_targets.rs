// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 可交互目标清单（`Target`）的回归测试。
//!
//! 背景：媒体库的「排序：最近添加 ▼」曾经**点击没反应**——回调注册了，实现是空的，
//! 而当时没有任何测试能发现它。这里把"界面能点哪些东西"变成可断言的数据：
//!
//! 1. 扫描 `ui/**/*.slint` 里所有 `TouchArea` 声明，逐个要求**登记**——
//!    要么登记成某个 `Target`（本文件用 `Coverage::Targets` / `AnyPrefix` 指向它），
//!    要么在 `NoTarget` 里写明"为什么它不是可枚举控件"。
//!    新增一个 `TouchArea` 而忘了登记 → `touch_areas_are_classified` 直接红灯。
//! 2. 每个页面场景（首页 / 空首页 / 媒体库 / 搜索 / 详情 / 播放 / 对话框 / 横幅）
//!    断言目标清单：非空、id 唯一、`kind` 在允许集合、矩形非零且中心在窗口内。
//! 3. 表格里登记的目标必须真的出现在对应页面的清单里（id 稳定、坐标来自布局）。
//!
//! 断言用的是 `App::targets()`（声明式数组 + 登记总线合并后的结果），
//! 因为重复元素（卡片 / 标签 / 菜单行）与条件子树（hero / 空态 / 对话框）
//! 没法写进父组件的 `targets` 数组（见 `docs/ui-architecture.md` 的说明）。

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{
    detail_view, home_empty, home_filled, home_image_ids, init_backend, install_backdrops,
    install_posters, library_view, player_audio_menu, player_error, player_subtitle_menu,
    player_view, search_empty, search_view,
};
use nova_ui::{
    App, AppWindow, BannerData, BannerLevel, CardView, HomeView, LibraryRowView, Page,
    TARGET_KINDS, TargetInfo,
};
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};

const WINDOW_W: f32 = 1280.0;
const WINDOW_H: f32 = 800.0;

/// 一个 `TouchArea` 的登记方式。
#[derive(Clone, Copy, Debug)]
enum Coverage {
    /// 这些目标 id 必须出现在清单里。
    Targets(&'static [&'static str]),
    /// 清单里至少有一个 id 以其中之一开头（重复元素的 id 带下标）。
    AnyPrefix(&'static [&'static str]),
    /// 不需要 `Target`，理由写在这里（不允许空理由）。
    NoTarget(&'static str),
}

/// 一条 `TouchArea` → 目标 / 理由的登记表。
struct Case {
    file: &'static str,
    component: &'static str,
    /// 具名元素写元素 id；匿名 `TouchArea {}` 写 `<anonymous#序号>`（序号按组件内出现顺序）。
    entry: &'static str,
    /// 这些页面场景里要断言它（`NoTarget` 用空数组）。
    pages: &'static [&'static str],
    coverage: Coverage,
}

/// 所有 `TouchArea` 的登记表。新增控件必须在这里留一行。
const TOUCH_AREAS: &[Case] = &[
    // ---- 空态 ----
    Case {
        file: "empty_state.slint",
        component: "EmptyState",
        entry: "primary-area",
        pages: &["home-empty", "library-empty", "search-error"],
        coverage: Coverage::AnyPrefix(&[
            "home.empty.primary",
            "library.empty.primary",
            "search.empty.primary",
        ]),
    },
    Case {
        file: "empty_state.slint",
        component: "EmptyState",
        entry: "secondary-area",
        pages: &["home-empty"],
        coverage: Coverage::Targets(&["home.empty.secondary"]),
    },
    // ---- 横幅 ----
    Case {
        file: "components/banner.slint",
        component: "Banner",
        entry: "<anonymous#0>",
        pages: &["banner"],
        coverage: Coverage::Targets(&["banner.action"]),
    },
    // ---- 添加服务器对话框 ----
    Case {
        file: "components/add_server.slint",
        component: "AddServerDialog",
        entry: "<anonymous#0>",
        pages: &[],
        // 遮罩占满整窗，中心正好落在面板上；点它不会关闭对话框。
        // 取消有显式的「取消」按钮（`dialog.cancel`），不需要再枚举遮罩。
        coverage: Coverage::NoTarget("半透明遮罩：中心落在面板上，取消走显式的「取消」按钮"),
    },
    Case {
        file: "components/add_server.slint",
        component: "AddServerDialog",
        entry: "<anonymous#1>",
        pages: &[],
        // 面板自己吞掉指针事件，避免透给遮罩——它没有任何行为。
        coverage: Coverage::NoTarget("面板自身的事件拦截层：没有点击行为"),
    },
    Case {
        file: "components/add_server.slint",
        component: "AddServerDialog",
        entry: "cancel-area",
        pages: &["dialog"],
        coverage: Coverage::Targets(&["dialog.cancel"]),
    },
    Case {
        file: "components/add_server.slint",
        component: "AddServerDialog",
        entry: "connect-area",
        pages: &["dialog"],
        coverage: Coverage::Targets(&["dialog.connect"]),
    },
    // ---- 顶栏 ----
    Case {
        file: "navbar.slint",
        component: "Navbar",
        entry: "tab-area",
        pages: &["home", "library", "search"],
        coverage: Coverage::AnyPrefix(&["home.nav.tab.", "library.nav.tab.", "search.nav.tab."]),
    },
    Case {
        file: "navbar.slint",
        component: "Navbar",
        entry: "clear-area",
        // 「×」只在搜索框展开且有文字时可见（顶栏三个搜索入口互斥），
        // 所以单独一个"聚焦 + 有文字"的场景断言它。
        pages: &["nav-typing"],
        coverage: Coverage::Targets(&["home.nav.search-clear"]),
    },
    Case {
        file: "navbar.slint",
        component: "Navbar",
        entry: "search-area",
        pages: &["home", "library"],
        coverage: Coverage::AnyPrefix(&["home.nav.search", "library.nav.search"]),
    },
    // ---- 媒体库 ----
    Case {
        file: "library.slint",
        component: "LibraryPage",
        entry: "sort-area",
        pages: &["library"],
        coverage: Coverage::Targets(&["library.sort"]),
    },
    // ---- 首页 ----
    Case {
        file: "home.slint",
        component: "CardRow",
        entry: "more-area",
        pages: &["home"],
        coverage: Coverage::AnyPrefix(&["home.row."]),
    },
    Case {
        file: "home.slint",
        component: "CardRow",
        entry: "arrow-left-area",
        // 行在横向起点时左箭头是不可见的（体检 R8-C3），所以这个场景必须先把行滚开。
        pages: &["home-scrolled"],
        coverage: Coverage::AnyPrefix(&[
            "home.row.continue.left",
            "home.row.recent.left",
            "home.row.lib.0.left",
        ]),
    },
    Case {
        file: "home.slint",
        component: "CardRow",
        entry: "arrow-right-area",
        pages: &["home-hover"],
        coverage: Coverage::AnyPrefix(&[
            "home.row.continue.right",
            "home.row.recent.right",
            "home.row.lib.0.right",
        ]),
    },
    Case {
        file: "home.slint",
        component: "HomePage",
        entry: "hero-play-area",
        pages: &["home"],
        coverage: Coverage::Targets(&["home.hero.play"]),
    },
    Case {
        file: "home.slint",
        component: "HomePage",
        entry: "hero-info-area",
        pages: &["home"],
        coverage: Coverage::Targets(&["home.hero.info"]),
    },
    // ---- 卡片（首页行 / 媒体库网格 / 搜索网格共用）----
    Case {
        file: "poster_card.slint",
        component: "PosterCard",
        entry: "card-area",
        pages: &["home", "library", "search"],
        coverage: Coverage::AnyPrefix(&[
            "home.row.continue.card.",
            "home.row.recent.card.",
            "library.card.",
            "search.card.",
        ]),
    },
    // ---- 搜索页 ----
    Case {
        file: "search.slint",
        component: "SearchPage",
        entry: "clear-area",
        pages: &["search"],
        coverage: Coverage::Targets(&["search.clear"]),
    },
    // ---- 详情页 ----
    Case {
        file: "detail.slint",
        component: "DetailPage",
        entry: "play-area",
        pages: &["detail"],
        coverage: Coverage::Targets(&["detail.play"]),
    },
    Case {
        file: "detail.slint",
        component: "DetailPage",
        entry: "list-area",
        pages: &["detail"],
        coverage: Coverage::Targets(&["detail.my-list"]),
    },
    Case {
        file: "detail.slint",
        component: "DetailPage",
        entry: "drop-area",
        pages: &["detail"],
        coverage: Coverage::Targets(&["detail.drop"]),
    },
    Case {
        file: "detail.slint",
        component: "DetailPage",
        entry: "source-row",
        pages: &["detail-sources"],
        coverage: Coverage::AnyPrefix(&["detail.source."]),
    },
    Case {
        file: "detail.slint",
        component: "DetailPage",
        entry: "back-area",
        pages: &["detail"],
        coverage: Coverage::Targets(&["detail.back"]),
    },
    // ---- 播放页 ----
    // 音量控件：一个 TouchArea 覆盖图标 + 滑杆，但**登记两个目标**——
    // `player.mute`（喇叭 = 静音开关，恒存在）与 `player.volume`（展开后的滑杆，
    // 收起时宽度 0 被清单过滤）。覆盖按图标（`player.mute`）断言，
    // 滑杆见 `volume_slider_target_appears_only_while_expanded`。
    Case {
        file: "player_overlay.slint",
        component: "VolumeControl",
        entry: "area",
        pages: &["player"],
        coverage: Coverage::Targets(&["player.mute"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "TransportButton",
        entry: "area",
        pages: &["player"],
        coverage: Coverage::Targets(&[
            "player.seek-back",
            "player.seek-forward",
            "player.subtitles",
            "player.audio",
            "player.rate",
            "player.fullscreen",
        ]),
    },
    Case {
        file: "player_overlay.slint",
        component: "PlayButton",
        entry: "area",
        pages: &["player"],
        coverage: Coverage::Targets(&["player.play-pause"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "TrackRow",
        entry: "area",
        pages: &["player-audio-menu", "player-subtitle-menu"],
        coverage: Coverage::AnyPrefix(&["player.menu.audio.row.", "player.menu.subtitle.row."]),
    },
    Case {
        file: "player_overlay.slint",
        component: "OffRow",
        entry: "area",
        pages: &["player-subtitle-menu"],
        coverage: Coverage::Targets(&["player.menu.subtitle.off"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "ActionRow",
        entry: "action-area",
        pages: &["player-subtitle-menu"],
        // 外挂字幕那一行已下架（缺文件选择依赖），所以只剩两条延迟行。
        coverage: Coverage::Targets(&[
            "player.menu.subtitle.delay-down",
            "player.menu.subtitle.delay-up",
        ]),
    },
    // 播放页中央大播放图标：点它 = 播放/暂停（`player.center-play`）。
    //
    // 它只在**暂停**时出现（`show-center-play = !playing`），而 `player_view()` 夹具
    // 是播放中，所以不在这个场景里断言；由 `center_play_icon_is_clickable_when_paused`
    // 单独覆盖"暂停时它真的出现在清单里"。
    Case {
        file: "player_overlay.slint",
        component: "PlayerOverlay",
        entry: "center-play-area",
        pages: &[],
        coverage: Coverage::Targets(&["player.center-play"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "PlayerProgress",
        entry: "bar-area",
        pages: &["player"],
        coverage: Coverage::Targets(&["player.progress"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "PlayerOverlay",
        entry: "<anonymous#0>",
        pages: &[],
        // 整屏输入层：任何点击/移动都只是"唤醒控制层"，没有独立的控件语义；
        // sweep 点其它目标时也会穿过它，单独列一条只会得到重复的结论。
        coverage: Coverage::NoTarget("整屏输入层：点击只唤醒控制层，不是独立控件"),
    },
    Case {
        file: "player_overlay.slint",
        component: "PlayerOverlay",
        entry: "back-area",
        pages: &["player"],
        coverage: Coverage::Targets(&["player.back"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "PlayerOverlay",
        entry: "retry-area",
        pages: &["player-error"],
        coverage: Coverage::Targets(&["player.retry"]),
    },
    Case {
        file: "player_overlay.slint",
        component: "PlayerOverlay",
        entry: "time-area",
        pages: &["player"],
        coverage: Coverage::Targets(&["player.time"]),
    },
];

/// 扫描界面里的 `TouchArea`：`(文件, 所在组件, 元素 id 或 <anonymous#n>)`。
fn scan_touch_areas() -> BTreeSet<(String, String, String)> {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let mut files = Vec::new();
    collect_slint_files(&ui, &mut files);
    let mut found = BTreeSet::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = path
            .strip_prefix(&ui)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut component = String::from("<none>");
        let mut anonymous = 0usize;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some(name) = component_name(trimmed) {
                component = name;
                anonymous = 0;
                continue;
            }
            if let Some(id) = touch_area_id(trimmed) {
                let entry = match id {
                    Some(id) => id,
                    None => {
                        let index = anonymous;
                        anonymous += 1;
                        format!("<anonymous#{index}>")
                    }
                };
                found.insert((file.clone(), component.clone(), entry));
            }
        }
    }
    found
}

/// 从 `component Foo inherits ...` / `export component Foo inherits ...` 里取组件名。
fn component_name(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let mut word = words.next()?;
    if word == "export" {
        word = words.next()?;
    }
    if word != "component" {
        return None;
    }
    let name = words.next()?;
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        .then(|| name.to_owned())
}

/// `name := TouchArea {` → `Some(Some(name))`；匿名 `TouchArea {` / `TouchArea { }` → `Some(None)`。
fn touch_area_id(line: &str) -> Option<Option<String>> {
    let rest = if let Some((name, rest)) = line.split_once(":=") {
        let name = name.trim();
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || name.is_empty()
        {
            return None;
        }
        Some((name.to_owned(), rest.trim_start().to_owned()))
    } else {
        None
    };
    match rest {
        Some((name, after)) => after.starts_with("TouchArea").then_some(Some(name)),
        None => line.starts_with("TouchArea").then_some(None),
    }
}

fn collect_slint_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_slint_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "slint") {
            out.push(path);
        }
    }
}

/// 新窗口（尺寸必须在 `show()` 之前定好，testing 后端不会为重设尺寸重排）。
fn window() -> App {
    window_sized(WINDOW_W, WINDOW_H)
}

fn window_sized(width: f32, height: f32) -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().expect("创建窗口"));
    app.ui().set_window_w(width);
    app.ui().set_window_h(height);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    app
}

/// 推进时间 + 让目标重新登记，然后读清单。
///
/// 两轮登记是必需的：第一轮让**已存在**的元素重新上报；第一轮之后才被创建的元素
/// （例如刚展开的菜单行、刚切页后的卡片）`target-gen` 的初值就是新一轮的值，
/// 不会触发 `changed`，必须再推进一次才会被收进清单。
fn settle(app: &App, millis: u64) -> Vec<TargetInfo> {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(millis));
    app.targets()
}

fn assert_invariants(scenario: &str, targets: &[TargetInfo]) {
    assert_invariants_in(scenario, targets, WINDOW_W, WINDOW_H);
}

fn assert_invariants_in(scenario: &str, targets: &[TargetInfo], width: f32, height: f32) {
    assert!(!targets.is_empty(), "{scenario}: 可交互目标清单是空的");
    let mut seen = BTreeSet::new();
    for target in targets {
        assert!(
            seen.insert(target.id.clone()),
            "{scenario}: target id 重复：{}",
            target.id
        );
        assert!(
            TARGET_KINDS.contains(&target.kind.as_str()),
            "{scenario}: {} 的 kind `{}` 不在允许集合 {:?}",
            target.id,
            target.kind,
            TARGET_KINDS
        );
        assert!(
            target.width > 0.0 && target.height > 0.0,
            "{scenario}: {} 的矩形是零面积（{}×{}）",
            target.id,
            target.width,
            target.height
        );
        assert!(
            target.center_in_window(width, height),
            "{scenario}: {} 的中心 ({}, {}) 不在窗口内",
            target.id,
            target.center().0,
            target.center().1
        );
    }
}

/// 表格里登记这一场景的 `TouchArea` 必须真的产出了对应目标。
fn assert_coverage(scenario: &str, targets: &[TargetInfo]) {
    for case in TOUCH_AREAS
        .iter()
        .filter(|case| case.pages.contains(&scenario))
    {
        match case.coverage {
            Coverage::Targets(ids) => {
                for id in ids {
                    assert!(
                        targets.iter().any(|target| target.id == *id),
                        "{scenario}: {}:{} 的 {id} 不在清单里（清单：{:?}）",
                        case.file,
                        case.entry,
                        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
                    );
                }
            }
            Coverage::AnyPrefix(prefixes) => {
                assert!(
                    targets
                        .iter()
                        .any(|target| prefixes.iter().any(|prefix| target.id.starts_with(prefix))),
                    "{scenario}: {}:{} 没有对应的目标（前缀 {prefixes:?}，清单：{:?}）",
                    case.file,
                    case.entry,
                    targets.iter().map(|t| &t.id).collect::<Vec<_>>()
                );
            }
            Coverage::NoTarget(reason) => {
                assert!(
                    !reason.trim().is_empty(),
                    "{}:{} 的 NoTarget 必须写明理由",
                    case.file,
                    case.entry
                );
            }
        }
    }
}

#[test]
fn touch_areas_are_classified() {
    let scanned = scan_touch_areas();
    let registered: BTreeSet<(String, String, String)> = TOUCH_AREAS
        .iter()
        .map(|case| {
            (
                case.file.to_owned(),
                case.component.to_owned(),
                case.entry.to_owned(),
            )
        })
        .collect();

    let missing: Vec<_> = scanned.difference(&registered).collect();
    assert!(
        missing.is_empty(),
        "这些 TouchArea 没有登记目标或理由（见 tests/interaction_targets.rs）：{missing:?}"
    );
    let stale: Vec<_> = registered.difference(&scanned).collect();
    assert!(
        stale.is_empty(),
        "登记表里有已经不存在的 TouchArea（改名/删除后要同步登记表）：{stale:?}"
    );
}

#[test]
fn home_targets_cover_hero_rows_and_navbar() {
    // 首页内容比一屏高：给一个高窗口，让三条行（继续观看 / 最近添加 / 各库行）
    // 都在视口里。真实窗口靠滚动，滚动后的坐标由 `interaction_sweep` 与真机验证覆盖。
    let app = window_sized(WINDOW_W, 1600.0);
    let mut home = home_filled();
    // hero 的播放/更多信息按钮只在有简介时实例化（见 home.slint 的注释）。
    if let Some(hero) = home.hero.clone() {
        home.hero = Some(hero.with_overview("保罗·厄崔迪走上复仇之路。"));
    }
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.show_home();
    // 压缩 billboard 让行区进入首屏（真实窗口靠滚动）。
    app.ui().set_billboard_ratio(0.2);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants_in("home", &targets, WINDOW_W, 1600.0);
    assert_coverage("home", &targets);

    let ids: BTreeSet<&str> = targets.iter().map(|target| target.id.as_str()).collect();
    for id in [
        "home.hero.play",
        "home.hero.info",
        "home.row.continue.card.0",
        "home.row.recent.card.0",
        "home.row.lib.0.card.0",
        "home.nav.tab.0",
    ] {
        assert!(ids.contains(id), "首页缺少目标 {id}（清单：{ids:?}）");
    }
    // 库标签的 id 带下标：4 个标签都在。
    for index in 0..4 {
        let id = format!("home.nav.tab.{index}");
        assert!(ids.contains(id.as_str()), "首页顶栏缺少 {id}");
    }
}

#[test]
fn hovering_a_card_reveals_the_row_arrows() {
    let app = window();
    let mut home = home_filled();
    if let Some(hero) = home.hero.clone() {
        home.hero = Some(hero.with_overview("保罗·厄崔迪走上复仇之路。"));
    }
    install_posters(&app, &home_image_ids(&home));
    app.set_home(&home);
    app.show_home();
    app.ui().set_billboard_ratio(0.2);
    app.ui().set_nav_active(true);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    let card = targets
        .iter()
        .find(|target| target.id == "home.row.continue.card.1")
        .expect("继续观看第 2 张卡");
    let (x, y) = card.center();
    let position = slint::LogicalPosition::new(x, y);
    app.ui()
        .window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let targets = settle(&app, 80);
    assert_invariants("home-hover", &targets);
    assert_coverage("home-hover", &targets);
    // 体检 R8-C3：行在起点时不显示左箭头，也不登记为可交互目标（否则 sweep 点它是空操作）。
    assert!(
        !targets
            .iter()
            .any(|target| target.id == "home.row.continue.left"),
        "行在起点时左箭头不该出现（清单：{:?}）",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );
    assert!(
        targets
            .iter()
            .any(|target| target.id == "home.row.continue.right"),
        "悬停后应出现右箭头目标"
    );

    // 键盘把这一行滚开一格：左箭头这才出现（覆盖 `arrow-left-area`）。
    for _ in 0..3 {
        let right: slint::SharedString = Key::RightArrow.into();
        app.press_key(right).expect("按右键");
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    }
    let targets = settle(&app, 80);
    assert!(
        app.ui().get_home_row_scroll_x() < 0.0,
        "前提：行已经横向滚开"
    );
    assert!(
        targets
            .iter()
            .any(|target| target.id == "home.row.continue.left"),
        "滚动之后左箭头必须出现并进清单（清单：{:?}）",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );
    assert_invariants("home-scrolled", &targets);
    assert_coverage("home-scrolled", &targets);
}

/// 纯悬停（`nav-active = false`）也要显示行箭头。
///
/// 首屏没按过方向键时 `nav-active` 还是 false，键盘焦点不点亮任何卡；但鼠标悬停
/// 仍然应该给出可滚动的入口（行被点亮 = `zoom-index >= 0`，悬停也走这条路）。
/// 这里锁住“箭头不对 `nav-active` 敏感”这条规则，防止以后把显隐条件越收越紧。
#[test]
fn hovering_a_card_reveals_the_row_arrows_without_keyboard_nav() {
    let app = window();
    app.set_home(&home_filled());
    app.show_home();
    app.ui().set_billboard_ratio(0.2);
    app.ui().set_nav_active(false);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    // 没悬停时行没被点亮：箭头不在清单里。
    let targets = settle(&app, 80);
    assert!(
        !targets
            .iter()
            .any(|target| target.id.starts_with("home.row.continue.right")),
        "未悬停且未启用键盘导航时不该显示箭头（清单：{:?}）",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );

    // 纯悬停一张卡（不按方向键、不进键盘层）。
    let card = app
        .target("home.row.continue.card.1")
        .expect("继续观看第 2 张卡");
    let (x, y) = card.center();
    app.ui().window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(x, y),
    });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

    let targets = settle(&app, 80);
    assert!(!app.ui().get_nav_active(), "前提：键盘导航仍未启用");
    assert!(
        targets
            .iter()
            .any(|target| target.id == "home.row.continue.right"),
        "悬停也要点亮行并给出右箭头（清单：{:?}）",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );
    assert!(
        !targets
            .iter()
            .any(|target| target.id == "home.row.continue.left"),
        "行在起点时左箭头仍不该出现"
    );
}

#[test]
fn navbar_search_targets_switch_with_focus_and_text() {
    let app = window();
    app.set_home(&home_filled());
    app.show_home();
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    // 收起态：只有放大镜入口。
    settle(&app, 80);
    assert!(app.target("home.nav.search").is_some());
    assert!(app.target("home.nav.search-input").is_none());
    assert!(app.target("home.nav.search-clear").is_none());

    // 点放大镜 → 输入框出现（放大镜自己不可点：`enabled: !search-focused`）。
    app.click_target("home.nav.search").expect("点搜索图标");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    let targets = settle(&app, 80);
    assert_invariants("nav-typing", &targets);
    assert!(app.target("home.nav.search-input").is_some());
    assert!(
        app.target("home.nav.search-clear").is_none(),
        "没有文字时不该有「×」"
    );
    assert!(
        app.target("home.nav.search").is_none(),
        "展开后放大镜不可点"
    );

    // 有文字 → 「×」出现。
    app.ui().set_search_text("沙".into());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    let targets = settle(&app, 80);
    assert_invariants("nav-typing", &targets);
    assert_coverage("nav-typing", &targets);
    assert!(app.target("home.nav.search-clear").is_some());
}

#[test]
fn empty_home_targets_cover_the_empty_state() {
    let app = window();
    app.set_home(&home_empty());
    app.show_home();
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    let targets = settle(&app, 80);
    assert_invariants("home-empty", &targets);
    assert_coverage("home-empty", &targets);
    assert!(
        app.target("home.empty.primary").is_some(),
        "缺少「连接服务器」"
    );
    // 空首页没有 hero：hero 的目标不应出现（条件子树没实例化）。
    assert!(app.target("home.hero.play").is_none());
}

#[test]
fn library_targets_cover_sort_and_cards() {
    let app = window();
    let library = library_view();
    install_posters(&app, &["l01", "l02", "l03"]);
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants("library", &targets);
    assert_coverage("library", &targets);
    let sort = app.target("library.sort").expect("排序按钮必须是目标");
    assert_eq!(sort.kind, "button");
    assert!(
        sort.label.contains("排序"),
        "排序目标的 label 是 {}",
        sort.label
    );
    assert!(
        targets.iter().any(|target| target.id == "library.card.0"),
        "媒体库网格卡片没有登记"
    );
}

#[test]
fn empty_library_targets_cover_the_empty_state() {
    let app = window();
    let mut library = library_view();
    library.cards.clear();
    app.set_library(&library);
    app.open_library(&library);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));

    let targets = settle(&app, 80);
    assert_invariants("library-empty", &targets);
    assert_coverage("library-empty", &targets);
    // 空库时排序条藏起来（点了也没意义），但空态的按钮要在。
    assert!(app.target("library.sort").is_none());
    assert!(app.target("library.empty.primary").is_some());
}

#[test]
fn search_targets_cover_input_and_results() {
    let app = window();
    // 搜索页顶栏的标签数据借自首页（见 `SearchData::tabs`）。
    app.set_home(&home_filled());
    app.open_search(&search_view());
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants("search", &targets);
    assert_coverage("search", &targets);
    for id in ["search.input", "search.clear", "search.card.0"] {
        assert!(app.target(id).is_some(), "搜索页缺少目标 {id}");
    }
}

#[test]
fn empty_search_targets_cover_the_empty_state() {
    let app = window();
    app.set_home(&home_filled());
    app.open_search(&search_empty());
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants("search-empty", &targets);
    assert_coverage("search-empty", &targets);
    // 「没有找到相关内容」空态没有动作按钮（primary/secondary 都是空串）→ 不登记。
    assert!(app.target("search.empty.primary").is_none());
    assert!(app.target("search.input").is_some());
}

#[test]
fn search_error_targets_cover_the_retry_button() {
    let app = window();
    app.set_home(&home_filled());
    app.open_search(&nova_ui::SearchView::error("沙丘", "搜索失败，请稍后重试"));
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    let targets = settle(&app, 80);
    assert_invariants("search-error", &targets);
    assert_coverage("search-error", &targets);
    let retry = app
        .target("search.empty.primary")
        .expect("重试按钮必须是目标");
    assert_eq!(retry.label, "重试");
}

#[test]
fn detail_targets_cover_buttons_and_source_rows() {
    let app = window();
    install_backdrops(&app, &["dune"]);
    app.set_detail(&detail_view());
    app.show_page(Page::Detail);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants("detail", &targets);
    assert_coverage("detail", &targets);

    // 版本行只有展开时才有目标：点一下「播放版本」。
    let drop = app.target("detail.drop").expect("版本下拉必须是目标");
    let (x, y) = drop.center();
    let position = slint::LogicalPosition::new(x, y);
    for event in [
        WindowEvent::PointerMoved { position },
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    ] {
        app.ui().window().dispatch_event(event);
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));

    let targets = settle(&app, 80);
    assert_invariants("detail-sources", &targets);
    assert_coverage("detail-sources", &targets);
    for index in 0..3 {
        let id = format!("detail.source.{index}");
        assert!(app.target(&id).is_some(), "展开后缺少 {id}");
    }
}

/// R9-A2：电影在片源为空（离线 / 直链还没取到）时，空的「播放版本」入口和
/// 展开行都不该出现，但播放按钮保留（能不能播按条目类型判断，不按片源判断）。
#[test]
fn detail_without_sources_hides_the_version_drop_but_keeps_play() {
    let app = window();
    install_backdrops(&app, &["dune"]);
    let mut view = detail_view();
    view.sources.clear();
    app.set_detail(&view);
    app.show_page(Page::Detail);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants("detail-no-sources", &targets);
    assert!(
        app.target("detail.play").is_some(),
        "片源为空的电影仍要保留播放按钮"
    );
    assert!(
        app.target("detail.drop").is_none(),
        "片源为空时不该有「播放版本」入口"
    );
    assert!(
        app.target("detail.source.0").is_none(),
        "片源为空时不该有版本行"
    );
}

/// R9-A2（真机实测）：剧集没有片源（片源挂在单集上）、选集未做，剧集详情页
/// 播放按钮整个不实例化，也不登记为目标；焦点顶到「我的列表」上。
#[test]
fn series_detail_has_no_play_or_drop_target_but_keeps_my_list() {
    let app = window();
    install_backdrops(&app, &["dune"]);
    let mut view = detail_view();
    view.playable = false;
    view.sources.clear();
    app.set_detail(&view);
    app.show_page(Page::Detail);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));

    let targets = settle(&app, 80);
    assert_invariants("detail-series", &targets);
    assert!(
        app.target("detail.play").is_none(),
        "剧集详情不该有播放按钮"
    );
    assert!(
        app.target("detail.drop").is_none(),
        "剧集没有片源，不该有「播放版本」入口"
    );
    assert!(app.target("detail.my-list").is_some(), "「我的列表」要保留");
}

/// 暂停时中央大播放图标必须出现在清单里、且能被点到（它此前没有 TouchArea，
/// 点击会穿透到整屏命中层，只唤醒控制层——典型的"死按钮"）。
#[test]
fn center_play_icon_is_clickable_when_paused() {
    let app = window();
    app.set_player(&player_view().with_playing(false));
    app.show_page(Page::Player);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    let target = app
        .target("player.center-play")
        .unwrap_or_else(|| panic!("暂停时必须有中央播放图标目标，实际：{targets:?}"));
    assert_invariants("player-paused", &targets);
    assert!(target.width > 0.0 && target.height > 0.0);

    // 矩形必须是**图标本身**，不是整个窗口：`player_overlay.slint` 曾把坐标写成
    // `root.absolute-position/width/height`，登记出来是 1280×800、原点 0,0，
    // `targets` / 递增扫描都看不出它其实是一枚中央图标（体检就会报“没有中央图标”）。
    let (center_x, center_y) = target.center();
    assert!(
        (target.width - target.height).abs() < 0.5,
        "中央图标应是正方形：{}×{}",
        target.width,
        target.height
    );
    assert!(
        target.width < WINDOW_W / 2.0 && target.height < WINDOW_H / 2.0,
        "中央图标的目标矩形不能被登记成整个窗口：{}×{}",
        target.width,
        target.height
    );
    assert!(
        (center_x - WINDOW_W / 2.0).abs() < 2.0 && (center_y - WINDOW_H / 2.0).abs() < 2.0,
        "中央图标应在窗口正中，实际中心 ({center_x}, {center_y})"
    );

    // 点它必须切到"播放"状态（暂停 → 播放）。
    app.click_target("player.center-play").expect("点中央图标");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(app.player_view().playing, "点中央图标后应开始播放");
}

/// 点喇叭图标 = 静音 / 取消静音（不是“把音量降一档”），且静音态在目标清单里看得见。
///
/// 体检报告的“点喇叭 0.20→0.15→0.10”在 HEAD 上无法复现（另外核对了真实 mpv：
/// 静音走的是 `mute` 属性，音量值本身不变）；这里把正确行为固化成回归测试，
/// 同时保证调试通道能读到静音态。
#[test]
fn volume_icon_toggles_mute_without_touching_volume() {
    let app = window();
    app.set_player(&player_view().with_volume(0.4));
    app.show_page(Page::Player);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    settle(&app, 80);
    assert!(!app.player_view().muted);

    app.click_target("player.mute")
        .expect("喇叭图标必须是可点目标");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(app.player_view().muted, "点喇叭图标应切到静音");
    assert!(
        (app.player_view().volume - 0.4).abs() < f32::EPSILON,
        "静音不能改动音量值：{}",
        app.player_view().volume
    );

    let targets = settle(&app, 80);
    let target = app.target("player.mute").expect("静音后仍有喇叭图标目标");
    assert_eq!(
        target.label, "取消静音",
        "静音态必须在目标清单里可见：{targets:?}"
    );

    app.click_target("player.mute")
        .expect("再点一次应能取消静音");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(!app.player_view().muted, "再点喇叭图标应取消静音");
    assert!((app.player_view().volume - 0.4).abs() < f32::EPSILON);
}

/// 滑杆目标只在控件展开（hover）后存在：收起时它宽度为 0，清单里点不到。
///
/// 这一条把两种动作分开钉死：图标（`player.mute`）= 静音开关；展开的滑杆
/// （`player.volume`）= 直接把音量设到点击位置。不分开的话，悬停后目标矩形
/// 会突然变宽、中心点落到滑杆上，坐标点击就会“点喇叭却改了音量”。
#[test]
fn volume_slider_target_appears_only_while_expanded() {
    let app = window();
    app.set_player(&player_view().with_volume(0.4));
    app.show_page(Page::Player);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    let mute = app
        .target("player.mute")
        .unwrap_or_else(|| panic!("收起时应有喇叭图标目标：{targets:?}"));
    assert!(
        app.target("player.volume").is_none(),
        "收起时滑杆不可点，不应出现在清单里：{targets:?}"
    );

    // 悬停到图标上：控件向右展开，滑杆变成可点目标。
    let (x, y) = mute.center();
    let position = slint::LogicalPosition::new(x, y);
    app.ui()
        .window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    let targets = settle(&app, 80);
    let slider = app
        .target("player.volume")
        .unwrap_or_else(|| panic!("展开后应出现滑杆目标（清单：{:?}）", targets));
    assert!(slider.width > 0.0, "展开后的滑杆必须有面积");
    assert!(
        slider.x >= mute.x + mute.width - 0.5,
        "滑杆目标不能把喇叭图标包进去：slider.x={} mute.x={} width={}",
        slider.x,
        mute.x,
        mute.width
    );

    // 点滑杆 = 设到百分比（不是切静音）。
    app.click_target("player.volume").expect("点展开的滑杆");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(!app.player_view().muted, "点滑杆不应切静音");
    let volume = app.player_view().volume;
    assert!(
        volume > 0.2 && volume < 0.8,
        "点滑杆中部应把音量设到中间偏上，实际 {volume}"
    );
}

#[test]
fn player_targets_cover_transport_and_menus() {
    let app = window();
    app.set_player(&player_view());
    app.show_page(Page::Player);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));

    let targets = settle(&app, 80);
    assert_invariants("player", &targets);
    assert_coverage("player", &targets);

    // 音轨菜单打开后的菜单行。
    app.set_player(&player_audio_menu());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    assert_invariants("player-audio-menu", &targets);
    assert_coverage("player-audio-menu", &targets);
    assert!(app.target("player.menu.audio.row.1").is_some());

    // 字幕菜单（含「关闭」与两条延迟动作行；外挂字幕已下架）。
    app.set_player(&player_subtitle_menu());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    assert_invariants("player-subtitle-menu", &targets);
    assert_coverage("player-subtitle-menu", &targets);
    assert!(app.target("player.menu.subtitle.off").is_some());

    // 错误卡的重试按钮。
    app.set_player(&player_error());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    assert_invariants("player-error", &targets);
    assert_coverage("player-error", &targets);
    assert!(app.target("player.retry").is_some());
}

#[test]
fn retry_target_disappears_once_the_error_clears() {
    // 体检 R2-7：错误卡（`has-error`）是条件子树，元素消失后登记总线只增不减，
    // 旧的 `player.retry` 会留在清单里——用户看到一个“看得见、点了没反应”的死按钮。
    //
    // 以前 `App::targets()` 合并上一代与本代，元素消失后**第一轮刷新**里它仍会被
    // 报出来。这里故意只刷新一轮就断言：现在每轮刷新整张替换，不得有旧代残留。
    let app = window();
    app.set_player(&player_error());
    app.show_page(Page::Player);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    assert!(
        targets.iter().any(|target| target.id == "player.retry"),
        "失败态必须有「重试」目标"
    );

    // 重试成功 / 画面恢复，且控制层已自动隐藏（3 秒静默后）：这时页面上
    // 只剩常驻的 `player.mute` 会重新登记，旧清单整张换掉。
    app.set_player(&player_view().with_controls_visible(false));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    app.refresh_targets();
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    let targets = app.targets();
    assert!(
        !targets.iter().any(|target| target.id == "player.retry"),
        "播放正常（控制层已隐藏）时清单里不该还有「重试」：{:?}",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );

    // 控制层可见时也要消失（旧元素重新登记会整张换掉，这里避免回归）。
    app.set_player(&player_view());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    let targets = settle(&app, 80);
    assert!(
        !targets.iter().any(|target| target.id == "player.retry"),
        "播放正常时清单里不该还有「重试」：{:?}",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );
    assert_invariants("player-after-error", &targets);
    assert_coverage("player", &targets);
}

#[test]
fn dialog_and_banner_targets_are_registered() {
    let app = window();
    app.set_home(&home_filled());
    app.show_home();
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    app.set_banner(Some(
        BannerData::new("登录已过期", BannerLevel::Warning).with_action("重新登录"),
    ));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    let targets = settle(&app, 80);
    assert_invariants("banner", &targets);
    assert_coverage("banner", &targets);
    assert!(app.target("banner.action").is_some(), "横幅动作按钮没登记");

    // 对话框打开时：清单只剩 dialog.*（下层被遮罩挡住，点它们只会得到假结论）。
    app.open_add_server("http://127.0.0.1:8096", "nova");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));
    let targets = settle(&app, 80);
    assert_invariants("dialog", &targets);
    assert_coverage("dialog", &targets);
    assert!(
        targets
            .iter()
            .all(|target| target.id.starts_with("dialog.")),
        "对话框打开时清单里混进了下层目标：{:?}",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );
    for id in [
        "dialog.address",
        "dialog.user",
        "dialog.password",
        "dialog.cancel",
        "dialog.connect",
    ] {
        assert!(app.target(id).is_some(), "对话框缺少目标 {id}");
    }
}

/// 「查看全部 ›」必须打开该行对应的**媒体库列表页**，而不是行内第一张卡。
///
/// 以前它直接 `open-item(cards[0].id)`——用户点"查看全部"却进了某一部作品。
#[test]
fn row_more_opens_the_library_page() {
    let app = window();
    // 只留一个库行：目标清单只保留中心落在窗口内的元素，行太多会把库行挤到折叠下。
    let mut home = HomeView::new();
    home.library_rows = vec![LibraryRowView {
        library_id: "lib-1".to_string(),
        title: "电影".to_string(),
        items: vec![CardView::new("m1", "银翼杀手"), CardView::new("m2", "异形")],
    }];
    app.set_home(&home);
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    let targets = settle(&app, 300);
    let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    {
        let fired = fired.clone();
        app.on_tab_activated(move |id| fired.borrow_mut().push(id.to_owned()));
    }

    let target = app.target("home.row.lib.0.more").unwrap_or_else(|| {
        panic!(
            "库行的「查看全部」目标不存在，当前：{:?}",
            targets.iter().map(|t| t.id.clone()).collect::<Vec<_>>()
        )
    });
    let (cx, cy) = target.center();
    let window = app.ui().window();
    let position = slint::LogicalPosition::new(cx, cy);
    window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
    window.dispatch_event(slint::platform::WindowEvent::PointerPressed {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    window.dispatch_event(slint::platform::WindowEvent::PointerReleased {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(200));

    // 无头测试里没有真实库数据（`local.library()` 会失败、路由不变），
    // 所以断言"链路正确"：点击必须发出 `tab-activated(lib-1)`，
    // 而不是 `open-item(第一张卡)`。
    let ids = fired.borrow().clone();
    assert!(
        ids.iter().any(|id| id == "lib-1"),
        "「查看全部」应当切到该行的库 lib-1，实际触发：{ids:?}"
    );
}

/// 聚合行（「继续观看」「最近添加」）没有对应的媒体库页：
/// 不显示「查看全部 ›」，清单里也不能有 `*.more` 目标。
///
/// 以前它会退回 `open-item(cards[0].id)`——用户点"查看全部"却进了某一部作品的
/// 详情页，"看着有反应其实是空转"。没有可去的库页就不留这个入口（不新建列表页）。
/// 有库 id 的库行保持原样：显示「查看全部 ›」，点击切到对应库。
#[test]
fn aggregate_rows_have_no_more_target() {
    // 首页内容比一屏高（库行在 800 高窗口外，目标清单只收中心在窗口内的元素）。
    // 给一个高窗口，让继续观看行与库行都在视口里——与 `home_targets_cover_hero_rows_and_navbar`
    // 同一做法（真实窗口靠滚动，滚动后的坐标由 `interaction_sweep` 与真机验证覆盖）。
    let app = window_sized(WINDOW_W, 1600.0);
    let mut home = HomeView {
        hero: Some(CardView::new("h1", "沙丘：第二部")),
        continue_watching: vec![CardView::new("c1", "最后生还者")],
        library_rows: vec![LibraryRowView {
            library_id: "lib-1".to_string(),
            title: "电影".to_string(),
            items: vec![CardView::new("m1", "银翼杀手")],
        }],
        ..HomeView::new()
    };
    home.hero = home
        .hero
        .map(|hero| hero.with_overview("保罗·厄崔迪走上复仇之路。"));
    app.set_home(&home);
    app.ui().set_billboard_ratio(0.2);
    app.show_page(Page::Home);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
    let targets = settle(&app, 300);

    // 两行的卡片都在清单里（行本身可用，只是聚合行没有"查看全部"入口）。
    for id in ["home.row.continue.card.0", "home.row.lib.0.card.0"] {
        assert!(
            targets.iter().any(|target| target.id == id),
            "缺少卡片目标 {id}（清单：{:?}）",
            targets.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
    }
    for id in ["home.row.continue.more", "home.row.recent.more"] {
        assert!(
            app.target(id).is_none(),
            "聚合行不该登记「查看全部」目标 {id}（清单：{:?}）",
            targets.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
    }
    // 库行仍然有「查看全部 ›」（点击行为见 `row_more_opens_the_library_page`）。
    assert!(
        app.target("home.row.lib.0.more").is_some(),
        "库行必须有「查看全部」目标（清单：{:?}）",
        targets.iter().map(|t| &t.id).collect::<Vec<_>>()
    );

    // 人工复核用的帧：只有库行标题右侧有「查看全部 ›」。
    let snapshot = app.ui().window().take_snapshot().expect("首页快照");
    let path = common::snapshots_dir().join("home-row-more-only-for-libraries.png");
    common::write_png(&path, &snapshot);
    println!("snapshot 首页行「查看全部」: {}", path.display());
}

/// 通用护栏：任何条件子树消失后，**一次** `refresh_targets` 就不得再列出它的目标。
///
/// 回归：目标清单曾合并"上一代 + 本代"，条件子树（错误卡「重试」、对话框、暂停时的
/// 中央图标）消失后旧坐标会多留一代，只能逐个按视图状态打补丁过滤。这里用一个
/// **从没打过补丁**的条件目标（暂停时的中央大图标）证明根因已在清单层面解决。
#[test]
fn a_vanished_conditional_target_is_gone_after_one_refresh() {
    let app = window();
    app.set_player(&player_view().with_playing(false));
    app.show_page(Page::Player);
    app.ui().show().expect("显示窗口");
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(300));
    assert!(
        settle(&app, 80)
            .iter()
            .any(|t| t.id == "player.center-play"),
        "前提：暂停时应有中央大图标"
    );

    app.set_player(&player_view().with_playing(true));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    app.refresh_targets();
    let ids: Vec<String> = app.targets().into_iter().map(|t| t.id).collect();
    assert!(
        !ids.iter().any(|id| id == "player.center-play"),
        "恢复播放后一次刷新就不该再有中央大图标：{ids:?}"
    );
}
