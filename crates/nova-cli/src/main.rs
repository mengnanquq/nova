// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `nova` 可执行文件入口。
//!
//! 这里只做三件事：初始化 `tracing`、把参数交给 `nova_cli::run`、把错误翻译成
//! "错误 + 下一步"的人话并决定退出码。

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

/// 日志默认只到 `warn`：命令行的正常输出不该被 provider 的 debug 日志淹没。
fn init_tracing() {
    let filter = EnvFilter::try_from_env("NOVA_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    // 已经装过 subscriber（测试里）时忽略错误：不是致命问题。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let argv: Vec<String> = std::env::args().skip(1).collect();
    match nova_cli::run(argv).await {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("错误：{error}");
            if let Some(hint) = error.hint() {
                eprintln!("下一步：{hint}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}
