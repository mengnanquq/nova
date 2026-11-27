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
    // 推过对话框入场动画（`Theme.motion-page`；无头后端不自行走时间）。
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1000));
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

/// 刷新两轮目标清单（调试通道与 `sweep` 读它之前的同一步）。
fn settle(app: &App) {
    for _ in 0..2 {
        app.refresh_targets();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(80));
    }
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
fn cancel_button_closes_the_dialog_and_clears_password() {
    let app = setup();
    app.open_add_server("http://127.0.0.1:8096", "me");
    app.ui().set_add_server_password("secret".into());
    app.ui().show().unwrap();
    settle(&app);

    // 真点「取消」：对话框必须真的关掉（不是只有 Esc 能关），密码用完即弃。
    let cancel = app.target("dialog.cancel").expect("取消按钮应在清单里");
    let (x, y) = cancel.center();
    click(&app, x, y);

    assert!(!app.add_server_open(), "点「取消」应关闭对话框");
    assert_eq!(app.add_server_password(), "", "关框必须清空密码");

    // 关框后目标清单里不能再出现 dialog.*。这里**不等**下一次刷新：登记总线上
    // 还留着关框前的 5 条快照，`targets()` 必须靠 `add-server-open` 把它们过滤掉
    // （以前关框后仍会返回 5 个点到不存在的浮层上）。
    assert!(
        app.target("dialog.cancel").is_none(),
        "关框后（同一帧）目标清单不应再有 dialog.cancel"
    );
    settle(&app);
    for id in [
        "dialog.address",
        "dialog.user",
        "dialog.password",
        "dialog.cancel",
        "dialog.connect",
    ] {
        assert!(
            app.target(id).is_none(),
            "对话框已关闭，目标清单不应再有 {id}"
        );
    }
}

