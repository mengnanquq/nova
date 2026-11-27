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

use common::{detail_view, init_backend, player_controls_hidden, player_error, player_view};
use nova_ui::{App, AppWindow, Page, PlayerKey, Route};
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
fn navigating_away_from_the_player_route_still_exits_the_session() {
    // 体检 R4-2：调试通道的 `route` / `open` 不走 Esc，但换页就是离开播放页，
    // 会话必须先提交进度并收尾（否则 mpv 在后台continue，错误也会跟着留在
    // `view.player` 里）。
    let app = player_app();
    let exits = Rc::new(RefCell::new(Vec::<Duration>::new()));
    {
        let exits = exits.clone();
        app.on_player_exit(move |position| exits.borrow_mut().push(position));
    }

    assert!(app.open_detail("other"), "导航 API 应能直接打开另一条目");

    assert_eq!(
        exits.borrow().as_slice(),
        &[Duration::from_secs(754)],
        "直接换页也必须先退出播放会话，并带上最终进度"
    );
    assert_eq!(app.page(), Page::Detail);
    assert!(!matches!(
        app.current_route(),
        Some(nova_ui::Route::Player { .. })
    ));
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
fn error_state_does_not_register_seek_or_time_targets() {
    // 体检 R3-3：错误态下会话是 `Failed`，seek / 拖动 / 切换时间都没有意义。
    // 它们如果还登记在目标清单里，就是个“点得中、但没有反应”的死控件；
    // 这里断言错误态不登记它们，正常态照旧，与「重试只在错误态出现」对称。
    let app = player_app();
    app.ui().show().expect("显示窗口");

    let seek_targets = [
        "player.progress",
        "player.seek-back",
        "player.seek-forward",
        "player.time",
    ];
    let normal = player_targets(&app);
    for id in seek_targets {
        assert!(
            normal.iter().any(|target| target.id == id),
            "正常播放时 {id} 必须可点（清单：{:?}）",
            normal.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
    }

    app.set_player(&player_error());
    let failed = player_targets(&app);
    for id in seek_targets {
        assert!(
            !failed.iter().any(|target| target.id == id),
            "错误态不该登记没有意义的 {id}（清单：{:?}）",
            failed.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
    }
    assert!(
        failed.iter().any(|target| target.id == "player.retry"),
        "错误态必须保留「重试」"
    );
    assert!(
        failed.iter().any(|target| target.id == "player.play-pause"),
        "非 seek 控件（播放 / 暂停）仍然可用"
    );
    assert!(
        !failed
            .iter()
            .any(|target| target.id == "player.center-play"),
        "错误态不显示中央暂停图标"
    );
}

/// 推进时间 + 让目标重新登记两轮，然后读当前页面的清单。
fn player_targets(app: &App) -> Vec<nova_ui::TargetInfo> {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(40));
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(80));
    app.targets()
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
    // 1280×800：控制块 178.4 + 底边 gutter 72 + 字幕安全间距 16 = 266.4 → 266。
    // 控制层隐藏时归零。不是窗口高 800 的 35%（280）。
    assert_eq!(
        margins.borrow().as_slice(),
        &[0, 266],
        "控制层隐藏时归零，可见时按控制块实际布局高度抬起"
    );
}

/// 矮窗口回归：800×600 时控制块（标题行 + 进度条 + 按钮行 + gutter）占窗口高度
/// 约 42%（≈250px），旧的固定系数 0.35 × 600 = 210px 盖不住它（字幕贴到标题行）。
/// 现在推的是控制块顶边到窗口底边的实际距离 266px。
#[test]
fn subtitle_margin_covers_the_control_block_in_a_short_window() {
    let app = player_app();
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(800.0, 600.0));
    let margins = Rc::new(RefCell::new(Vec::new()));
    {
        let margins = margins.clone();
        app.on_player_subtitle_margin(move |margin| margins.borrow_mut().push(margin));
    }

    app.set_player(&player_view());
    assert_eq!(
        margins.borrow().as_slice(),
        &[266],
        "800×600：旧公式 600×0.35=210 小于控制块 250，会重叠；新值按实际布局 266"
    );
}

