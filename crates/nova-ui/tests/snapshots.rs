// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 无头快照：testing backend 内嵌软件光栅器，把页面渲染成 `target/snapshots/*.png`。

mod common;

use common::{
    SUBTITLE_BAND_TOP, bright_frame, detail_view, home_empty, home_filled, home_image_ids,
    init_backend, install_backdrops, install_posters, library_view, player_audio_menu,
    player_buffering, player_controls_hidden, player_error, player_subtitle_menu, player_view,
    snapshots_dir, write_png,
};
use nova_ui::{App, AppWindow, BannerData, BannerLevel, KeyCommand, Page};
use slint::ComponentHandle;
use slint::platform::{Key, WindowEvent};

fn capture(app: &App, name: &str) {
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    let snapshot = app.ui().window().take_snapshot().unwrap();
    let path = snapshots_dir().join(name);
    write_png(&path, &snapshot);
    println!(
        "snapshot {name}: {}x{} -> {}",
        snapshot.width(),
        snapshot.height(),
        path.display()
    );
}

#[test]
fn write_all_snapshots() {
    init_backend();

    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));

    // 1. 空首页
    app.set_home(&home_empty());
    app.show_page(Page::Home);
    capture(&app, "home-empty.png");

    // 2. 有内容的首页
    let home = home_filled();
    install_posters(&app, &home_image_ids(&home));
    install_backdrops(&app, &["dune"]);
    app.set_home(&home);
    app.ui().set_home_focus(2);
    app.show_page(Page::Home);
    capture(&app, "home-filled.png");

    // 2a. 顶部横幅（warning + 动作按钮）：必须在滚动前拍——
    // 横幅只应把内容整体下移 48px，元素尺寸不变（卡片高度与 home-filled 逐像素一致）。
    app.set_banner(Some(
        BannerData::new(
            "登录已过期：重新运行 nova login；本地缓存仍可浏览",
            BannerLevel::Warning,
        )
        .with_action("重新登录"),
    ));
    capture(&app, "banner-warning.png");
    app.set_banner(None);

    // 2b. 搜索框聚焦 + 有输入（占位消失，右侧出现清除按钮）；拍完退出聚焦并清空。
    assert!(app.handle_key(KeyCommand::Search));
    let window = app.ui().window();
    for ch in "dune".chars() {
        let text: slint::SharedString = ch.to_string().into();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }
    capture(&app, "search-active.png");
    app.set_search_text("");
    let escape: slint::SharedString = Key::Escape.into();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: escape.clone(),
    });
    window.dispatch_event(WindowEvent::KeyReleased { text: escape });

    // 2c. 首页滚动后：区块标题贴住视口顶（粘性），不会被视口底腰斩。
    // 用鼠标移入 + 滚轮事件推进 Flickable（testing backend 会正常处理 Wheel 事件）。
    app.ui().window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(640.0, 500.0),
    });
    app.ui()
        .window()
        .dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(640.0, 500.0),
            delta_x: 0.0,
            delta_y: -480.0,
        });
    capture(&app, "home-scrolled.png");

    // 3. 媒体库
    let library = library_view();
    let library_ids: Vec<&str> = library
        .cards
        .iter()
        .map(|card| card.item_id.as_str())
        .collect();
    // 最后一格故意不推图片：快照里可以看到占位（首字母）且网格尺寸不变。
    install_posters(&app, &library_ids[..library_ids.len().saturating_sub(1)]);
    app.open_library(&library);
    app.ui().set_library_focus(4);
    capture(&app, "library.png");

    // 4. 详情页
    let detail = detail_view();
    app.set_backdrop("dune", common::backdrop("dune"));
    app.set_poster("dune", common::poster("dune"));
    app.set_detail(&detail);
    app.ui().set_detail_focus(0);
    app.show_page(Page::Detail);
    capture(&app, "detail.png");

    // 5. 播放页：控制层可见（先用详情页 → 播放，让播放页拿到真实条目 id 与背景图）
    app.open_detail("dune");
    app.play("dune");
    app.set_player(&player_view());
    assert_eq!(app.page(), Page::Player);
    capture(&app, "player.png");
    capture(&app, "player-controls.png");

    // 6. 播放页：3 秒静默后控制层隐藏（只剩画面）
    app.set_player(&player_controls_hidden());
    capture(&app, "player-controls-hidden.png");

    // 7. 播放页：缓冲中（画面保留 + 角落提示，控件不自动隐藏）
    app.set_player(&player_buffering());
    capture(&app, "player-buffering.png");

    // 8. 播放页：错误卡（原因 + 重试）
    app.set_player(&player_error());
    capture(&app, "player-error.png");

    // 9. 播放页：音轨菜单
    app.set_player(&player_audio_menu());
    capture(&app, "player-audio-menu.png");

    // 10. 播放页：字幕菜单（关闭 + 图形字幕说明 + 延迟 / 外挂字幕）
    app.set_player(&player_subtitle_menu());
    capture(&app, "player-subtitle-menu.png");

    // 11. 播放页：亮色假帧（米白 / 天空色）。
    // 遮罩的两段式渐变必须在亮画面上也能让标题 / 副标题 / 诊断行读得清。
    // 字幕带画在“已抬起”的位置（控制层可见时引擎把 sub-margin-y 抬到 ≈280px），
    // 验证传输行不会再盖住字幕。
    app.set_backdrop("dune", bright_frame(SUBTITLE_BAND_TOP - 280));
    app.set_player(&player_view());
    capture(&app, "player-controls-bright.png");

    // 12. 播放页：刚按过静音 / 音量键——数字文本短时可见（喇叭图标变成斜线）。
    let mut volume = player_view().with_muted(true);
    volume.note_volume_change();
    app.set_player(&volume);
    capture(&app, "player-volume-hint.png");
}
