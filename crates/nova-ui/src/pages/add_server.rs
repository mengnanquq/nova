// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! “添加服务器”对话框绑定层：视图模型 → Slint 属性。

use crate::AppWindow;
use crate::viewmodel::{AddServerState, AddServerView};

/// 把对话框视图写进 Slint。
pub fn apply(ui: &AppWindow, view: &AddServerView) {
    ui.set_add_server_open(view.open);
    ui.set_add_server_state(match view.state {
        AddServerState::Idle => 0,
        AddServerState::Connecting => 1,
    });
    ui.set_add_server_address(view.address.clone().into());
    ui.set_add_server_user(view.user.clone().into());
    ui.set_add_server_hint(view.address_hint().unwrap_or_default().into());
    ui.set_add_server_plaintext(view.is_plaintext());
    let error = view.error.as_ref();
    ui.set_add_server_error(
        error
            .map(|error| error.message().to_owned())
            .unwrap_or_default()
            .into(),
    );
    ui.set_add_server_error_detail(
        error
            .and_then(|error| error.detail.clone())
            .unwrap_or_default()
            .into(),
    );
    ui.set_add_server_note(view.note.clone().unwrap_or_default().into());
}

/// 只刷新派生属性（地址 / 错误变化后的轻量更新，不碰 open / state）。
pub fn apply_derived(ui: &AppWindow, view: &AddServerView) {
    ui.set_add_server_hint(view.address_hint().unwrap_or_default().into());
    ui.set_add_server_plaintext(view.is_plaintext());
    let error = view.error.as_ref();
    ui.set_add_server_error(
        error
            .map(|error| error.message().to_owned())
            .unwrap_or_default()
            .into(),
    );
    ui.set_add_server_error_detail(
        error
            .and_then(|error| error.detail.clone())
            .unwrap_or_default()
            .into(),
    );
    ui.set_add_server_state(match view.state {
        AddServerState::Idle => 0,
        AddServerState::Connecting => 1,
    });
}
