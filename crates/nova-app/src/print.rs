// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `--print-view`：不建窗口，把当前首页摘要打到 stdout。
//!
//! 默认只读本地缓存（不联网），这样脚本与 CI 拿到的是确定性输出；加 `--refresh`
//! 才会先做一次有界刷新。输出格式见 [`crate::views::summary`]。

use std::sync::Arc;

use tokio::runtime::Runtime;

use crate::args::Cli;
use crate::error::AppError;
use crate::local::Local;
use crate::refresh;
use crate::views;

/// 执行 `--print-view`。
pub fn run_print_view(local: &Arc<Local>, cli: &Cli, runtime: &Runtime) -> Result<(), AppError> {
    let mut status = local.status();
    if cli.should_refresh() && status.is_connected() {
        let report = runtime.block_on(refresh::refresh(Arc::clone(local), status.clone()));
        status = report.status;
        local.set_status(status.clone());
    }
    let home = local.home(cli.search_term())?;
    println!("{}", views::summary(&status, &home));
    Ok(())
}
