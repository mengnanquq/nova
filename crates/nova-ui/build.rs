// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 编译 `ui/*.slint`。Slint 标记在编译期生成 Rust 代码，运行时不需要解释器。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile("ui/app.slint")?;
    Ok(())
}