#[test]
fn disabled_connect_is_not_listed_as_a_target() {
    let app = setup();
    app.ui().show().unwrap();

    // 空地址：「连接」是禁用态（TouchArea `enabled: false`），不能作为可点目标
    // 进清单，否则 `sweep` 会对着点不动的按钮报假 dead。
    app.open_add_server("", "");
    settle(&app);
    assert!(
        app.target("dialog.connect").is_none(),
        "空地址时「连接」被禁用，不该出现在目标清单里"
    );
    assert!(app.target("dialog.cancel").is_some());

    // 合法地址：按钮可用，重新进清单。
    app.open_add_server("http://127.0.0.1:8096", "me");
    settle(&app);
    assert!(
        app.target("dialog.connect").is_some(),
        "地址合法后「连接」应重新成为可点目标"
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

    // 用户名先填上：空用户名的本地校验见 `empty_user_is_rejected_locally_without_a_request`。
    app.open_add_server("", "me");
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

/// Enter 是绕过「连接」按钮的第二条提交路径（`.slint` 里 `accepted => root.submit()`）。
///
/// 体检 R2-6 怀疑“空地址点「连接」会发起真实请求”：实际两道门都在——空地址时按钮
/// 是禁用态（不进目标清单），Enter 也会被 `can_submit()` 拦住（本用例锁住第二条）。
#[test]
fn enter_does_not_submit_an_empty_or_invalid_address() {
    let app = setup();
    let submits = Rc::new(RefCell::new(Vec::<String>::new()));
    {
        let submits = submits.clone();
        app.on_add_server_submit(move |address, _user, _password| {
            submits.borrow_mut().push(address.to_string());
        });
    }

    // 空地址 + Enter：焦点在地址输入框里，Enter 走 `accepted` → 门控拦下。
    app.open_add_server("", "");
    press(&app, Key::Return);
    assert!(submits.borrow().is_empty(), "空地址按 Enter 不能提交");

    // 地址不合规（缺协议）+ Enter：同样不能提交。
    app.open_add_server("192.168.1.5:8096", "me");
    press(&app, Key::Return);
    assert!(submits.borrow().is_empty(), "非法地址按 Enter 不能提交");

    // 合法地址 + Enter：正常提交。
    app.open_add_server("http://127.0.0.1:8096", "me");
    press(&app, Key::Return);
    assert_eq!(
        submits.borrow().as_slice(),
        ["http://127.0.0.1:8096"],
        "合法地址应当能用 Enter 提交"
    );
    app.close_add_server();
}

/// 体检 R8-C4：用户名为空时点「连接」不能联网，错误区给出本地文案。
///
/// 修复前空用户名会一路走到 `submit_add_server`：后台真的发一次登录请求，
/// 服务端回 401，日志里多一条「用户名或密码不对」——用户看到的却只是"没连上"。
#[test]
fn empty_user_is_rejected_locally_without_a_request() {
    let app = setup();
    let submits = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    {
        let submits = submits.clone();
        app.on_add_server_submit(move |address, user, _password| {
            submits
                .borrow_mut()
                .push((address.to_string(), user.to_string()));
        });
    }

    // 「最近使用」只回填了地址、用户名是空的（首次连接/清空过用户名都是这个状态）。
    app.open_add_server("http://127.0.0.1:8096", "");
    app.ui().show().expect("显示窗口");
    settle(&app);
    let connect = app
        .target("dialog.connect")
        .expect("地址合法时「连接」是可点目标");
    let (x, y) = connect.center();
    click(&app, x, y);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(80));

    assert!(
        submits.borrow().is_empty(),
        "用户名为空时不能发出任何登录请求"
    );
    assert_eq!(
        app.ui().get_add_server_error(),
        "请填写用户名",
        "本地校验要在错误区给出文案"
    );
    assert_eq!(
        app.ui().get_add_server_state(),
        0,
        "本地校验不应进入「连接中」"
    );

    // 补上用户名再点：正常提交一次（错误区被 `begin_connecting` 清掉）。
    app.ui().set_add_server_user("me".into());
    settle(&app);
    let connect = app.target("dialog.connect").expect("「连接」仍在");
    let (x, y) = connect.center();
    click(&app, x, y);
    assert_eq!(
        submits.borrow().as_slice(),
        [("http://127.0.0.1:8096".to_owned(), "me".to_owned())],
        "补上用户名后应正常提交一次"
    );
}

/// 用户名一变就清掉本地拦截的「请填写用户名」。
///
/// 修复前这条错误只在**下一次提交**时被覆盖：用户明明补上了名字，提示还挂着，
/// 看起来像“还是填错了”。只清自己那条本地错误，认证 / 网络错误不随输入消失。
#[test]
fn typing_a_user_name_clears_the_missing_user_error() {
    let app = setup();
    app.open_add_server("http://127.0.0.1:8096", "");
    app.ui().show().expect("显示窗口");
    settle(&app);

    let connect = app
        .target("dialog.connect")
        .expect("地址合法时「连接」是可点目标");
    let (x, y) = connect.center();
    click(&app, x, y);
    assert_eq!(
        app.ui().get_add_server_error(),
        "请填写用户名",
        "空用户名点「连接」应出现本地提示"
    );

    // 点用户名输入框再真敲字（走 `edited` 回调，而不是直接改视图模型）。
    // 错误区出现时对话框会长高，先让目标清单落下新坐标再点。
    settle(&app);
    let user = app.target("dialog.user").expect("用户名输入框");
    let (x, y) = user.center();
    click(&app, x, y);
    settle(&app);
    type_text(&app, "me");

    assert_eq!(app.ui().get_add_server_user(), "me", "用户名已填上");
    assert_eq!(
        app.ui().get_add_server_error(),
        "",
        "补上用户名后「请填写用户名」必须立刻消失"
    );
}

/// Emby / Jellyfin 允许空密码账号：密码留空也必须能提交。
///
/// 回归：曾经担心“空密码 = 没填完”而想加门控；实际服务端允许空密码登录，
/// 本地拦下只会让这类账号永远连不上。
#[test]
fn empty_password_still_submits() {
    let app = setup();
    let submits = Rc::new(RefCell::new(Vec::<(String, String, String)>::new()));
    {
        let submits = submits.clone();
        app.on_add_server_submit(move |address, user, password| {
            submits.borrow_mut().push((
                address.to_string(),
                user.to_string(),
                password.to_string(),
            ));
        });
    }

    app.open_add_server("http://127.0.0.1:8096", "me");
    app.ui().show().expect("显示窗口");
    settle(&app);
    assert_eq!(app.add_server_password(), "", "前提：密码框是空的");

    let connect = app.target("dialog.connect").expect("「连接」可点");
    let (x, y) = connect.center();
    click(&app, x, y);

    assert_eq!(
        submits.borrow().as_slice(),
        [(
            "http://127.0.0.1:8096".to_owned(),
            "me".to_owned(),
            String::new()
        )],
        "空密码也要发出连接请求（服务端允许空密码账号）"
    );
}

#[test]
fn every_error_kind_lands_in_the_error_area() {
    let app = setup();
    app.open_add_server("https://media.example.com", "me");

    // 四类错误→主文案 / 补充说明，两行都进错误区（`error-text` / `error-detail`）。
    for (kind, expected) in [
        (AddServerErrorKind::Auth, "用户名或密码不对"),
        (
            AddServerErrorKind::Connect,
            "连不上服务器：检查地址、端口和网络",
        ),
        (
            AddServerErrorKind::Tls,
            "证书校验失败：自签名证书需要导入系统信任，不能跳过校验",
        ),
        (
            AddServerErrorKind::UnknownServer,
            "这个地址看起来不像 Emby 或 Jellyfin",
        ),
    ] {
        app.set_add_server_error(AddServerError::with_detail(kind, "补充说明"));
        assert_eq!(
            app.ui().get_add_server_error(),
            expected,
            "{kind:?} 的主文案"
        );
        assert_eq!(
            app.ui().get_add_server_error_detail(),
            "补充说明",
            "{kind:?} 的补充说明也应可见"
        );
    }

    // 地址不合法不是“连接失败”：走地址框下的内联红字，且同时让「连接」不可提交。
    app.open_add_server("192.168.1.5:8096", "me");
    assert_eq!(
        app.ui().get_add_server_hint(),
        "需要 http:// 或 https:// 开头"
    );
    settle(&app);
    assert!(
        app.target("dialog.connect").is_none(),
        "非法地址时「连接」必须是禁用态"
    );
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
