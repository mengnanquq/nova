// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 窗口模式：Slint 主线程 + tokio 后台线程的装配。
//!
//! # 线程模型（这是本模块存在的理由）
//!
//! ```text
//! 主线程（Slint / winit 事件循环）            tokio 工作线程
//! ────────────────────────────────           ─────────────────────────────
//! AppWindow + nova_ui::App（不可 Send）       refresh(): libraries / items / 图片下载
//! UiState（当前视图，RefCell）                db.upsert_items（spawn_blocking）
//! 收到 UiMessage ── set_home/set_image        └─ post(UiMessage) + invoke_from_event_loop
//! ```
//!
//! - Slint 的窗口与组件**只能在主线程**创建和修改，所以 `App` / `AppWindow` 从不跨线程；
//! - 网络、数据库、图片解码都在 tokio runtime 上跑，结果打包成 [`UiMessage`]；
//! - 跨线程更新走 [`slint::invoke_from_event_loop`]：后台线程把消息推进
//!   `Arc<Mutex<VecDeque<_>>>`，再请求事件循环执行一个**不捕获任何非 Send 数据**的
//!   闭包；闭包在主线程上通过 thread-local 找到 `Rc<Shell>` 并应用消息；
//! - 解码后的像素在主线程上才转成 `slint::Image`（纹理属于界面线程）。

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use nova_core::ItemId;
use nova_images::DecodedImage;
use nova_provider::MediaProvider;
use nova_ui::{
    App, AppWindow, DetailView, HomeView, ImageKey, LibraryView, PlayerShellView, Route,
};
use slint::ComponentHandle;
use tokio::runtime::Handle;

use crate::error::AppError;
use crate::images::{ImageService, ProviderFetcher, image_from_decoded};
use crate::local::Local;
use crate::player::{self, Message, Player, Post, StartPlay};
use crate::refresh;
use crate::status::{STATUS_ITEM_ID, Status};

/// 队列上限：界面长期不处理（最小化/锁屏）时不无限积压。
const MAX_QUEUED_MESSAGES: usize = 4096;

thread_local! {
    /// 主线程上的 Shell；只在 UI 线程（包括 `invoke_from_event_loop` 的回调）访问。
    static SHELL: RefCell<Option<Rc<Shell>>> = const { RefCell::new(None) };
}

/// 后台 → 界面的消息。
enum UiMessage {
    /// 一次刷新完成，带上重建后的视图。
    Refreshed(Box<Refreshed>),
    /// 详情页数据（按需从服务端取回后重建）。
    Detail(Box<DetailView>),
    /// 一张图片解码完成。
    Image {
        /// 图片键。
        key: ImageKey,
        /// 解码后的像素。
        decoded: DecodedImage,
    },
    /// 播放链路的异步结果（取直链等）。
    Playback(Message),
}

/// 刷新结果。
struct Refreshed {
    status: Status,
    home: HomeView,
    library: Option<LibraryView>,
}

/// 当前界面持有的视图（用于图片晚到后重新应用模型）。
struct UiState {
    status: Status,
    home: HomeView,
    library: Option<LibraryView>,
    detail: Option<DetailView>,
    player: Option<PlayerShellView>,
}

/// 主线程上的装配体。
struct Shell {
    app: Rc<App>,
    local: Arc<Local>,
    runtime: Handle,
    images: Arc<ImageService>,
    channel: Arc<UiChannel>,
    state: RefCell<UiState>,
    /// 与渲染 notifier 共享的 mpv 引擎状态（所有 GL 调用都在主线程）。
    host: Rc<RefCell<player::Host>>,
    /// 播放页控制器；只在播放页打开时存在。
    player: RefCell<Option<Player>>,
    /// 播放心跳（8ms）：事件泵、会话 tick、帧 watchdog。
    timer: slint::Timer,
    /// `--play` 指定的捕获参数（等 `App::play` 回调到 [`Shell::play`] 时消费）。
    pending_capture: RefCell<Option<player::CaptureOptions>>,
    /// `--play --source` 指定的版本（同上）。
    pending_source: RefCell<Option<String>>,
    /// 捕获结果槽（与 `Player` 共享）。
    capture_out: player::CaptureSlot,
}

/// 跨线程消息通道：队列 + 唤醒事件循环用的弱窗口句柄。
struct UiChannel {
    weak: slint::Weak<AppWindow>,
    queue: Arc<Mutex<VecDeque<UiMessage>>>,
}

