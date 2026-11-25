// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 交互扫描（"死按钮"）回归测试。
//!
//! 用无头 testing 后端 + `App::sweep()`（与 `--debug-socket` 的 `sweep` / `dead`
//! 同一条实现）：对当前页面每个可交互目标做"悬停 → 点击"，比较状态摘要，然后
//! 把应用恢复到原页。**点击后状态没有任何变化**的目标记为 `dead`。
//!
//! 约定：
//! - `dead` 不是失败——有的目标本来就只改局部状态（悬停态）或是当前项的同页
//!   空操作（点已经选中的标签）。但**每个 dead 都必须出现在下面的 `KNOWN_DEAD`
//!   里并写明原因**；出现新的 dead 说明新控件没接行为，测试红灯。
//! - `KNOWN_DEAD` 里列了但实际不 dead 的条目也会失败（清单过时要删掉）。
//!
//! 本测试自己接了一小部分 `nova-app` 的回调（排序 / 标签 / 清空搜索），
//! 因为 `Shell` 需要真实事件循环才能构造。这些替身只做"重建视图"，
//! 与 `crates/nova-app/src/ui.rs` 的实现一一对应；真实实现的端到端验证见
//! `docs/debugging.md` 的真机操作步骤。

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use nova_app::Local;
use nova_app::env::{CredentialMode, Env};
use nova_app::views::LibrarySort;
use nova_core::{
    AuthState, ImageSet, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo, ServerKind,
    UserData,
};
use nova_provider::Library;
use nova_storage::Database;
use nova_ui::{App, AppWindow, Page, SweepReport};
use slint::ComponentHandle;

/// 一个页面在**本测试环境**里已知"点击后没有任何可观测变化"的目标。
///
/// 每条都必须能解释清楚；新增条目要同时想清楚"是不是让按钮变成死了"。
const KNOWN_DEAD: &[(&str, &str, &str)] = &[(
    "home",
    "home.nav.tab.0",
    "点已经选中的「首页」标签：重建出同一份首页，同页空操作",
)];

/// 一个页面的扫描结果摘要。
struct Scan {
    page: String,
    changed: usize,
    dead: Vec<(String, String, String)>,
}

fn server_info() -> ServerInfo {
    ServerInfo {
        id: ServerId::new("srv-sweep"),
        kind: ServerKind::Jellyfin,
        name: "扫描测试服务器".to_owned(),
        base_url: "http://127.0.0.1:8096".to_owned(),
        version: None,
        user_id: None,
        auth: AuthState::Unauthorized,
        metadata_language: Language::undetermined(),
    }
}

fn movie(id: &str, title: &str, year: u16, progress: u64) -> MediaItem {
    MediaItem {
        id: ItemId::new(id),
        kind: MediaKind::Movie,
        title: title.to_owned(),
        sort_title: title.to_owned(),
        original_title: None,
        year: Some(year),
        overview: Some("简介".to_owned()),
        genres: vec!["科幻".to_owned()],
        runtime: Some(Duration::from_secs(7200)),
        parent_id: None,
        index_number: None,
        index_number_end: None,
        child_count: None,
        images: ImageSet::default(),
        user_data: UserData {
            played: false,
            resume_position: Duration::from_secs(progress),
            play_count: 0,
            favorite: false,
        },
    }
}

