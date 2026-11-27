// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 无头快照：用 Slint 的 testing backend + 内嵌软件光栅器渲染一张 PNG。
//!
//! 这条路**不需要合成器**：testing backend 不会等待窗口系统的帧回调，因此在
//! 屏幕锁定、没有 Wayland/X11 会话（甚至 CI）里也能出图。窗口模式仍然走
//! winit + femtovg，两者只在 `main` 里根据 `--snapshot` 分叉。
//!
//! 快照流程与真实窗口共用同一套视图构建与图片装载逻辑，区别只有：
//! 没有事件循环，所以图片在主线程上 `block_on` 等待，而不是 `invoke_from_event_loop`。

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use i_slint_core::graphics::{Rgba8Pixel, SharedPixelBuffer};
use nova_ui::{App, AppWindow, ImageKey, Page};
use slint::ComponentHandle;
use tokio::runtime::Runtime;

use crate::args::{Cli, RouteArg};
use crate::error::AppError;
use crate::images::{ImageService, ProviderFetcher, image_from_decoded};
use crate::local::Local;
use crate::refresh;
use crate::status::STATUS_ITEM_ID;
use crate::views;

/// 快照尺寸：与 `.slint` 里的默认窗口一致，便于与 `target/snapshots/` 对照。
pub const SNAPSHOT_WIDTH: f32 = 1280.0;
/// 快照高度。
pub const SNAPSHOT_HEIGHT: f32 = 800.0;

/// 初始化 testing backend（每个进程只能初始化一次）。
pub fn init_testing_backend() {
    let backend = i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            mock_time: true,
            threading: false,
            renderer_name: Some("software".into()),
        },
    );
    if let Err(error) = slint::platform::set_platform(Box::new(backend)) {
        // 已经初始化过（例如同一进程里另一个测试）就沿用现有后端。
        tracing::debug!(error = ?error, "Slint 平台已初始化，沿用现有后端");
    }
}

/// 渲染一张快照并写盘，返回实际写入的绝对路径。
pub fn run_snapshot(
    local: Arc<Local>,
    cli: &Cli,
    path: &Path,
    runtime: &Runtime,
) -> Result<PathBuf, AppError> {
    init_testing_backend();

    let mut status = local.status();
    if cli.should_refresh() && status.is_connected() {
        let report = runtime.block_on(refresh::refresh(Arc::clone(&local), status.clone()));
        status = report.status;
        local.set_status(status.clone());
    }

    let search = cli.search_term();
    let home = local.home(search)?;
    let library = match cli.route {
        RouteArg::Library => {
            let id = local
                .tabs()
                .first()
                .map(|tab| tab.id.clone())
                .unwrap_or_default();
            Some(local.library(&id, search, views::LibrarySort::default())?)
        }
        _ => None,
    };
    let detail = match cli.route {
        RouteArg::Detail => {
            let item_id = cli
                .item
                .clone()
                .or_else(|| first_item_id(&home))
                .unwrap_or_default();
            local.detail(&item_id)?
        }
        _ => None,
    };
    let player = match cli.route {
        RouteArg::Player => {
            let item_id = cli
                .item
                .clone()
                .or_else(|| first_item_id(&home))
                .unwrap_or_default();
            local.player(&item_id)?
        }
        _ => None,
    };

    // 先把图片装好再建模型：`nova_ui::pages::convert` 在 `set_*` 时取图，
    // 晚到的 `set_image` 不会自动刷新已生成的模型。
    let mut keys: Vec<ImageKey> = home.image_keys();
    if let Some(library) = &library {
        keys.extend(library.image_keys());
    }
    if let Some(detail) = &detail {
        keys.extend(detail.image_keys());
    }
    keys.sort_by(|a, b| (&a.item_id, kind_rank(a)).cmp(&(&b.item_id, kind_rank(b))));
    keys.dedup();

    let ui = AppWindow::new()?;
    let app = App::new_with(ui);
    app.ui()
        .window()
        .set_size(slint::LogicalSize::new(SNAPSHOT_WIDTH, SNAPSHOT_HEIGHT));

    load_images(&local, &keys, runtime, |key, image| {
        app.set_image(&key, image);
    });

    app.set_home(&home);
    if let Some(library) = &library {
        app.set_library(library);
    }
    if let Some(detail) = &detail {
        app.set_detail(detail);
    }
    if let Some(player) = &player {
        app.set_player(player);
    }

    let page = match cli.route {
        RouteArg::Home => Page::Home,
        RouteArg::Library => Page::Library,
        RouteArg::Detail => Page::Detail,
        RouteArg::Player => Page::Player,
    };
    app.show_page(page);

    let snapshot: SharedPixelBuffer<Rgba8Pixel> = app.ui().window().take_snapshot()?;
    let absolute = absolute_path(path)?;
    write_png(&absolute, &snapshot)?;
    println!(
        "snapshot: {}x{} -> {}",
        snapshot.width(),
        snapshot.height(),
        absolute.display()
    );
    Ok(absolute)
}

/// 当前视图需要的第一张真实条目 id（跳过状态卡）。
fn first_item_id(home: &nova_ui::HomeView) -> Option<String> {
    home.hero
        .iter()
        .chain(home.continue_watching.iter())
        .chain(home.recently_added.iter())
        .find(|card| card.item_id != STATUS_ITEM_ID)
        .map(|card| card.item_id.clone())
}

fn kind_rank(key: &ImageKey) -> u8 {
    match key.kind {
        nova_ui::ImageKind::Poster => 0,
        nova_ui::ImageKind::Backdrop => 1,
        nova_ui::ImageKind::Logo => 2,
    }
}

/// 在快照模式下同步装载图片：`block_on` 一个受限并发的 `JoinSet`。
fn load_images<F>(local: &Arc<Local>, keys: &[ImageKey], runtime: &Runtime, mut apply: F)
where
    F: FnMut(ImageKey, slint::Image),
{
    let specs: Vec<_> = keys
        .iter()
        .filter_map(|key| local.image_spec(key))
        .collect();
    if specs.is_empty() {
        return;
    }
    let service = Arc::new(ImageService::new(
        Arc::clone(&local.image_cache),
        Arc::new(ProviderFetcher::from_local(Arc::clone(local))),
    ));

    let loaded = runtime.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for spec in specs {
            let service = Arc::clone(&service);
            set.spawn(async move { service.load(&spec).await });
        }
        let mut loaded = Vec::new();
        while let Some(result) = set.join_next().await {
            match result {
                Ok(Some((key, decoded))) => loaded.push((key, decoded)),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "图片加载任务失败"),
            }
        }
        loaded
    });

    for (key, decoded) in loaded {
        if let Some(image) = image_from_decoded(&decoded) {
            apply(key, image);
        }
    }
}

/// 相对路径转绝对路径（父目录不存在时先按当前目录解析）。
fn absolute_path(path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

/// 把像素缓冲写成 PNG。
fn write_png(path: &Path, buffer: &SharedPixelBuffer<Rgba8Pixel>) -> Result<(), AppError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let to_snapshot_error = |source: std::io::Error| AppError::Snapshot {
        path: path.display().to_string(),
        source,
    };
    let file = std::fs::File::create(path).map_err(to_snapshot_error)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, buffer.width(), buffer.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| to_snapshot_error(std::io::Error::other(error)))?;
    let mut bytes = Vec::with_capacity(buffer.as_slice().len() * 4);
    for pixel in buffer.as_slice() {
        bytes.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
    }
    writer
        .write_image_data(&bytes)
        .map_err(|error| to_snapshot_error(std::io::Error::other(error)))?;
    Ok(())
}