/// 错误态下 ←/→ 不做"乐观 seek"：界面上的进度条 / ±15s 已禁用，键盘同理。
///
/// 回归：以前错误态按 → 会先把界面进度改成 +15s 并发出 seek 回调；引擎在 `Failed`
/// 状态下拒绝 seek，于是进度数字跳了一下却永远回不去——"按了有变化、其实没生效"。
#[test]
fn arrow_keys_do_not_fake_a_seek_in_the_error_state() {
    let app = player_app();
    app.set_player(&player_error());
    let seeks = record_seeks(&app);
    let before = app.player_view().position;

    press(&app, Key::RightArrow);
    press(&app, Key::LeftArrow);
    assert!(app.handle_player_key(PlayerKey::SeekForward60));

    assert!(
        seeks.borrow().is_empty(),
        "错误态不该发出 seek：{:?}",
        seeks.borrow()
    );
    assert_eq!(
        app.player_view().position,
        before,
        "错误态进度不该被乐观改写"
    );
}

/// 体检 R7-D3：详情页还贴着一帧前的 A 时点「播放」，真正起播的必须是路由里的 B。
///
/// `app.slint` 传上来的是渲染出来的 `detail.item-id`（`root.play-item(root.detail.item-id)`）。
/// `play()` 在压栈前就按路由纠正 id，所以回调拿到的已经是纠正过的值——
/// `nova-app` 不再自己按路由重算（那里的校验是循环的，等于没有防线）。
#[test]
fn play_uses_the_detail_route_item_when_the_view_lags_behind() {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    // 界面数据还停在 A（迟到的详情刚被丢弃、还没重贴）。
    let mut stale = detail_view();
    stale.item_id = "a".to_owned();
    stale.title = "上一部".to_owned();
    app.set_detail(&stale);
    app.open_detail("b");

    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));

    // 点的是渲染数据里的 A。
    assert!(app.play("a"));
    assert_eq!(
        played.borrow().as_slice(),
        ["b"],
        "回调必须拿到路由里的 B（已经被纠正过）"
    );
    assert_eq!(
        app.current_route(),
        Some(Route::Player {
            item_id: "b".to_owned()
        })
    );
    assert_eq!(app.page(), Page::Player);
}

/// 已经在播放页 A 时再播 B：替换栈顶，不叠一层（返回仍然只回一层）。
#[test]
fn play_replaces_the_player_route_instead_of_stacking() {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));

    assert!(app.play("a"));
    let depth = app.navigator().len();
    assert!(app.play("b"));
    assert_eq!(app.navigator().len(), depth, "换片不能多叠一层播放页");
    assert_eq!(
        app.current_route(),
        Some(Route::Player {
            item_id: "b".to_owned()
        })
    );
    assert_eq!(played.borrow().as_slice(), ["a", "b"]);
}

/// 换片后按返回：回到的是**新片**的详情页，不是上一部的。
///
/// 栈是 `[Home, Detail{A}, Player{A}]` 时起播 B，以前只把 `Player{A}` 换成
/// `Player{B}`，`Detail{A}` 留在下面——返回看到的是 A 的标题 / 简介 / 播放按钮，
/// 与「从 B 的详情页起播」的语义不符。
#[test]
fn switching_items_repoints_the_detail_page_below_the_player() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));

    assert!(app.play("arrival"));
    assert_eq!(
        app.navigator().stack(),
        &[
            Route::Home,
            Route::Detail {
                item_id: "arrival".to_owned()
            },
            Route::Player {
                item_id: "arrival".to_owned()
            },
        ],
        "换片后栈顶下面是 B 的详情页，且不多叠一层"
    );
    assert_eq!(played.borrow().as_slice(), ["arrival"], "换片要起播新片");

    assert!(app.go_back());
    assert_eq!(
        app.current_route(),
        Some(Route::Detail {
            item_id: "arrival".to_owned()
        }),
        "从播放页返回必须回到新片的详情页"
    );
    assert_eq!(app.page(), Page::Detail);
}

/// 播放页再按 Enter（`play_current` → `play`）不能重开同一部片。
///
/// 以前会把同一个 `Player{A}` 弹了再压一次：播放页闪一下、重新取直链、重开 mpv。
#[test]
fn playing_the_item_already_on_screen_changes_nothing() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));
    let before = app.navigator();

    assert!(app.play("dune"), "同一个条目也算处理掉了");

    assert!(
        played.borrow().is_empty(),
        "不能回调起播：那会重新取直链、重开 mpv：{:?}",
        played.borrow()
    );
    assert_eq!(
        app.navigator(),
        before,
        "栈不能变（弹了再压会让页面闪一次）"
    );
    assert_eq!(app.page(), Page::Player);
}