/// 预置数据的状态目录：一部在看的片 + 若干部不同年份/标题的电影。
fn seeded_local(dir: &Path) -> Rc<Local> {
    {
        let db = Database::open(dir.join("nova.db")).expect("建库");
        db.upsert_server(&server_info(), 1).expect("写服务器");
        let items = vec![
            movie("dune2", "沙丘：第二部", 2024, 1800),
            movie("arrival", "降临", 2016, 0),
            movie("br2049", "银翼杀手 2049", 2017, 0),
            movie("parasite", "寄生虫", 2019, 0),
            movie("alien", "异形", 1979, 0),
            movie("heat", "盗火线", 1995, 0),
            movie("tenet", "信条", 2020, 0),
            movie("gravity", "地心引力", 2013, 0),
            movie("interstellar", "星际穿越", 2014, 0),
            movie("memento", "记忆碎片", 2000, 0),
        ];
        db.upsert_items(&ServerId::new("srv-sweep"), &items, 2)
            .expect("写条目");
    }
    let env = Env {
        data_dir: dir.to_path_buf(),
        credentials: CredentialMode::Memory,
    };
    let local = Local::open(&env, None).expect("打开状态目录");
    // 媒体库标签来自刷新结果（离线时没有 SQLite 表），这里直接给内存里塞两个，
    // 让顶栏标签成为真实的可点目标。
    local.set_libraries(vec![
        Library {
            id: ItemId::new("movies"),
            name: "电影".to_owned(),
            kind: MediaKind::Movie,
            collection_type: Some("movies".to_owned()),
            item_count: None,
        },
        Library {
            id: ItemId::new("shows"),
            name: "剧集".to_owned(),
            kind: MediaKind::Series,
            collection_type: Some("tvshows".to_owned()),
            item_count: None,
        },
    ]);
    Rc::new(local)
}

/// 无头后端（软件光栅器，不依赖合成器）。
fn init_backend() {
    let backend = i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
        },
    );
    let _ = slint::platform::set_platform(Box::new(backend));
}

/// 把界面接到本地缓存上（与 `nova-app` 的 Shell 同一套视图构建），
/// 并接住排序 / 标签 / 清空搜索这三个上层的回调。
fn build_app(local: &Rc<Local>, page: Page, billboard_ratio: f32) -> Rc<App> {
    init_backend();
    let app = Rc::new(App::new_with(AppWindow::new().expect("创建窗口")));
    app.ui().set_window_w(1280.0);
    app.ui().set_window_h(800.0);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    let home = local.home(None).expect("首页");
    app.set_home(&home);
    let tabs = local.tabs();
    let library_id = tabs
        .iter()
        .find(|tab| !tab.id.is_empty())
        .map(|tab| tab.id.clone())
        .unwrap_or_default();
    let _ = &library_id;
    let sort = Rc::new(RefCell::new(LibrarySort::default()));
    let library = local
        .library(&library_id, None, *sort.borrow())
        .expect("媒体库");
    app.set_library(&library);
    if let Some(first) = home.hero.as_ref().map(|card| card.item_id.clone()) {
        if let Ok(Some(detail)) = local.detail(&first) {
            app.set_detail(&detail);
        }
    }
    if let Ok(view) = local.search_view("沙丘", 60) {
        app.set_search(&view);
    }

    // ---- 上层回调的替身（真实实现在 `crates/nova-app/src/ui.rs`）----
    {
        let handler_app = Rc::clone(&app);
        let local = Rc::clone(local);
        let library_id = library_id.clone();
        let sort = Rc::clone(&sort);
        app.on_sort(move || {
            let next = sort.borrow().next();
            *sort.borrow_mut() = next;
            if let Ok(view) = local.library(&library_id, None, next) {
                handler_app.set_library(&view);
            }
        });
    }
    {
        let handler_app = Rc::clone(&app);
        let local = Rc::clone(local);
        app.on_tab_activated(move |id| {
            if id == nova_app::local::HOME_TAB_ID {
                if let Ok(home) = local.home(None) {
                    handler_app.set_home(&home);
                }
                // 与 `Shell::activate_tab` 一致：还要真的回首页（只重建数据的话
                // 在媒体库点「首页」什么都不会发生，那是个死按钮）。
                handler_app.show_home();
                return;
            }
            if let Ok(view) = local.library(id, None, LibrarySort::default()) {
                let _ = handler_app.open_library(&view);
            }
        });
    }
    {
        let handler_app = Rc::clone(&app);
        app.on_search_clear(move || {
            handler_app.set_search_text("");
        });
    }

    app.ui().set_billboard_ratio(billboard_ratio);
    if page == Page::Detail {
        // 详情页要压栈（`go_back` 才有地方可回），所以走真实的导航入口。
        if let Some(item) = home.hero.as_ref().map(|card| card.item_id.clone()) {
            let _ = app.open_detail(&item);
        } else {
            app.show_page(page);
        }
    } else {
        app.show_page(page);
    }
    app.ui().show().expect("显示窗口");
    // 入场动画结算（testing 后端不会自己走时间）。
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1200));
    app
}

