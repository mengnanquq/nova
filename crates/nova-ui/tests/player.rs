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

use common::{detail_view, init_backend, player_controls_hidden, player_view};
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

/// 菜单展开时 `Esc` / `Backspace` **先收菜单**，再按一次才退出播放；
/// 退出播放（含点「返回」直接退）时菜单状态必须复位。
///
/// 体检 R2-4：此前 `page == 3` 分支对 Esc 无条件发 `player-key(12)`，菜单展开时
/// 一下就把用户踢出播放页；回到详情页后 `view.player.audio_menu` 还留着 true。
#[test]
fn escape_closes_open_menu_before_leaving_player() {
    let app = player_app();

    press_text(&app, "s");
    assert!(app.ui().get_player().subtitle_menu_open, "S 展开字幕菜单");
    press(&app, Key::Escape);
    assert_eq!(
        app.page(),
        Page::Player,
        "第一下 Esc 只收菜单，不离开播放页"
    );
    assert!(!app.ui().get_player().subtitle_menu_open, "字幕菜单已收起");

    // Backspace 与 Esc 同一语义。
    press_text(&app, "a");
    assert!(app.ui().get_player().audio_menu_open);
    press(&app, Key::Backspace);
    assert_eq!(app.page(), Page::Player, "Backspace 同样只收菜单");
    assert!(!app.ui().get_player().audio_menu_open);

    // 菜单已收起，再按 Esc 才退出。
    press(&app, Key::Escape);
    assert_eq!(app.page(), Page::Detail, "菜单收起后 Esc 才退出播放");
    assert!(
        !app.player_view().menus_open(),
        "退出后菜单状态必须复位（不能残留 audio_menu/subtitle_menu）"
    );

    // 菜单展开时点「返回」直接退出（不经过 Esc）：退出后同样要复位。
    app.play("dune");
    assert_eq!(app.page(), Page::Player);
    press_text(&app, "a");
    assert!(app.ui().get_player().audio_menu_open);
    app.ui().invoke_player_back();
    assert_eq!(app.page(), Page::Detail);
    assert!(
        !app.player_view().menus_open(),
        "点「返回」退出后菜单状态也要复位"
    );
}

