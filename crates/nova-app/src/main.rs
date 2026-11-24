// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova-desktop` 入口：初始化日志、解析参数、把错误翻译成人话。
//!
//! UI、网络、数据库的装配都在 `nova_app` 库里；这里只做进程级的事。

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

/// 日志默认只到 `warn`：正常启动不该被图片/刷新日志淹没。
fn init_tracing() {
    let filter = EnvFilter::try_from_env("NOVA_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    // 已经装过 subscriber（测试里）时忽略错误：不是致命问题。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

fn main() -> ExitCode {
    init_tracing();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match nova_app::run(argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error}");
            if let Some(hint) = error.hint() {
                eprintln!("下一步：{hint}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}
