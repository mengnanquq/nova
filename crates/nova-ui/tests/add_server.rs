// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! “添加服务器”对话框：无头快照 + 交互测试（testing backend 发真实键 / 鼠标事件）。
//!
//! 覆盖：打开对话框（空态按钮 / Ctrl+N）、Esc 关闭、空地址不可提交、
//! 连接中禁用重复提交、错误文案出现在错误区、http 警示条、明文密码不进日志路径
//! （密码只经回调传出，不进视图模型——由 `add_server_password()` 只在提交瞬间读取保证）。

mod common;

use common::{home_empty, init_backend, snapshots_dir, write_png};
use nova_ui::{AddServerError, AddServerErrorKind, App, AppWindow, KeyCommand, Page};
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use std::cell::RefCell;
use std::rc::Rc;

fn setup() -> App {
    init_backend();
    let app = App::new_with(AppWindow::new().unwrap());
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(1280.0, 800.0));
    app.set_home(&home_empty());
    app.show_page(Page::Home);
    app
}

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

fn press(app: &App, key: Key) {
    let window = app.ui().window();
    let text: slint::SharedString = key.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn press_ctrl(app: &App, ch: char) {
    // testing backend 的 dispatch_event 带不上修饰键；Ctrl+N 走 key-intent 同一路径。
    let _ = ch;
    assert!(app.handle_key(KeyCommand::ConnectServer), "Ctrl+N 应被消费");
}

fn type_text(app: &App, text: &str) {
    let window = app.ui().window();
    for ch in text.chars() {
        let text: slint::SharedString = ch.to_string().into();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }
}

fn click(app: &App, x: f32, y: f32) {
    let window = app.ui().window();
    let position = slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn connect_button_and_ctrl_n_open_the_dialog() {
    let app = setup();

    // 首页空态的「连接服务器」按钮（快照测得的 accent 胶囊位置）。
    let opened = Rc::new(RefCell::new(0));
    {
        let opened = opened.clone();
        app.on_connect_server(move || {
            *opened.borrow_mut() += 1;
        });
    }
    click(&app, 554.0, 472.0);
    assert_eq!(*opened.borrow(), 1, "点击空态按钮应触发 connect-server");

    // Ctrl+N（全局加速键）：真实键盘事件经 .slint 映射为 intent 8，
    // testing backend 无法注入修饰键，测试直接走同一 Rust 入口。
    press_ctrl(&app, 'n');
    assert_eq!(*opened.borrow(), 2, "Ctrl+N 应触发 connect-server");

    // 上层（nova-app）调 open_add_server 后对话框可见、焦点在地址输入框。
    app.open_add_server("", "");
    assert!(app.add_server_open());
}

#[test]
fn escape_and_cancel_close_the_dialog() {
    let app = setup();
    app.open_add_server("", "");
    assert!(app.add_server_open());

    // Esc：焦点在地址输入框里，Esc 由输入框转出关框。
    type_text(&app, "http://a");
    press(&app, Key::Escape);
    assert!(!app.add_server_open(), "Esc 应关闭对话框");
    // 关框即清密码；焦点还给全局键盘（后续全局快捷键可用）。
    assert!(app.add_server_password().is_empty());
    assert!(
        app.handle_key(KeyCommand::Search),
        "关框后键盘应恢复全局处理"
    );
}

#[test]
fn empty_address_cannot_submit_and_connecting_disables_resubmit() {
    let app = setup();
    let submits = Rc::new(RefCell::new(Vec::<String>::new()));
    {
        let submits = submits.clone();
        app.on_add_server_submit(move |address, _user, _password| {
            submits.borrow_mut().push(address.to_string());
        });
    }

    app.open_add_server("", "");
    // 空地址：连接按钮禁用（快照里它是半透明的），点击不应提交。
    click(&app, 702.0, 566.0);
    assert!(submits.borrow().is_empty(), "空地址不可提交");

    // 输入合法地址后提交一次（按钮位置由快照测得：y 543-590，x 667-737）。
    type_text(&app, "http://192.168.1.5:8096");
    click(&app, 702.0, 566.0);
    assert_eq!(
        submits.borrow().as_slice(),
        ["http://192.168.1.5:8096"],
        "合法地址应能提交"
    );

    // 连接中：再次点击不再提交（状态机门控）。
    app.begin_add_server_connecting();
    click(&app, 702.0, 566.0);
    assert_eq!(submits.borrow().len(), 1, "连接中必须禁用重复提交");
    app.close_add_server();
}

#[test]
fn error_lands_in_the_error_area() {
    let app = setup();
    app.open_add_server("https://media.example.com", "me");
    app.set_add_server_error(AddServerError::with_detail(
        AddServerErrorKind::Auth,
        "服务器拒绝了这次登录",
    ));
    assert_eq!(
        app.ui().get_add_server_error(),
        "用户名或密码不对",
        "401 → 用户名或密码不对"
    );
    assert_eq!(
        app.ui().get_add_server_error_detail(),
        "服务器拒绝了这次登录"
    );
    capture(&app, "add-server-error.png");
}

#[test]
fn plaintext_warning_tracks_http_only() {
    let app = setup();
    app.open_add_server("http://192.168.1.5:8096", "me");
    assert!(app.ui().get_add_server_plaintext(), "http 应显示警示条");
    app.close_add_server();
    app.open_add_server("https://192.168.1.5:8096", "me");
    assert!(!app.ui().get_add_server_plaintext(), "https 不显示警示条");
}

#[test]
fn address_hint_appears_live_while_typing() {
    let app = setup();
    app.open_add_server("", "");
    type_text(&app, "192.168.1.5:8096");
    assert_eq!(
        app.ui().get_add_server_hint(),
        "需要 http:// 或 https:// 开头",
        "缺前缀应有内联提示"
    );
    // 补上协议后提示消失。
    let backspace: slint::SharedString = Key::Backspace.into();
    let window = app.ui().window();
    for _ in 0.."192.168.1.5:8096".chars().count() {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: backspace.clone(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: backspace.clone(),
        });
    }
    type_text(&app, "http://192.168.1.5:8096");
    assert_eq!(app.ui().get_add_server_hint(), "", "合法地址不应再有提示");
}

#[test]
fn dialog_snapshots() {
    let app = setup();

    // 1. 空表单
    app.open_add_server("", "");
    capture(&app, "add-server-empty.png");

    // 2. 错误态（错误区在表单下方）
    app.set_add_server_error(AddServerError::with_detail(
        AddServerErrorKind::Connect,
        "请求超时",
    ));
    capture(&app, "add-server-error.png");
    app.close_add_server();

    // 3. http 明文警示条（不阻止提交）
    app.open_add_server("http://192.168.1.5:8096", "me");
    assert!(app.ui().get_add_server_plaintext());
    capture(&app, "add-server-plaintext-warning.png");

    // 关闭后首页完全恢复（覆盖层隐藏，零残留）。
    app.close_add_server();
    assert!(!app.add_server_open());
}
