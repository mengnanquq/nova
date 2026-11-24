// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放页交互：真键盘事件 → 视图模型回调、拖动进度条不发 seek、3 秒自动隐藏。
//!
//! 自动隐藏不依赖 Slint `Timer`：testing backend（`threading: false`）不跑定时器，
//! 所以时间由 `App::tick_player(delta)` 推进视图模型，测试灌的就是虚拟时间。

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use common::{detail_view, init_backend, player_view};
use nova_ui::{App, AppWindow, Page, PlayerKey};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn press_text(app: &App, text: &str) {
    let window = app.ui().window();
    let text: slint::SharedString = text.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

/// 起一个停在播放页的应用（导航栈 Home → Detail → Player）。
fn player_app() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.set_detail(&detail_view());
    app.open_detail("dune");
    app.play("dune");
    assert_eq!(app.page(), Page::Player, "应先切到播放页");
    app.set_player(&player_view());
    app
}

fn record_seeks(app: &App) -> Rc<RefCell<Vec<Duration>>> {
    let seeks = Rc::new(RefCell::new(Vec::new()));
    let sink = seeks.clone();
    app.on_player_seek(move |target| sink.borrow_mut().push(target));
    seeks
}

#[test]
fn right_arrow_seeks_forward_fifteen_seconds_with_exact_target() {
    let app = player_app();
    let seeks = record_seeks(&app);

    press(&app, Key::RightArrow);
    assert_eq!(
        seeks.borrow().as_slice(),
        &[Duration::from_secs(769)],
        "12:34 + 15s = 12:49（769 秒）"
    );
    assert_eq!(app.player_view().position, Duration::from_secs(769));

    press(&app, Key::LeftArrow);
    assert_eq!(
        seeks.borrow().as_slice(),
        &[Duration::from_secs(769), Duration::from_secs(754)],
        "12:49 - 15s = 12:34"
    );

    // Shift+←/→ 在 .slint 里映射到 ±60s；testing backend 的 WindowEvent 不带修饰键，
    // 所以这里直接走同一入口验证参数。
    let seeks = record_seeks(&app);
    assert!(app.handle_player_key(PlayerKey::SeekForward60));
    assert_eq!(seeks.borrow().as_slice(), &[Duration::from_secs(814)]);
    assert!(app.handle_player_key(PlayerKey::SeekBack60));
    assert_eq!(
        seeks.borrow().as_slice(),
        &[Duration::from_secs(814), Duration::from_secs(754)]
    );
}

#[test]
fn s_opens_subtitle_menu_and_a_switches_to_audio_menu() {
    let app = player_app();

    press_text(&app, "s");
    let data = app.ui().get_player();
    assert!(data.subtitle_menu_open, "S 应打开字幕菜单");
    assert!(!data.audio_menu_open, "两个菜单互斥");

    press_text(&app, "a");
    let data = app.ui().get_player();
    assert!(data.audio_menu_open, "A 应打开音轨菜单");
    assert!(!data.subtitle_menu_open, "打开音轨菜单会收起字幕菜单");

    press_text(&app, "a");
    assert!(
        !app.ui().get_player().audio_menu_open,
        "再按一次 A 收起菜单"
    );
}

#[test]
fn escape_commits_progress_before_leaving_player() {
    let app = player_app();

    let order = Rc::new(RefCell::new(Vec::<String>::new()));
    {
        let order = order.clone();
        let weak = app.ui().as_weak();
        app.on_player_exit(move |position| {
            let page = weak.upgrade().map(|ui| ui.get_page()).unwrap_or(-1);
            order
                .borrow_mut()
                .push(format!("exit page={page} position={}", position.as_secs()));
        });
    }

    press(&app, Key::Escape);

    assert_eq!(
        order.borrow().as_slice(),
        &["exit page=3 position=754".to_string()],
        "退出回调必须带着最终进度、且发生在离开播放页之前（page 还是 3）"
    );
    assert_eq!(app.page(), Page::Detail, "Esc 之后回到详情页");
}