/// 片子自然播完（`Ended`）后再按 Enter：必须重新起播。
///
/// 回归：`play()` 只按“路由里的条目等于当前条目”就提前返回，播完的片子
/// 按 Enter 一点反应也没有，用户只能先退出播放页再进来。
/// “播完”直接读 `PlayerView::ended`（由 `SessionState::Ended` 直传），
/// 不再用 `position >= duration` 推。
#[test]
fn ended_playback_restarts_the_same_item() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));

    let finished = player_view().with_ended(true).with_playing(false);
    app.set_player(&finished);
    assert!(app.player_view().ended, "用例前提：视图已标记播完");

    assert!(app.play("dune"), "重播也算处理掉了");
    assert_eq!(
        played.borrow().as_slice(),
        ["dune"],
        "播完之后按 Enter 必须重新起播（不能走提前返回）"
    );
    assert_eq!(
        app.current_route(),
        Some(Route::Player {
            item_id: "dune".to_owned()
        }),
        "重播不能多叠一层播放页"
    );
}

/// 起播失败停在错误卡上时再按 Enter：必须重新起播（错误卡上的「重试」同理）。
#[test]
fn error_state_playback_replays_the_same_item() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));

    app.set_player(&player_error());

    assert!(app.play("dune"), "重播也算处理掉了");
    assert_eq!(
        played.borrow().as_slice(),
        ["dune"],
        "错误态下按 Enter 必须重新起播（不能走提前返回）"
    );
}

/// 时长未知的片源播完（`Ended`）后再按 Enter：同样必须重新起播。
///
/// 回归（复审 🟡1）：旧实现用 `position >= duration` 判“播完”，`duration` 为
/// `None` 时永远返回 false（`Session::handle(Ended)` 里 `if let Some(total)` 才推
/// 位置），于是播完按 Enter 一点反应也没有。
#[test]
fn ended_playback_without_known_duration_restarts_on_enter() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));

    // 时长未知：控制层退化为不确定态，`position` 停在最后读到的地方。
    let mut finished = player_view().with_ended(true).with_playing(false);
    finished.duration = None;
    app.set_player(&finished);
    assert!(app.player_view().duration.is_none(), "用例前提：时长未知");

    assert!(app.play("dune"), "重播也算处理掉了");
    assert_eq!(
        played.borrow().as_slice(),
        ["dune"],
        "时长未知时播完按 Enter 也必须重新起播"
    );
}

/// 续播点贴着片尾、会话还在 `Opening`（位置已 >= 时长）：按 Enter 不能重开。
///
/// 回归（复审 🟡2）：旧实现用 `position >= duration` 判“播完”，于是从续播点
/// 起播的首帧（甚至缓冲中）就被当成已结束，Enter 把片子从头重开。
#[test]
fn near_end_position_while_opening_does_not_restart_on_enter() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));
    let before = app.navigator();

    // 续播点在某些片子里会保留在片尾之后（服务端钳位差异），位置因此 >= 时长。
    let mut opening = player_view().with_playing(true).with_ended(false);
    let total = opening.duration.unwrap_or_default();
    opening.position = total + Duration::from_secs(1);
    app.set_player(&opening);

    assert!(app.play("dune"), "同一个条目也算处理掉了");
    assert!(
        played.borrow().is_empty(),
        "起播中（Opening）不能当成播完重开：{:?}",
        played.borrow()
    );
    assert_eq!(app.navigator(), before, "重开会让播放页弹了再压");
}

/// 正常播放中（未播完）按 Enter：不能重开。
///
/// 含缓冲态：缓冲首帧的位置也可能已经跨过时长，旧实现会把这种情况判成播完。
#[test]
fn normal_playback_does_not_restart_on_enter() {
    let app = player_app();
    let played = Rc::new(RefCell::new(Vec::new()));
    let sink = played.clone();
    app.on_play(move |id| sink.borrow_mut().push(id.to_owned()));
    let before = app.navigator();

    // 播到一半。
    let playing = player_view().with_playing(true).with_ended(false);
    app.set_player(&playing);
    assert!(app.play("dune"), "同一个条目也算处理掉了");
    assert!(
        played.borrow().is_empty(),
        "正常播放中按 Enter 不能重开：{:?}",
        played.borrow()
    );

    // 缓冲中（画面保留，算播放中）：位置已跨过时长，也不能当播完。
    let mut buffering = player_view()
        .with_buffering(true)
        .with_playing(true)
        .with_ended(false);
    buffering.position = buffering.duration.unwrap_or_default() + Duration::from_secs(1);
    app.set_player(&buffering);
    assert!(app.play("dune"), "同一个条目也算处理掉了");
    assert!(
        played.borrow().is_empty(),
        "缓冲中按 Enter 不能重开：{:?}",
        played.borrow()
    );
    assert_eq!(app.navigator(), before, "栈不能变");
}
