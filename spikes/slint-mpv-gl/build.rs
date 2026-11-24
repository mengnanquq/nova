// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI 编译失败（ui/app.slint）");
}