#[test]
fn controls_hide_after_three_seconds_and_any_key_reveals_them() {
    let app = player_app();
    assert!(app.ui().get_player().controls_visible);

    // 2.75 秒还不该隐藏。
    assert!(!app.tick_player(Duration::from_millis(2750)));
    assert!(app.ui().get_player().controls_visible);

    // 跨过 3 秒隐藏。
    assert!(app.tick_player(Duration::from_millis(300)));
    assert!(
        !app.ui().get_player().controls_visible,
        "静默 3 秒后控制层要隐藏"
    );

    // 任何键都立刻唤醒（这里是 →：seek 同时算一次输入）。
    press(&app, Key::RightArrow);
    assert!(
        app.ui().get_player().controls_visible,
        "按键应立刻显示控制层"
    );

    // 鼠标移动同理（Slint 的 input 回调 → App::player_note_input）。
    app.tick_player(Duration::from_millis(4000));
    assert!(!app.ui().get_player().controls_visible);
    app.player_note_input();
    assert!(app.ui().get_player().controls_visible);
}

#[test]
fn paused_and_buffering_players_never_auto_hide() {
    let app = player_app();
    app.set_player(&player_view().with_playing(false));
    assert!(!app.tick_player(Duration::from_secs(10)));
    assert!(app.ui().get_player().controls_visible, "暂停时控件常显");

    app.set_player(&player_view().with_buffering(true));
    assert!(!app.tick_player(Duration::from_secs(10)));
    assert!(app.ui().get_player().controls_visible, "缓冲中控件常显");
}

#[test]
fn scrubbing_updates_bubble_only_and_seeks_on_commit() {
    let app = player_app();
    let seeks = record_seeks(&app);

    app.player_scrub_start();
    app.player_scrub_move(0.5);
    assert!(seeks.borrow().is_empty(), "拖动期间不许发 seek");
    assert_eq!(app.player_view().position, Duration::from_secs(754));
    assert_eq!(app.ui().get_player_scrub_text().as_str(), "1:09:00");

    app.player_scrub_commit(0.5);
    assert_eq!(
        seeks.borrow().as_slice(),
        &[Duration::from_secs(4140)],
        "松手才 seek 到 50% 处"
    );
    assert_eq!(app.player_view().position, Duration::from_secs(4140));
    assert_eq!(app.ui().get_player_scrub_text().as_str(), "");
}

#[test]
fn space_toggles_playback_and_volume_keys_step_by_five_percent() {
    let app = player_app();
    let states = Rc::new(RefCell::new(Vec::new()));
    {
        let states = states.clone();
        app.on_player_play_pause(move |playing| states.borrow_mut().push(playing));
    }
    let volumes = Rc::new(RefCell::new(Vec::new()));
    {
        let volumes = volumes.clone();
        app.on_player_volume(move |volume| volumes.borrow_mut().push(volume));
    }

    press(&app, Key::Space);
    assert_eq!(states.borrow().as_slice(), &[false], "从播放切到暂停");
    press(&app, Key::Space);
    assert_eq!(states.borrow().as_slice(), &[false, true]);

    press(&app, Key::UpArrow);
    press(&app, Key::UpArrow);
    press(&app, Key::DownArrow);
    let volumes = volumes.borrow().clone();
    assert_eq!(volumes.len(), 3);
    assert!((volumes[0] - 0.65).abs() < 0.001, "↑ = +5%");
    assert!((volumes[1] - 0.70).abs() < 0.001);
    assert!((volumes[2] - 0.65).abs() < 0.001, "↓ = -5%");

    press_text(&app, "m");
    assert!(app.ui().get_player().muted, "M 静音");
    assert_eq!(app.ui().get_player().volume_text.as_str(), "静音");
}