impl UiChannel {
    fn new(weak: slint::Weak<AppWindow>) -> Self {
        Self {
            weak,
            queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// 推入一条消息并请求主线程处理。
    ///
    /// 闭包只捕获 `Arc` 与 `slint::Weak`（都是 `Send`），不捕获 `Rc<Shell>`；
    /// 真正的应用发生在主线程的 [`apply_message`] 里。
    fn post(self: &Arc<Self>, message: UiMessage) {
        {
            let mut queue = lock(&self.queue);
            if queue.len() >= MAX_QUEUED_MESSAGES {
                queue.pop_front();
            }
            queue.push_back(message);
        }
        let weak = self.weak.clone();
        let queue = Arc::clone(&self.queue);
        if let Err(error) = slint::invoke_from_event_loop(move || {
            if weak.upgrade().is_none() {
                return;
            }
            drain(&queue);
        }) {
            tracing::debug!(event_loop_error = ?error, "事件循环已退出，丢弃界面更新");
        }
    }
}

/// 打开真实窗口并运行事件循环。
///
/// `start` 非空时（`--play`）直达播放页并起播；其中 `capture` 还会在播够秒数后
/// 回读窗口像素存 PNG，并把验收结论作为返回值（失败是 [`AppError::Playback`]，退出码 3）。
pub fn run_window(
    local: Arc<Local>,
    runtime: Handle,
    start: Option<StartPlay>,
) -> Result<(), AppError> {
    let ui = AppWindow::new()?;
    let app = Rc::new(App::new_with(ui));
    let status = local.status();
    // 离线优先：先用 SQLite 里的缓存建好首页，再启动后台刷新；状态由顶部横幅表达。
    let home = local.home(None)?;
    app.set_home(&home);
    app.set_banner(status.banner_data());

    let online: Option<Arc<dyn MediaProvider>> = local.provider_dyn();
    let images = Arc::new(ImageService::new(
        Arc::clone(&local.image_cache),
        Arc::new(ProviderFetcher::new(online)),
    ));
    let channel = Arc::new(UiChannel::new(app.ui().as_weak()));

    // 引擎与渲染回调：`MpvEngine` 只在 `RenderingSetup` 里创建（那时 GL 上下文 current），
    // 之后所有 GL 调用都发生在同一个线程的 notifier 里。
    let silent = start
        .as_ref()
        .and_then(|start| start.capture.as_ref())
        .is_some();
    let host = Rc::new(RefCell::new(player::Host::new(silent)));
    // 捕获结果的共享槽：事件循环结束后据此决定退出码（0 通过 / 3 不通过）。
    let capture_out: player::CaptureSlot = Rc::new(RefCell::new(None));
    player::install_notifier(app.ui().window(), &host, app.ui().as_weak())?;

    let shell = Rc::new(Shell {
        app: Rc::clone(&app),
        local: Arc::clone(&local),
        runtime: runtime.clone(),
        images,
        channel,
        state: RefCell::new(UiState {
            status,
            home: home.clone(),
            library: None,
            detail: None,
            player: None,
        }),
        host,
        player: RefCell::new(None),
        timer: slint::Timer::default(),
        pending_capture: RefCell::new(None),
        pending_source: RefCell::new(None),
        capture_out: Rc::clone(&capture_out),
    });
    SHELL.with(|slot| {
        *slot.borrow_mut() = Some(Rc::clone(&shell));
    });

    install_callbacks(&shell);
    shell.request_images(home.image_keys());
    if start.is_none() {
        shell.spawn_refresh();
    }

    app.ui().window().on_close_requested(|| {
        if let Err(error) = slint::quit_event_loop() {
            tracing::debug!(event_loop_error = ?error, "退出事件循环失败");
        }
        slint::CloseRequestResponse::HideWindow
    });

    // `--play`：直接切到播放页并起播（不经过首页点击）。
    if let Some(start) = start {
        shell.start_playback(&start);
    }

    // 事件循环启动前可能已经有消息落进队列（刷新很快时）：先手动清一次，
    // 剩下的交给 `invoke_from_event_loop` 唤醒。
    channel_drain(&shell.channel);

    // 窗口必须显式 show 才会被后端映射、才会开始出渲染回调（`run_event_loop` 只跑循环）。
    app.ui().show()?;

    let result = slint::run_event_loop();
    // 窗口关掉之前先把会话收尾（进度落盘），再摘掉 thread-local 的 Shell。
    if let Some(shell) = SHELL.with(|slot| slot.borrow().clone()) {
        shell.stop_playback();
    }
    SHELL.with(|slot| {
        slot.borrow_mut().take();
    });
    result.map_err(AppError::from)?;

    if let Some(outcome) = capture_out.borrow_mut().take()
        && !outcome.passed()
    {
        return Err(AppError::Playback(outcome.failure_text()));
    }
    Ok(())
}

impl Shell {
    /// 进入播放页并起播（`--play` 直达，不走首页点击）。
    ///
    /// 捕获参数先存进 `pending_capture`：`App::play` 会回调 `on_play` → [`Shell::play`]，
    /// 由那里一次性创建播放器——否则同一个条目会被准备两次（两个 `OpenRequest` 竞态）。
    fn start_playback(self: &Rc<Self>, start: &StartPlay) {
        *self.pending_capture.borrow_mut() = start.capture.clone();
        self.pending_source
            .borrow_mut()
            .clone_from(&start.source_id);
        self.app.play(&start.item_id);
    }

    /// 取直链结果回主线程的投递函数（`Send`：只塞队列 + 唤醒事件循环）。
    fn post_handle(self: &Rc<Self>) -> Post {
        let channel = Arc::clone(&self.channel);
        Arc::new(move |message| channel.post(UiMessage::Playback(message)))
    }

    /// 让播放器开始播一个条目；播放器不存在时新建（含 8ms 心跳）。
    fn play_item(
        self: &Rc<Self>,
        item_id: &str,
        source_id: Option<String>,
        capture: Option<player::CaptureOptions>,
        post: &Post,
    ) {
        let mut slot = self.player.borrow_mut();
        if slot.is_none() {
            *slot = Some(Player::new(
                Rc::clone(&self.app),
                Arc::clone(&self.local),
                self.runtime.clone(),
                Rc::clone(&self.host),
                capture,
                Rc::clone(&self.capture_out),
            ));
        }
        if let Some(player) = slot.as_mut() {
            player.begin(item_id, source_id, post);
        }
        drop(slot);
        self.apply_pending_view();
        self.start_timer();
    }

    /// 启动 8ms 心跳。
    fn start_timer(self: &Rc<Self>) {
        if self.timer.running() {
            return;
        }
        let weak = Rc::downgrade(self);
        self.timer.start(
            slint::TimerMode::Repeated,
            player::TICK_INTERVAL,
            move || {
                if let Some(shell) = weak.upgrade() {
                    shell.tick_playback();
                }
            },
        );
    }

    /// 心跳：泵事件、推进会话、按需重绘。
    fn tick_playback(&self) {
        {
            let mut slot = self.player.borrow_mut();
            if let Some(player) = slot.as_mut() {
                player.tick();
            }
        }
        self.apply_pending_view();
    }

    /// 在播放器上执行一个界面动作（播放器不存在时忽略）。
    fn with_player(&self, action: impl FnOnce(&mut Player)) {
        {
            let mut slot = self.player.borrow_mut();
            if let Some(player) = slot.as_mut() {
                action(player);
            }
        }
        self.apply_pending_view();
    }

    /// 把播放器排队的视图提交给界面。
    ///
    /// 必须在**放下** `player` 借用之后调用：`App::set_player` 会同步回调 nova-ui 的
    /// 播放页处理器（字幕安全区），那里会再次借用播放器。
    fn apply_pending_view(&self) {
        let view = {
            let mut slot = self.player.borrow_mut();
            slot.as_mut().and_then(Player::take_pending_view)
        };
        if let Some(view) = view {
            self.app.set_player(&view);
        }
    }

    /// 退出播放页 / 关窗时的收尾。
    fn stop_playback(&self) {
        self.timer.stop();
        let mut slot = self.player.borrow_mut();
        if let Some(player) = slot.as_mut() {
            player.shutdown();
        }
        *slot = None;
    }

    /// 把播放链路的异步结果交给播放器。
    fn apply_playback(&self, message: Message) {
        match message {
            Message::OpenReady {
                generation,
                request,
            } => {
                let mut slot = self.player.borrow_mut();
                if let Some(player) = slot.as_mut() {
                    player.on_open_ready(generation, request);
                }
            }
        }
    }

    /// 把图片键交给后台装载；到达后在主线程 `set_image`。
    fn request_images(&self, keys: Vec<ImageKey>) {
        request_images(
            &self.images,
            &self.local,
            keys,
            &self.runtime,
            &self.channel,
        );
    }

    /// 后台刷新一次，然后重建首页与第一个媒体库页。
    fn spawn_refresh(self: &Rc<Self>) {
        let local = Arc::clone(&self.local);
        let channel = Arc::clone(&self.channel);
        let images = Arc::clone(&self.images);
        let runtime = self.runtime.clone();
        let status = self.state.borrow().status.clone();
        self.runtime.spawn(async move {
            let report = refresh::refresh(Arc::clone(&local), status).await;
            tracing::info!(
                items = report.item_count,
                status = report.status.headline(),
                "刷新完成"
            );
            let build_local = Arc::clone(&local);
            let build = tokio::task::spawn_blocking(
                move || -> Result<(HomeView, Option<LibraryView>), AppError> {
                    let home = build_local.home(None)?;
                    let library = match build_local.tabs().first() {
                        Some(tab) => Some(build_local.library(&tab.id, None)?),
                        None => None,
                    };
                    Ok((home, library))
                },
            )
            .await;
            match build {
                Ok(Ok((home, library))) => {
                    let keys = image_keys(&home, library.as_ref(), None);
                    channel.post(UiMessage::Refreshed(Box::new(Refreshed {
                        status: report.status,
                        home,
                        library,
                    })));
                    request_images(&images, &local, keys, &runtime, &channel);
                }
                Ok(Err(error)) => tracing::warn!(%error, "刷新后构建视图失败"),
                Err(error) => tracing::warn!(%error, "刷新后构建视图任务失败"),
            }
        });
    }

    fn apply_search(&self, text: &str) {
        // 空串 = 清空过滤；本地缓存查询是同步读，输入防抖交给阶段 3 的现状即可。
        let term = text.trim();
        let search = (!term.is_empty()).then_some(term);
        match self.local.home(search) {
            Ok(home) => {
                self.app.set_home(&home);
                self.request_images(home.image_keys());
                self.state.borrow_mut().home = home;
            }
            Err(error) => tracing::warn!(%error, "搜索过滤本地缓存失败"),
        }
    }

    fn apply_refresh(&self, payload: Refreshed) {
        let Refreshed {
            status,
            home,
            library,
        } = payload;
        self.app.set_home(&home);
        // 刷新后的连接状态同步到顶部横幅（过期 / 断开时出现，恢复后消失）。
        self.app.set_banner(status.banner_data());
        if matches!(self.app.current_route(), Some(Route::Library { .. })) {
            if let Some(library) = &library {
                self.app.set_library(library);
            }
        }
        let keys = image_keys(&home, library.as_ref(), None);
        {
            let mut state = self.state.borrow_mut();
            state.status = status;
            state.home = home;
            if library.is_some() {
                state.library = library;
            }
        }
        self.request_images(keys);
    }

    fn apply_detail(&self, detail: DetailView) {
        let keys = detail.image_keys();
        self.app.set_detail(&detail);
        self.state.borrow_mut().detail = Some(detail);
        self.request_images(keys);
    }

    fn apply_image(&self, key: ImageKey, decoded: &DecodedImage) {
        // 纹理上传属于界面线程：像素转 `slint::Image` 必须在这里做。
        let Some(image) = image_from_decoded(decoded) else {
            return;
        };
        self.app.set_image(&key, image);
        // 重新应用当前持有的视图，让新图片进入 Slint 模型。
        let (home, library, detail) = {
            let state = self.state.borrow();
            (
                state.home.clone(),
                state.library.clone(),
                state.detail.clone(),
            )
        };
        self.app.set_home(&home);
        if let Some(library) = library {
            self.app.set_library(&library);
        }
        if let Some(detail) = detail {
            self.app.set_detail(&detail);
        }
    }

    fn open_detail(&self, item_id: &str) {
        if item_id == STATUS_ITEM_ID {
            return;
        }
        match self.local.detail(item_id) {
            Ok(Some(detail)) => {
                let keys = detail.image_keys();
                self.app.set_detail(&detail);
                self.state.borrow_mut().detail = Some(detail);
                self.request_images(keys);
            }
            Ok(None) => {
                let placeholder = DetailView::new(item_id, "正在加载…");
                self.app.set_detail(&placeholder);
                self.state.borrow_mut().detail = Some(placeholder);
            }
            Err(error) => tracing::warn!(%error, "读取详情缓存失败"),
        }
        self.spawn_detail(item_id.to_owned());
    }

    /// 详情页按需联网：`item()` + `sources()` 写回缓存后重建详情视图。
    fn spawn_detail(&self, item_id: String) {
        let Some(provider) = self.local.provider.clone() else {
            return;
        };
        let local = Arc::clone(&self.local);
        let channel = Arc::clone(&self.channel);
        self.runtime.spawn(async move {
            let id = ItemId::new(item_id.clone());
            let item = match provider.item(&id).await {
                Ok(item) => item,
                Err(nova_provider::ProviderError::Unauthorized) => {
                    local.mark_expired();
                    return;
                }
                Err(error) => {
                    tracing::warn!(item = id.as_str(), %error, "拉取条目失败");
                    return;
                }
            };
            let sources = match provider.sources(&id).await {
                Ok(sources) => sources,
                Err(nova_provider::ProviderError::Unauthorized) => {
                    local.mark_expired();
                    return;
                }
                Err(error) => {
                    tracing::warn!(item = id.as_str(), %error, "拉取播放源失败，仅显示元数据");
                    Vec::new()
                }
            };
            let build_local = Arc::clone(&local);
            let view_id = item_id.clone();
            let build =
                tokio::task::spawn_blocking(move || -> Result<Option<DetailView>, AppError> {
                    build_local.cache_item_and_sources(&item, &sources)?;
                    build_local.detail(&view_id)
                })
                .await;
            match build {
                Ok(Ok(Some(detail))) => channel.post(UiMessage::Detail(Box::new(detail))),
                Ok(Ok(None)) => tracing::debug!(item = item_id, "详情写回后仍读不到条目"),
                Ok(Err(error)) => tracing::warn!(%error, "详情写缓存失败"),
                Err(error) => tracing::warn!(%error, "详情构建任务失败"),
            }
        });
    }

    fn play(self: &Rc<Self>, item_id: &str) {
        if item_id == STATUS_ITEM_ID {
            return;
        }
        match self.local.player(item_id) {
            Ok(Some(player)) => {
                self.app.set_player(&player);
                self.state.borrow_mut().player = Some(player);
            }
            Ok(None) => {
                let player = PlayerShellView::new("正在加载…");
                self.app.set_player(&player);
                self.state.borrow_mut().player = Some(player);
            }
            Err(error) => tracing::warn!(%error, "读取播放页缓存失败"),
        }
        let post = self.post_handle();
        let capture = self.pending_capture.borrow_mut().take();
        let source = self.pending_source.borrow_mut().take();
        tracing::debug!(?item_id, has_capture = capture.is_some(), "开始播放条目");
        self.play_item(item_id, source, capture, &post);
        // 标题/进度需要真实条目；顺手刷新一次。
        self.spawn_detail(item_id.to_owned());
    }

    fn activate_tab(&self, library_id: &str) {
        match self.local.library(library_id, None) {
            Ok(view) => {
                let keys = view.image_keys();
                let _ = self.app.open_library(&view);
                self.state.borrow_mut().library = Some(view);
                self.request_images(keys);
            }
            Err(error) => tracing::warn!(%error, "构建媒体库视图失败"),
        }
    }

    fn select_source(&self, source_id: &str) {
        let Some(mut detail) = self.state.borrow().detail.clone() else {
            return;
        };
        for source in &mut detail.sources {
            source.selected = source.source_id == source_id;
        }
        self.app.set_detail(&detail);
        self.state.borrow_mut().detail = Some(detail);
    }
}

fn install_callbacks(shell: &Rc<Shell>) {
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_open_detail(move |item_id| {
            if let Some(shell) = weak.upgrade() {
                shell.open_detail(item_id);
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_play(move |item_id| {
            if let Some(shell) = weak.upgrade() {
                shell.play(item_id);
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_tab_activated(move |library_id| {
            if let Some(shell) = weak.upgrade() {
                shell.activate_tab(library_id);
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_source_selected(move |source_id| {
            if let Some(shell) = weak.upgrade() {
                shell.select_source(source_id);
            }
        });
    }
    {
        // 阶段 3 遗留债清理：搜索框已可输入，每次编辑都用本地缓存过滤重建首页。
        let weak = Rc::downgrade(shell);
        shell.app.on_search_changed(move |text| {
            if let Some(shell) = weak.upgrade() {
                shell.apply_search(text);
            }
        });
        let weak = Rc::downgrade(shell);
        shell.app.on_search_submit(move || {
            if let Some(shell) = weak.upgrade() {
                let text = shell.app.search_text();
                shell.apply_search(&text);
            }
        });
        let weak = Rc::downgrade(shell);
        shell.app.on_search_clear(move || {
            if let Some(shell) = weak.upgrade() {
                shell.apply_search("");
            }
        });
        shell.app.on_search(|| {
            tracing::debug!("搜索框已聚焦");
        });
    }
    {
        // 横幅动作按钮：阶段 3 只记录意图；真正的重新登录 / 重试流程在阶段 4+ 接入。
        let weak = Rc::downgrade(shell);
        shell.app.on_banner_action(move || {
            if let Some(shell) = weak.upgrade() {
                if let Some(banner) = shell.app.banner() {
                    tracing::info!(text = banner.text, "横幅动作触发");
                }
            }
        });
    }
    {
        shell.app.on_sort(|| {
            tracing::debug!("排序条回调：本地缓存按标题排序，阶段 3 不提供切换");
        });
    }
    {
        shell.app.on_back(|| {
            tracing::debug!("返回上一级（导航栈由 nova-ui 维护）");
        });
    }
    {
        // 字幕安全区：控制层可见时把字幕抬起（`sub-margin-y`），隐藏时归零。
        let weak = Rc::downgrade(shell);
        shell.app.on_player_subtitle_margin(move |margin| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.set_subtitle_margin(margin));
            }
        });
    }

    // ---- 播放控件：全部落到 `PlaybackEngine` ----
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_play_pause(move |playing| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.set_playing(playing));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_seek(move |position| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.seek(position));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_volume(move |volume| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.set_volume(volume));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_muted(move |muted| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.set_muted(muted));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_rate(move |rate| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.set_rate(rate));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_audio_track(move |index| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.select_audio(index));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_subtitle_track(move |index| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.select_subtitle(index));
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_retry(move || {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(Player::retry);
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_player_fullscreen(move |fullscreen| {
            if let Some(shell) = weak.upgrade() {
                // Slint 的窗口全屏；播放器状态由 nova-ui 维护。
                shell.app.ui().window().set_fullscreen(fullscreen);
            }
        });
    }
    {
        // `Esc`：nova-ui 先调本回调（提交进度 + 补发），然后自己 `go_back()` 回详情页。
        let weak = Rc::downgrade(shell);
        shell.app.on_player_exit(move |position| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.exit(position));
                shell.timer.stop();
            }
        });
    }
}

fn request_images(
    images: &Arc<ImageService>,
    local: &Arc<Local>,
    keys: Vec<ImageKey>,
    runtime: &Handle,
    channel: &Arc<UiChannel>,
) {
    let deliver_channel = Arc::clone(channel);
    images.request(local, keys, runtime, move |key, decoded| {
        deliver_channel.post(UiMessage::Image { key, decoded });
    });
}

fn image_keys(
    home: &HomeView,
    library: Option<&LibraryView>,
    detail: Option<&DetailView>,
) -> Vec<ImageKey> {
    let mut keys = home.image_keys();
    if let Some(library) = library {
        keys.extend(library.image_keys());
    }
    if let Some(detail) = detail {
        keys.extend(detail.image_keys());
    }
    let mut seen = HashSet::new();
    keys.retain(|key| seen.insert(key.clone()));
    keys
}

fn channel_drain(channel: &Arc<UiChannel>) {
    drain(&channel.queue);
}

fn drain(queue: &Arc<Mutex<VecDeque<UiMessage>>>) {
    loop {
        let message = lock(queue).pop_front();
        match message {
            Some(message) => apply_message(message),
            None => break,
        }
    }
}

fn apply_message(message: UiMessage) {
    let Some(shell) = SHELL.with(|slot| slot.borrow().clone()) else {
        return;
    };
    match message {
        UiMessage::Refreshed(payload) => shell.apply_refresh(*payload),
        UiMessage::Detail(detail) => shell.apply_detail(*detail),
        UiMessage::Image { key, decoded } => shell.apply_image(key, &decoded),
        UiMessage::Playback(message) => shell.apply_playback(message),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
