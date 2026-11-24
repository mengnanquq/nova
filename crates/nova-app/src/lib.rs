// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 桌面应用（二进制名 `nova-desktop`）：四层装配。
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────────┐
//! │ nova-app                                                           │
//! │  Local（SQLite + 凭证 + provider + 图片缓存）                       │
//! │    ├─ nova-storage   nova.db（与 CLI 共用）                        │
//! │    ├─ nova-secrets   KeyringStore / MemoryStore                    │
//! │    ├─ nova-provider  libraries / items / item / sources / image    │
//! │    ├─ nova-images    ImageCache（<data>/images，512 MiB 预算）      │
//! │    └─ views          nova-core 模型 → nova-ui 视图模型              │
//! │  ui / snapshot / print ──> nova-ui（Slint 标记 + 视图模型）         │
//! └────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # 启动与数据流
//!
//! 1. `Local::open` 打开（必要时创建）数据目录、`nova.db`、`<data>/images`，
//!    读一次偏好并尽力从凭证存储里恢复会话；
//! 2. **先渲染本地缓存**：`views` 只读 SQLite，立刻得到首页/媒体库/详情；
//! 3. 后台刷新（tokio）：`provider.libraries()` → 每库 `provider.items()` 前
//!    [`refresh::ITEMS_PER_LIBRARY`] 条 → `db.upsert_items` → 重建视图 → 推给界面；
//! 4. 图片另走 `ImageCache::get_or_fetch`（同时最多 4 张），解码完成后按
//!    `ImageKey` 推入界面，失败的图不重试。
//!
//! # 线程模型
//!
//! Slint 的窗口/组件只能在主线程创建与修改；数据库与网络在 tokio runtime 上。
//! 窗口模式下后台任务把结果打包成 `ui::UiMessage`，通过 `slint::invoke_from_event_loop`
//! 交回主线程（闭包只捕获 `Send` 的队列与弱窗口句柄，主线程再用 thread-local
//! 找到 `Rc<Shell>`）。快照模式没有事件循环，直接在 `block_on` 之后于主线程应用。
//! 详见 `ui` 与 `snapshot` 的模块文档。
//!
//! # 降级
//!
//! 没有服务器、令牌缺失、令牌过期、刷新失败都不会 panic，也不会清空缓存：
//! `nova-ui` 没有专门的横幅属性，所以状态被渲染成首页 hero 位上的"状态卡"
//! （详情见 [`status`]）；缓存里的继续观看/最近添加照常可浏览。
//!
//! # 可验证模式
//!
//! - `nova-desktop`：真实窗口；
//! - `nova-desktop --snapshot <path.png>`：testing backend + 软件光栅器，无合成器也能出图；
//! - `nova-desktop --print-view`：不建窗口，打印首页摘要（脚本/CI 用）。

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod args;
pub mod connect;
pub mod env;
pub mod error;
pub mod images;
pub mod local;
pub mod player;
pub mod print;
pub mod refresh;
pub mod snapshot;
pub mod status;
pub mod ui;
pub mod views;

use std::sync::Arc;

pub use args::{Cli, Mode, PlayArgs, RouteArg};
pub use error::AppError;
pub use local::Local;
pub use status::Status;

/// 按命令行参数启动。
pub fn run(argv: Vec<String>) -> Result<(), AppError> {
    let cli = Cli::parse(&argv)?;
    match cli.mode.clone() {
        Mode::Help => {
            print!("{}", args::USAGE);
            Ok(())
        }
        Mode::Version => {
            println!("nova-desktop {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        mode @ (Mode::PrintView | Mode::Snapshot(_) | Mode::Window | Mode::Play(_)) => {
            let environment = env::Env::from_process()?;
            let local = Arc::new(Local::open(&environment, cli.server.as_deref())?);
            let runtime = build_runtime()?;
            match mode {
                Mode::Play(play) => {
                    let start = player::StartPlay {
                        item_id: play.item.clone(),
                        source_id: play.source.clone(),
                        capture: play.capture.clone().map(|path| {
                            player::CaptureOptions::new(path, play.seconds).with_max_first_frame(
                                std::time::Duration::from_secs_f64(play.max_first_frame),
                            )
                        }),
                    };
                    ui::run_window(local, runtime.handle().clone(), Some(start))
                }
                Mode::PrintView => print::run_print_view(&local, &cli, &runtime),
                Mode::Snapshot(path) => {
                    snapshot::run_snapshot(local, &cli, &path, &runtime).map(|_| ())
                }
                Mode::Window => ui::run_window(local, runtime.handle().clone(), None),
                // 外层分支已经排除了 Help / Version；这里保留正常返回而不是 panic。
                Mode::Help | Mode::Version => Ok(()),
            }
        }
    }
}

/// 网络 / 数据库 / 图片解码用的 tokio runtime（多线程）。
pub fn build_runtime() -> Result<tokio::runtime::Runtime, AppError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nova-net")
        .build()
        .map_err(AppError::Runtime)
}