#[test]
fn audio_track_selection_pushes_engine_command_and_closes_menu() {
    let app = player_app();
    let picked = Rc::new(RefCell::new(Vec::new()));
    {
        let picked = picked.clone();
        app.on_player_audio_track(move |index| picked.borrow_mut().push(index));
    }

    press_text(&app, "a");
    assert!(app.ui().get_player().audio_menu_open);

    // .slint 侧点菜单项 → 这里直接调同一个入口。
    app.ui().invoke_player_audio_selected(2);

    assert_eq!(picked.borrow().as_slice(), &[2]);
    let data = app.ui().get_player();
    assert!(!data.audio_menu_open, "选完立刻收起菜单");
    let selected: Vec<bool> = data
        .audio_tracks
        .iter()
        .map(|track| track.selected)
        .collect();
    assert_eq!(selected, vec![false, true, false], "只有第 2 条被打勾");

    let subtitles = Rc::new(RefCell::new(Vec::new()));
    {
        let subtitles = subtitles.clone();
        app.on_player_subtitle_track(move |index| subtitles.borrow_mut().push(index));
    }
    app.ui().invoke_player_subtitle_selected(-1);
    assert_eq!(subtitles.borrow().as_slice(), &[None], "-1 表示关闭字幕");
}

#[test]
fn error_and_hwdec_status_are_surfaced_in_the_data() {
    let app = player_app();
    app.set_player(
        &player_view()
            .with_error("服务器不可达，检查网络或服务器状态", "重试")
            .with_hwdec_notice("硬件解码未生效：正在用 CPU 软解"),
    );
    let data = app.ui().get_player();
    assert!(data.has_error);
    assert_eq!(data.error.as_str(), "服务器不可达，检查网络或服务器状态");
    assert_eq!(data.error_action.as_str(), "重试");
    assert_eq!(
        data.status_line.as_str(),
        "硬件解码未生效：正在用 CPU 软解",
        "hwdec 提示压过诊断行"
    );
}

#[test]
fn time_text_switches_between_elapsed_and_remaining() {
    let app = player_app();
    assert_eq!(app.ui().get_player().time_text.as_str(), "12:34 / 2:18:00");

    app.ui().invoke_player_time_toggle();
    assert_eq!(
        app.ui().get_player().time_text.as_str(),
        "-2:05:26",
        "点时间文本切到剩余，且不再同时渲染已播/总长"
    );

    app.ui().invoke_player_time_toggle();
    assert_eq!(app.ui().get_player().time_text.as_str(), "12:34 / 2:18:00");
}

#[test]
fn engine_detail_row_is_collapsed_by_default_and_toggles_with_ctrl_d() {
    let app = player_app();
    let data = app.ui().get_player();
    assert_eq!(data.transport_mode.as_str(), "直连", "常驻只有短标签");
    assert_eq!(data.engine_line.as_str(), "直连 · hwdec=vaapi · 1080p60");
    assert!(!data.show_engine_detail, "完整诊断行默认收起");

    assert!(app.handle_player_key(PlayerKey::ToggleEngineDetail));
    assert!(app.ui().get_player().show_engine_detail, "Ctrl+D 展开");
    assert!(app.handle_player_key(PlayerKey::ToggleEngineDetail));
    assert!(!app.ui().get_player().show_engine_detail);
}

#[test]
fn volume_number_is_only_visible_while_adjusting() {
    let app = player_app();
    assert!(!app.ui().get_player().volume_hint, "平时不显示数字文本");

    press(&app, Key::UpArrow);
    assert!(app.ui().get_player().volume_hint, "调节时短时显示");

    // 1.2 秒后收起（测试灌虚拟时间）。
    app.tick_player(Duration::from_millis(1200));
    assert!(!app.ui().get_player().volume_hint);
}

#[test]
fn subtitle_margin_follows_control_visibility() {
    let app = player_app();
    let margins = Rc::new(RefCell::new(Vec::new()));
    {
        let margins = margins.clone();
        app.on_player_subtitle_margin(move |margin| margins.borrow_mut().push(margin));
    }

    // 先隐藏一次：把内部缓存的值变成 0，再显示才能观察到一次上升。
    app.set_player(&player_view().with_controls_visible(false));
    app.set_player(&player_view());
    assert_eq!(
        margins.borrow().as_slice(),
        &[0, 280],
        "控制层隐藏时归零，可见时抬起（800px 窗口 → 280px）"
    );
}