/// 控制层隐藏时 `↑` / `↓` 只「唤醒」，不改音量；控制层可见时才调音量。
///
/// 体检 R2-5：`page == 3` 分支对所有键先 `player-input()`（唤醒），Down 又落到
/// `player-key(7)`（音量 −5%），于是每次唤醒都无声地降音量（实测 0.95 → 0.35）。
/// 依据：`docs/debugging.md` 一直把 `key Down` 当作「唤醒」键用，唤醒不该附带
/// 音量副作用；`←` / `→` 的快进快退、`Space` 的暂停在隐藏时仍直接生效。
#[test]
fn volume_keys_do_not_fire_on_the_wake_up_press() {
    let app = player_app();
    let volumes = Rc::new(RefCell::new(Vec::new()));
    {
        let volumes = volumes.clone();
        app.on_player_volume(move |volume| volumes.borrow_mut().push(volume));
    }

    // 隐藏态 ↓：只唤醒，音量不变。
    app.set_player(&player_controls_hidden());
    assert!((app.player_view().volume - 0.6).abs() < 1e-6);
    press(&app, Key::DownArrow);
    assert!(
        app.ui().get_player().controls_visible,
        "↓ 在隐藏态应唤醒控制层"
    );
    assert!(
        (app.player_view().volume - 0.6).abs() < 1e-6,
        "唤醒那一下不该改音量，实际 {}",
        app.player_view().volume
    );
    assert!(volumes.borrow().is_empty(), "不应对引擎发音量变更");

    // 隐藏态 ↑：同样只唤醒。
    app.set_player(&player_controls_hidden());
    press(&app, Key::UpArrow);
    assert!(app.ui().get_player().controls_visible);
    assert!((app.player_view().volume - 0.6).abs() < 1e-6);
    assert!(volumes.borrow().is_empty());

    // 控制层可见后 ↓ / ↑ 才调音量。
    press(&app, Key::DownArrow);
    press(&app, Key::UpArrow);
    let sent = volumes.borrow().clone();
    assert_eq!(sent.len(), 2, "可见态 ↑/↓ 各发一次音量：{sent:?}");
    assert!((sent[0] - 0.55).abs() < 1e-6, "可见态 ↓ = -5%");
    assert!((sent[1] - 0.60).abs() < 1e-6, "可见态 ↑ = +5%");

    // 隐藏态的快进仍然直接生效（不要顺手把 ←/→ 也变成「只唤醒」）。
    let seeks = record_seeks(&app);
    app.set_player(&player_controls_hidden());
    press(&app, Key::RightArrow);
    assert_eq!(seeks.borrow().len(), 1, "隐藏态 → 仍然直接快进 15s");
    assert!(app.ui().get_player().controls_visible);
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
fn time_text_defaults_to_remaining_and_switches_to_elapsed() {
    let app = player_app();
    // 官方播放器的时间位是剩余时长（`.time-remaining__time`），默认就该是它。
    assert_eq!(app.ui().get_player().time_text.as_str(), "-2:05:26");

    app.ui().invoke_player_time_toggle();
    assert_eq!(
        app.ui().get_player().time_text.as_str(),
        "12:34 / 2:18:00",
        "点时间文本切成已播/总长，且不同时渲染两份"
    );

    app.ui().invoke_player_time_toggle();
    assert_eq!(app.ui().get_player().time_text.as_str(), "-2:05:26");
}

#[test]
fn volume_slider_sets_absolute_volume_and_unmutes() {
    let app = player_app();
    let volumes = Rc::new(RefCell::new(Vec::new()));
    let sink = volumes.clone();
    app.on_player_volume(move |v| sink.borrow_mut().push(v));

    // 滑杆 = 直接设到百分比（不是加减）。
    app.ui().invoke_player_volume_set(0.25);
    assert!((app.player_view().volume - 0.25).abs() < f32::EPSILON);
    assert_eq!(volumes.borrow().as_slice(), &[0.25]);

    // 往上拖解除静音（与键盘加音量同一口径）。
    app.ui().invoke_player_mute_toggle();
    assert!(app.player_view().muted);
    app.ui().invoke_player_volume_set(0.5);
    assert!(!app.player_view().muted);
    assert_eq!(volumes.borrow().as_slice(), &[0.25, 0.5]);

    // 越界值夹到 0..1。
    app.ui().invoke_player_volume_set(1.7);
    assert!((app.player_view().volume - 1.0).abs() < f32::EPSILON);
}

/// 静音后把音量调大必须**真的**解除静音：界面与引擎两份状态都要变。
///
/// 此前只改界面那一份：宿主每帧把自己的 `muted` 推回界面（`player/mod.rs` 的
/// `view.muted = self.muted`），下一次 tick 又被刷回“静音”，而 mpv 的 `mute`
/// 属性一直没被清掉——界面显示已取消静音、实际没声音。真机复现：先 M 静音，
/// 再拖滑杆到 50%，`view` 里 muted 仍是 true、音量却变成 0.5。
#[test]
fn raising_volume_while_muted_unmutes_the_engine_too() {
    let app = player_app();
    let muted = Rc::new(RefCell::new(Vec::new()));
    {
        let muted = muted.clone();
        app.on_player_muted(move |value| muted.borrow_mut().push(value));
    }

    app.ui().invoke_player_mute_toggle();
    assert!(app.player_view().muted);
    assert_eq!(muted.borrow().as_slice(), &[true]);

    // 滑杆：非零音量 = 解除静音，并且要通知引擎。
    app.ui().invoke_player_volume_set(0.5);
    assert!(!app.player_view().muted, "拖滑杆后界面必须显示已取消静音");
    assert_eq!(
        muted.borrow().as_slice(),
        &[true, false],
        "引擎必须收到取消静音（否则 mpv 的 mute 仍是 yes）"
    );

    // 键盘 ↑：与滑杆同一口径。
    app.ui().invoke_player_mute_toggle();
    assert!(app.player_view().muted);
    assert!(app.handle_player_key(PlayerKey::VolumeUp));
    assert!(!app.player_view().muted, "↑ 应解除静音");
    assert_eq!(muted.borrow().as_slice(), &[true, false, true, false]);

    // 往下调音量不是“解除静音”的意图。
    app.ui().invoke_player_mute_toggle();
    assert!(app.player_view().muted);
    assert!(app.handle_player_key(PlayerKey::VolumeDown));
    assert!(app.player_view().muted, "↓ 不该悄悄取消静音");
    assert_eq!(muted.borrow().as_slice(), &[true, false, true, false, true]);
}

#[test]
fn engine_detail_stays_data_only_and_is_never_rendered() {
    // 引擎诊断（直连/转码、hwdec、1080p60）只是 `PlayerData` 里的数据：
    // 控制层上**没有**任何常驻 / 可展开的诊断文字（Netflix 播放器没有）。
    let app = player_app();
    let data = app.ui().get_player();
    assert_eq!(data.transport_mode.as_str(), "直连");
    assert_eq!(data.engine_line.as_str(), "直连 · hwdec=vaapi · 1080p60");
    assert!(!data.show_engine_detail, "展开位只是数据，不在画面上渲染");

    // 画面不改：Ctrl+D 已从播放页快捷键表里移除（13 号意图不再存在）。
    assert_eq!(PlayerKey::from_intent(13), None, "Ctrl+D 不再是播放页按键");

    // 数据侧仍可翻转（调试通道 / 测试用），但没有任何 .slint 元素读它。
    let mut view = app.player_view();
    view.toggle_engine_detail();
    assert!(view.show_engine_detail);
    view.toggle_engine_detail();
    assert!(!view.show_engine_detail);
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