fn scan(app: &Rc<App>) -> Scan {
    // 目标登记要跑两轮（见 `App::refresh_targets` 的说明与交互目标测试）。
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    let report: SweepReport = app.sweep();
    Scan {
        page: report.page.clone(),
        changed: report.entries.iter().filter(|entry| entry.changed).count(),
        dead: report
            .dead()
            .iter()
            .map(|entry| (report.page.clone(), entry.id.clone(), entry.label.clone()))
            .collect(),
    }
}

fn assert_known_dead(scan: &Scan) {
    let known: Vec<&str> = KNOWN_DEAD
        .iter()
        .filter(|(page, _, _)| *page == scan.page)
        .map(|(_, id, _)| *id)
        .collect();
    for (page, id, label) in &scan.dead {
        assert!(
            known.contains(&id.as_str()),
            "{page}: `{id}`（label：{label}）点击后状态没有任何变化——新死按钮？\
             确认它确实不需要行为后，才把它加进 KNOWN_DEAD 并写明原因"
        );
    }
    for id in known {
        assert!(
            scan.dead.iter().any(|(_, dead_id, _)| dead_id == id),
            "{}: KNOWN_DEAD 里的 {id} 现在有反应了，请从清单里删掉",
            scan.page
        );
    }
}

#[test]
fn library_sweep_has_live_controls_and_no_unknown_dead_targets() {
    let dir = tempfile::tempdir().expect("临时目录");
    let local = seeded_local(dir.path());
    let app = build_app(&local, Page::Library, 0.88);

    let scan = scan(&app);
    assert_eq!(scan.page, "library");
    // 排序按钮 / 卡片 / 标签 / 搜索入口都应该有反应。
    assert!(
        scan.changed >= 6,
        "媒体库只有 {} 个目标产生了状态变化（dead：{:?}）",
        scan.changed,
        scan.dead
    );
    assert_known_dead(&scan);

    // 排序按钮是这次修复的核心：点一下排序标签必须真的变。
    let before = app.target("library.sort").expect("排序按钮").label;
    let sort = app.target("library.sort").expect("排序按钮");
    let (x, y) = sort.center();
    app.click_at(x, y).expect("点击排序");
    let after = app.target("library.sort").map(|target| target.label);
    assert!(after.is_some(), "点完排序按钮后目标清单不应为空");
    assert_ne!(
        Some(before.clone()),
        after,
        "点排序按钮前后标签一样（{before}）——排序回调整理又变回空实现了吗？"
    );
}

#[test]
fn home_sweep_has_live_controls_and_no_unknown_dead_targets() {
    let dir = tempfile::tempdir().expect("临时目录");
    let local = seeded_local(dir.path());
    // 压缩 billboard，让行区进入首屏（首页 hero 占 88vh）。
    let app = build_app(&local, Page::Home, 0.2);
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(600));

    let scan = scan(&app);
    assert_eq!(scan.page, "home");
    assert!(
        scan.changed >= 4,
        "首页只有 {} 个目标产生了状态变化（dead：{:?}）",
        scan.changed,
        scan.dead
    );
    assert_known_dead(&scan);
}

#[test]
fn detail_sweep_has_live_controls_and_no_unknown_dead_targets() {
    let dir = tempfile::tempdir().expect("临时目录");
    let local = seeded_local(dir.path());
    let app = build_app(&local, Page::Detail, 0.88);

    let scan = scan(&app);
    assert_eq!(scan.page, "detail");
    // 返回 / 播放 / 我的列表 / 版本下拉都应该有反应。
    assert!(
        scan.changed >= 3,
        "详情页只有 {} 个目标产生了状态变化（dead：{:?}）",
        scan.changed,
        scan.dead
    );
    assert_known_dead(&scan);
}
