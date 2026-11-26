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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

use nova_core::{ItemId, MediaKind};
use nova_images::DecodedImage;
use nova_provider::{ItemQuery, MediaProvider};
use nova_secrets::SecretString;
use nova_ui::{
    AddServerError, App, AppWindow, BannerData, BannerLevel, DetailView, HomeView, ImageKey,
    LibraryView, PlayerShellView, Route, SearchView, TargetInfo,
};
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::connect::{ConnectedServer, connect};
use crate::error::AppError;
use crate::images::{ImageService, ProviderFetcher, image_from_decoded};
use crate::local::Local;
use crate::player::{self, Message, Player, Post, StartPlay};
use crate::refresh;
use crate::status::{STATUS_ITEM_ID, Status};
use crate::views;

/// 队列上限：界面长期不处理（最小化/锁屏）时不无限积压。
const MAX_QUEUED_MESSAGES: usize = 4096;

/// 搜索输入的防抖：停手 300ms 后才打服务端（本地缓存是即时的）。
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);

/// 调试通道定时器的节拍：命令在这个节拍上排队 / 执行 / 等待。
const DEBUG_TICK: std::time::Duration = std::time::Duration::from_millis(50);

/// 调试命令最长等待（`wait` 的上限与调试线程回信超时对齐，留 1s 余量）。
const DEBUG_WAIT_MAX_MS: u64 = 9_000;

/// 一条待执行的调试命令。
///
/// `settle` 是还需要"让界面重新登记目标"的回合数：Slint 只会在事件循环的下一回合
/// 重新求值依赖 `TargetBus.gen` 的绑定，登记回调要等那一拍才会把坐标报上来。
/// 两轮是必需的——第一轮让已存在的元素上报，第一轮之后才创建的元素（刚展开的
/// 菜单行、刚切页后的卡片）`gen` 的初值已经是新值，得再推一次才会触发 `changed`。
struct DebugTask {
    command: crate::debug::Command,
    reply: mpsc::Sender<serde_json::Value>,
    settle: u8,
    wait_ms: u64,
}

/// 一次服务端搜索最多取多少条（与 `views::SEARCH_LIMIT` 一致）。
const SEARCH_LIMIT: u32 = 60;

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
    /// 一次搜索的结果；`generation` 用来丢弃过期（乱序到达）的响应。
    Search {
        /// 发起这次查询时的代次。
        generation: u64,
        /// 重建好的搜索结果页。
        view: Box<SearchView>,
    },
    /// 一张图片解码完成。
    Image {
        /// 图片键。
        key: ImageKey,
        /// 解码后的像素。
        decoded: DecodedImage,
    },
    /// 播放链路的异步结果（取直链等）。
    Playback(Message),
    /// 应用内“添加服务器”的结果。
    AddServer(Box<Result<ConnectedServer, AddServerError>>),
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
    search: Option<SearchView>,
}

/// 主线程上的装配体。
pub(crate) struct Shell {
    app: Rc<App>,
    local: Arc<Local>,
    runtime: Handle,
    images: Arc<ImageService>,
    channel: Arc<UiChannel>,
    state: RefCell<UiState>,
    /// 与渲染 notifier 共享的 mpv 引擎状态（所有 GL 调用都在主线程）。
    host: Rc<RefCell<player::Host>>,
    /// 调试通道请求的自截：等到回读结果就写到这个路径。
    pending_shot: RefCell<Option<std::path::PathBuf>>,
    /// 图片下载器（登录成功后切换 provider 用；与 `images` 里的 fetcher 同一个）。
    fetcher: Arc<ProviderFetcher>,
    /// 播放页控制器；只在播放页打开时存在。
    player: RefCell<Option<Player>>,
    /// 播放心跳（8ms）：事件泵、会话 tick、帧 watchdog。
    timer: slint::Timer,
    /// `--play` 指定的捕获参数（等 `App::play` 回调到 [`Shell::play`] 时消费）。
    pending_capture: RefCell<Option<player::CaptureOptions>>,
    /// `--play --source` 指定的版本（同上）。
    pending_source: RefCell<Option<String>>,
    /// 详情页里用户选中的播放版本（`source_id`）。
    ///
    /// 以前这个选择只改高亮，`play` 用的还是 `--play --source` 那一次性的
    /// `pending_source`——于是"选 4K 播放出来还是默认版本"，是审计里的一条
    /// "看着有反应其实是空转"。
    selected_source: RefCell<Option<String>>,
    /// 捕获结果槽（与 `Player` 共享）。
    capture_out: player::CaptureSlot,
    /// 搜索代次：每次输入 +1，晚到的旧结果按它丢弃（避免乱序覆盖）。
    ///
    /// 用 `Arc<AtomicU64>` 而不是普通 `Cell`：防抖与服务端请求都在 tokio 线程上，
    /// 它们需要在**发请求前**就知道自己是否已经过期。
    search_generation: Arc<AtomicU64>,
    /// 上一次防抖任务的句柄：新输入 / 清空 / 关窗时 `abort()`，避免 300ms 睡眠与
    /// 后续网络请求、写缓存继续跑。任务正常结束后不主动清理（见
    /// [`Shell::abort_search_debounce`]）。
    search_debounce: RefCell<Option<JoinHandle<()>>>,
    /// 媒体库当前的排序方式（点排序条循环切换；只作用于本地缓存）。
    library_sort: RefCell<views::LibrarySort>,
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
    screenshot: Option<(std::path::PathBuf, u64)>,
    debug_socket: Option<std::path::PathBuf>,
    window_size: Option<(u32, u32)>,
    initial_search: Option<String>,
) -> Result<(), AppError> {
    let ui = AppWindow::new()?;
    // 逻辑尺寸必须在 show 之前定好：窗口映射后再改会触发一次整体重排，
    // 而 `--screenshot` 只有一次机会拍稳定帧。
    if let Some((w, h)) = window_size {
        ui.set_window_w(w as f32);
        ui.set_window_h(h as f32);
        ui.window()
            .set_size(slint::LogicalSize::new(w as f32, h as f32));
    }
    let app = Rc::new(App::new_with(ui));
    let status = local.status();
    // 离线优先：先用 SQLite 里的缓存建好首页，再启动后台刷新；状态由顶部横幅表达。
    let home = local.home(None)?;
    app.set_home(&home);
    app.set_banner(status.banner_data());

    let online: Option<Arc<dyn MediaProvider>> = local.provider_dyn();
    let fetcher = Arc::new(ProviderFetcher::new(online));
    let images = Arc::new(ImageService::new(
        Arc::clone(&local.image_cache),
        fetcher.clone(),
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
        fetcher,
        channel,
        state: RefCell::new(UiState {
            status,
            home: home.clone(),
            library: None,
            detail: None,
            player: None,
            search: None,
        }),
        host,
        pending_shot: RefCell::new(None),
        player: RefCell::new(None),
        timer: slint::Timer::default(),
        pending_capture: RefCell::new(None),
        pending_source: RefCell::new(None),
        selected_source: RefCell::new(None),
        capture_out: Rc::clone(&capture_out),
        search_generation: Arc::new(AtomicU64::new(0)),
        search_debounce: RefCell::new(None),
        library_sort: RefCell::new(views::LibrarySort::default()),
    });
    SHELL.with(|slot| {
        *slot.borrow_mut() = Some(Rc::clone(&shell));
    });

    install_callbacks(&shell);
    shell.request_images(home.image_keys());
    if start.is_none() {
        shell.spawn_refresh();
    }
    // `--search <term>`：启动后直接进搜索页（本地缓存立即出结果，同时打一次服务端）。
    if let Some(term) = initial_search.as_deref().map(str::trim) {
        if !term.is_empty() {
            shell.begin_search(term, false);
        }
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

    // 调试通道：socket 线程只负责收发，命令排队到 UI 线程执行（50ms 轮询）。
    let debug_timer = if let Some(path) = debug_socket {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<crate::debug::Job>();
        crate::debug::serve(path, job_tx);
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        let weak = Rc::downgrade(&shell);
        // 需要"等一拍"的命令留在这里，下一拍继续（不阻塞 UI 线程）。
        let pending: std::rc::Rc<RefCell<VecDeque<DebugTask>>> =
            std::rc::Rc::new(RefCell::new(VecDeque::new()));
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, DEBUG_TICK, move || {
            let Some(shell) = weak.upgrade() else {
                return;
            };
            // 命令：逐条在 UI 线程执行，回信给调试线程。
            let jobs: Vec<crate::debug::Job> = match job_rx.lock() {
                Ok(receiver) => receiver.try_iter().collect(),
                Err(poisoned) => poisoned.into_inner().try_iter().collect(),
            };
            {
                let mut queue = pending.borrow_mut();
                for (command, reply) in jobs {
                    queue.push_back(DebugTask {
                        settle: if command.needs_settle() { 2 } else { 0 },
                        wait_ms: command.wait_ms(),
                        command,
                        reply,
                    });
                }
            }
            let mut queue = std::mem::take(&mut *pending.borrow_mut());
            let mut later = VecDeque::new();
            let tick_ms = DEBUG_TICK.as_millis() as u64;
            while let Some(mut task) = queue.pop_front() {
                if task.wait_ms > tick_ms {
                    task.wait_ms -= tick_ms;
                    later.push_back(task);
                } else if task.settle > 0 {
                    // 让目标重新登记一遍，这一拍先不执行命令。
                    shell.app.refresh_targets();
                    task.settle -= 1;
                    later.push_back(task);
                } else {
                    let reply = shell.debug_execute(task.command);
                    let _ = task.reply.send(reply);
                }
            }
            *pending.borrow_mut() = later;
            // 自截：等回读结果到了再写盘。
            let shot_path = shell.pending_shot.borrow().clone();
            if let Some(path) = shot_path
                && let Some((frame, error)) = shell.host.borrow_mut().take_shot()
            {
                *shell.pending_shot.borrow_mut() = None;
                match error {
                    Some(error) => tracing::warn!(%error, "调试自截失败"),
                    None => match player::gl::write_png(&path, &frame) {
                        Ok(()) => tracing::info!(
                            path = %path.display(),
                            width = frame.width,
                            height = frame.height,
                            "调试自截已保存"
                        ),
                        Err(error) => tracing::warn!(%error, "调试自截写盘失败"),
                    },
                }
            }
        });
        Some(timer)
    } else {
        None
    };

    // 窗口自截（`--screenshot`）：等界面稳定后回读自己的 back buffer 写 PNG 再退出。
    // 走 `glReadPixels`（与播放捕获同一条路径），只含 Nova 自己的窗口，
    // 不需要截屏协议、不碰用户屏幕、也不抢焦点。
    let screenshot_timer = if let Some((path, delay_ms)) = screenshot {
        let timer = slint::Timer::default();
        let weak = Rc::downgrade(&shell);
        let delay = std::time::Duration::from_millis(delay_ms);
        let mut waited = std::time::Duration::ZERO;
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                waited += std::time::Duration::from_millis(100);
                let Some(shell) = weak.upgrade() else {
                    return;
                };
                if waited >= delay {
                    shell.host.borrow_mut().request_capture();
                    shell.app.ui().window().request_redraw();
                }
                if let Some((frame, error)) = shell.host.borrow_mut().take_shot() {
                    match error {
                        Some(error) => eprintln!("截图失败：{error}"),
                        None => match player::gl::write_png(&path, &frame) {
                            Ok(()) => println!(
                                "screenshot: {}x{} -> {}",
                                frame.width,
                                frame.height,
                                path.display()
                            ),
                            Err(error) => eprintln!("写 PNG 失败：{error}"),
                        },
                    }
                    let _ = slint::quit_event_loop();
                }
            },
        );
        Some(timer)
    } else {
        None
    };

    let result = slint::run_event_loop();
    drop(screenshot_timer);
    drop(debug_timer);
    // 窗口关掉之前先把会话收尾（进度落盘），再摘掉 thread-local 的 Shell。
    if let Some(shell) = SHELL.with(|slot| slot.borrow().clone()) {
        shell.stop_playback();
        // 退出时取消仍在防抖窗口里的搜索，别把任务留给 runtime 收尾。
        shell.abort_search_debounce();
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
        // 播放页进场：之后 mpv 才允许往窗口 back buffer 上画。
        self.host.borrow_mut().set_player_active(true);
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
    /// 调试通道用：读播放器的即时视图（若有播放会话）。
    fn live_player_view(&self) -> Option<nova_ui::PlayerView> {
        let mut slot = self.player.borrow_mut();
        slot.as_mut().map(Player::live_view)
    }

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
    /// 调试通道的命令执行入口（在 UI 线程上被调用）。
    pub(crate) fn debug_execute(&self, command: crate::debug::Command) -> serde_json::Value {
        let shell = self;
        use crate::debug::Command as Cmd;
        match command {
            Cmd::View => {
                let state = shell.state.borrow();
                let route = shell.app.current_route();
                // 详情页几何探针（`ui/detail.slint` 的 `probe-*` → `AppWindow.detail-*`）：
                // `body_y + body_height + 112 ≈ backdrop_height` 就是"内容块叠在 hero 下半部"，
                // 详见 `tests/detail_layout.rs` 与 docs/ui-netflix-1to1.md §7。
                let ui = shell.app.ui();
                let detail_geometry = serde_json::json!({
                    "body_y": ui.get_detail_body_y(),
                    "body_height": ui.get_detail_body_height(),
                    "backdrop_height": ui.get_detail_backdrop_height(),
                    "title_top": ui.get_detail_title_top(),
                    "title_bottom": ui.get_detail_title_bottom(),
                    "overview_top": ui.get_detail_overview_top(),
                    "overview_bottom": ui.get_detail_overview_bottom(),
                    "buttons_top": ui.get_detail_buttons_top(),
                    "buttons_bottom": ui.get_detail_buttons_bottom(),
                });
                serde_json::json!({
                    "ok": true,
                    "route": format!("{route:?}"),
                    "status": format!("{:?}", state.status),
                    // 当前页焦点索引（键盘导航后在真机上验证焦点落点）。
                    // `focus` 是 Rust 侧的镜像；`focus_ui` 是 `.slint` 属性（悬停会直接
                    // 改它——R2-3 的"方向键从悬停卡继续"靠 UI 侧为准）。
                    "focus": shell.app.focus_index(),
                    "focus_ui": [
                        ui.get_home_focus(),
                        ui.get_library_focus(),
                        ui.get_detail_focus(),
                        ui.get_search_focus(),
                    ],
                    "nav_active": ui.get_nav_active(),
                    // 网格滚动 / 焦点卡几何（`ui/library.slint`、`ui/search.slint` 的
                    // `probe-*`）：验证"方向键移动焦点后网格自动滚动、焦点卡完全
                    // 可见、有放大指示"。
                    "library_geometry": {
                        "scroll_y": ui.get_library_scroll_y(),
                        "viewport_h": ui.get_library_viewport_h(),
                        "focus_top": ui.get_library_focus_top(),
                        "focus_bottom": ui.get_library_focus_bottom(),
                        "zoom_on": ui.get_library_zoom_on(),
                    },
                    "search_geometry": {
                        "scroll_y": ui.get_search_scroll_y(),
                        "viewport_h": ui.get_search_viewport_h(),
                        "focus_top": ui.get_search_focus_top(),
                        "focus_bottom": ui.get_search_focus_bottom(),
                        "zoom_on": ui.get_search_zoom_on(),
                    },
                    "detail_drop_open": ui.get_detail_drop_open(),
                    // 首页整页滚动 / 继续观看行放大（`ui/home.slint` 的 `scroll-y` /
                    // `row0-zoom-on`）：验证滚轮真的滚了、悬停真的打开了放大层。
                    "home_geometry": {
                        "scroll_y": ui.get_home_scroll_y(),
                        "row0_zoom_on": ui.get_row0_zoom_on(),
                        // 键盘焦点行 / 焦点卡（体检 R2-2：首页纵向 + 行内横向跟随）。
                        // 焦点卡必须完整落在 `[-scroll_y, -scroll_y + viewport_h]` 与
                        // `[-row_scroll_x, -row_scroll_x + row_viewport_w]` 之内。
                        "viewport_h": ui.get_home_viewport_h(),
                        "focus_top": ui.get_home_focus_top(),
                        "focus_bottom": ui.get_home_focus_bottom(),
                        "row_scroll_x": ui.get_home_row_scroll_x(),
                        "row_viewport_w": ui.get_home_row_viewport_w(),
                        "focus_left": ui.get_home_focus_left(),
                        "focus_right": ui.get_home_focus_right(),
                    },
                    "home": {
                        "hero": state.home.hero.as_ref().map(|card| card.title.clone()),
                        "recent": state.home.recently_added.len(),
                        "continue": state.home.continue_watching.len(),
                        "tabs": state.home.library_tabs.iter().map(|tab| tab.title.clone()).collect::<Vec<_>>(),
                    },
                    "library": state.library.as_ref().map(|view| serde_json::json!({
                        "title": view.title,
                        "count": view.cards.len(),
                        "current_tab": view.current_tab,
                        "sort": view.sort_label,
                        "first": view.cards.first().map(|card| card.title.clone()),
                    })),
                    "detail": state.detail.as_ref().map(|view| serde_json::json!({
                        "item": view.item_id,
                        "title": view.title,
                        // 版本下拉：谁被选中（验证"选了版本再去播"是否真的生效）。
                        "sources": view.sources.iter().map(|source| serde_json::json!({
                            "id": source.source_id,
                            "label": source.title,
                            "selected": source.selected,
                        })).collect::<Vec<_>>(),
                    })),
                    "detail_geometry": detail_geometry,
                    "search": state.search.as_ref().map(|view| serde_json::json!({
                        "term": view.term,
                        "state": format!("{:?}", view.state),
                        "count": view.cards.len(),
                        "total": view.total,
                    })),
                    // 播放页状态：以前 `view` 读不到它，导致"点播放器按钮到底有没有
                    // 生效"没法在运行中的进程里验证（审计里专门指出过这个缺口）。
                    // 用**即时**视图，不要读 `state.player`：后者受 `VIEW_INTERVAL`
                    // 节流，会把"正在播放"读成"暂停"，据它下结论会得到完全相反的判断。
                    "player": shell.live_player_view().map(|view| serde_json::json!({
                        "playing": view.playing,
                        "buffering": view.buffering,
                        "volume": view.volume,
                        "muted": view.muted,
                        "rate": view.rate,
                        "subtitle_delay": view.subtitle_delay,
                        "audio_menu": view.audio_menu_open,
                        "subtitle_menu": view.subtitle_menu_open,
                        "position_ms": view.position.as_millis() as u64,
                        "error": view.error,
                    })),
                })
            }
            Cmd::Route { value, item } => match value.as_str() {
                "home" => {
                    shell.activate_tab(crate::local::HOME_TAB_ID);
                    serde_json::json!({ "ok": true, "route": "home" })
                }
                "library" => {
                    let tab = shell
                        .state
                        .borrow()
                        .home
                        .library_tabs
                        .iter()
                        .find(|tab| tab.id != crate::local::HOME_TAB_ID)
                        .map(|tab| tab.id.clone());
                    match tab {
                        Some(id) => {
                            shell.activate_tab(&id);
                            serde_json::json!({ "ok": true, "route": "library", "library": id })
                        }
                        None => serde_json::json!({ "ok": false, "error": "没有可用的媒体库标签" }),
                    }
                }
                "search" => {
                    // 搜索页：打开并聚焦页内搜索框（调试通道要能逐页扫描）。
                    shell.show_search(nova_ui::SearchView::new(""), true);
                    serde_json::json!({ "ok": true, "route": "search" })
                }
                "detail" | "player" => match item {
                    Some(item) if value == "detail" => {
                        // 走界面的导航入口：它会 push 路由并回调 `open_detail` 加载数据。
                        // 只调 `open_detail` 只准备数据、不切页（实测截图仍停在首页）。
                        shell.app.open_detail(&item);
                        serde_json::json!({ "ok": true, "route": "detail", "item": item })
                    }
                    Some(_) => serde_json::json!({
                        "ok": false,
                        "error": "player 路由请用 `--play <id>` 启动（需要整套播放会话）",
                    }),
                    None => serde_json::json!({ "ok": false, "error": "detail 路由需要 item" }),
                },
                other => serde_json::json!({ "ok": false, "error": format!("未知路由 `{other}`") }),
            },
            Cmd::Open { item } => {
                shell.app.open_detail(&item);
                serde_json::json!({ "ok": true, "item": item })
            }
            Cmd::Tab { library } => {
                shell.activate_tab(&library);
                serde_json::json!({ "ok": true, "library": library })
            }
            Cmd::Search { term, page } => {
                if page {
                    // 进搜索页并把查询词填好（本地缓存立即出结果 + 打一次服务端）。
                    shell.app.ui().set_search_text(term.as_str().into());
                    shell.begin_search(&term, false);
                    serde_json::json!({ "ok": true, "term": term, "route": "search" })
                } else {
                    // 兼容旧行为：只用本地缓存过滤首页。
                    shell.apply_search(&term);
                    serde_json::json!({ "ok": true, "term": term, "route": "home" })
                }
            }
            Cmd::Resize { width, height } => {
                // 等价于用户拖窗口边框：窗口系统会下发 configure，Slint 更新
                // `Window.width/height`，`changed width` 再把 vw 写回 Theme 触发重排。
                shell
                    .app
                    .ui()
                    .window()
                    .set_size(slint::LogicalSize::new(width as f32, height as f32));
                serde_json::json!({
                    "ok": true,
                    "width": width,
                    "height": height,
                    "viewport_width": shell.app.ui().window().size().width,
                })
            }
            Cmd::Screenshot { path } => {
                *shell.pending_shot.borrow_mut() = Some(path.clone());
                shell.host.borrow_mut().request_capture();
                shell.app.ui().window().request_redraw();
                serde_json::json!({ "ok": true, "queued": true, "path": path.display().to_string() })
            }
            Cmd::Targets => {
                let targets = shell.app.targets();
                serde_json::json!({
                    "ok": true,
                    "page": page_name(shell.app.page()),
                    "count": targets.len(),
                    "targets": targets.iter().map(target_json).collect::<Vec<_>>(),
                })
            }
            Cmd::Click { target, x, y } => match shell.resolve_point(target.as_deref(), x, y) {
                Ok((x, y, resolved)) => match shell.pointer_at(x, y, true) {
                    Ok(()) => serde_json::json!({
                        "ok": true,
                        "target": resolved.as_ref().map(|target| target.id.clone()),
                        "kind": resolved.as_ref().map(|target| target.kind.clone()),
                        "label": resolved.as_ref().map(|target| target.label.clone()),
                        "x": x,
                        "y": y,
                        "page": page_name(shell.app.page()),
                    }),
                    Err(error) => serde_json::json!({ "ok": false, "error": error }),
                },
                Err(reply) => reply,
            },
            Cmd::Hover { target, x, y } => match shell.resolve_point(target.as_deref(), x, y) {
                Ok((x, y, resolved)) => match shell.pointer_at(x, y, false) {
                    Ok(()) => serde_json::json!({
                        "ok": true,
                        "target": resolved.as_ref().map(|target| target.id.clone()),
                        "x": x,
                        "y": y,
                    }),
                    Err(error) => serde_json::json!({ "ok": false, "error": error }),
                },
                Err(reply) => reply,
            },
            Cmd::Scroll {
                target,
                x,
                y,
                dx,
                dy,
            } => match shell.resolve_point(target.as_deref(), x, y) {
                Ok((x, y, resolved)) => match shell.scroll_at(x, y, dx, dy) {
                    Ok(()) => serde_json::json!({
                        "ok": true,
                        "target": resolved.as_ref().map(|target| target.id.clone()),
                        "x": x,
                        "y": y,
                        "dx": dx,
                        "dy": dy,
                    }),
                    Err(error) => serde_json::json!({ "ok": false, "error": error }),
                },
                Err(reply) => reply,
            },
            Cmd::Key { key, ctrl, shift } => match key_event_text(&key) {
                Some(text) => {
                    let result = shell.dispatch_key_with_modifiers(&text, ctrl, shift);
                    match result {
                        Ok(()) => serde_json::json!({ "ok": true, "key": key }),
                        Err(error) => serde_json::json!({ "ok": false, "error": error }),
                    }
                }
                None => serde_json::json!({
                    "ok": false,
                    "error": format!("unknown key: {key}"),
                    "supported": [
                        "Enter", "Escape", "Left", "Right", "Up", "Down", "Space", "Tab",
                        "Backspace", "+ 单字符"
                    ],
                }),
            },
            Cmd::Type { text } => {
                let mut typed = 0u32;
                for character in text.chars() {
                    let key = slint::SharedString::from(character.to_string());
                    if shell.dispatch_key(key).is_err() {
                        break;
                    }
                    typed += 1;
                }
                serde_json::json!({ "ok": true, "typed": typed, "text": text })
            }
            // `wait` 由调试定时器节拍负责"真的等"，这里只回结果。
            Cmd::Wait { ms } => {
                serde_json::json!({ "ok": true, "waited": ms.min(DEBUG_WAIT_MAX_MS) })
            }
            Cmd::Sweep => shell.sweep(false),
            Cmd::Dead => shell.sweep(true),
            Cmd::Quit => {
                let _ = shell.app.ui().hide();
                let _ = slint::quit_event_loop();
                serde_json::json!({ "ok": true, "quit": true })
            }
        }
    }

    /// 注入一个窗口事件（坐标是逻辑像素）。
    fn dispatch(&self, event: WindowEvent) -> Result<(), String> {
        self.app
            .ui()
            .window()
            .dispatch_event_with_result(event)
            .map(|_| ())
            .map_err(|error| format!("注入事件失败：{error}"))
    }

    /// 移动指针；`click` 为真时再补按下 / 松开。
    fn pointer_at(&self, x: f32, y: f32, click: bool) -> Result<(), String> {
        let position = slint::LogicalPosition::new(x, y);
        self.dispatch(WindowEvent::PointerMoved { position })?;
        if click {
            self.dispatch(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            })?;
            self.dispatch(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            })?;
        }
        Ok(())
    }

    /// 在 `x,y` 处滚一下：先发 `PointerMoved`（真机的滚轮事件也带一个当前位置，
    /// 悬停态与命中测试都会用到它），再发 `PointerScrolled`。
    ///
    /// 符号跟 winit / Slint 原始语义：正 `dy` 是向上滚（`content-y` 加上 `dy`，
    /// 即内容下移），负 `dy` 向下滚。实测这就是真机滚轮的等效方向。
    fn scroll_at(&self, x: f32, y: f32, dx: f32, dy: f32) -> Result<(), String> {
        let position = slint::LogicalPosition::new(x, y);
        self.dispatch(WindowEvent::PointerMoved { position })?;
        self.dispatch(WindowEvent::PointerScrolled {
            position,
            delta_x: dx,
            delta_y: dy,
        })
    }

    /// 发一次按键（按下 + 松开）。
    fn dispatch_key(&self, text: slint::SharedString) -> Result<(), String> {
        self.dispatch(WindowEvent::KeyPressed { text: text.clone() })?;
        self.dispatch(WindowEvent::KeyReleased { text })
    }

    /// 带修饰键的按键注入。
    ///
    /// `WindowEvent::KeyPressed{ text }` 本身不带修饰位——Slint 的修饰状态是后端
    /// 根据"修饰键字符"的按下/抬起维护的（`key_codes::Control` 就是 `\u{11}`）。
    /// 所以要先发修饰键的按下、再发目标键、最后抬起修饰键，全局快捷键
    /// （Ctrl+F / Ctrl+N）才会命中。这一点是实测定下来的：发 `<Control>` 或
    /// `"Control"` 字符串都不生效，只有发送该控制字符可以。
    fn dispatch_key_with_modifiers(
        &self,
        text: &str,
        ctrl: bool,
        shift: bool,
    ) -> Result<(), String> {
        // `key_codes::Shift` / `key_codes::Control` 对应的控制字符。
        const SHIFT: &str = "\u{10}";
        const CONTROL: &str = "\u{11}";
        let mut held: Vec<&str> = Vec::new();
        if ctrl {
            self.dispatch(WindowEvent::KeyPressed {
                text: CONTROL.into(),
            })?;
            held.push(CONTROL);
        }
        if shift {
            self.dispatch(WindowEvent::KeyPressed { text: SHIFT.into() })?;
            held.push(SHIFT);
        }
        let result = self.dispatch_key(text.into());
        for modifier in held.into_iter().rev() {
            let _ = self.dispatch(WindowEvent::KeyReleased {
                text: modifier.into(),
            });
        }
        result
    }

    /// 把命令里的 `target` / `x,y` 解析成一个可以点的点。
    fn resolve_point(
        &self,
        target: Option<&str>,
        x: Option<f32>,
        y: Option<f32>,
    ) -> Result<(f32, f32, Option<TargetInfo>), serde_json::Value> {
        if let (Some(x), Some(y)) = (x, y) {
            return Ok((x, y, None));
        }
        let id = target.unwrap_or_default();
        match self.app.target(id) {
            Some(target) => {
                let (x, y) = target.center();
                Ok((x, y, Some(target)))
            }
            None => Err(self.unknown_target(id)),
        }
    }

    /// 目标 id 不存在时的回信：附上前 20 个可用 id，方便脚本直接改。
    fn unknown_target(&self, id: &str) -> serde_json::Value {
        let available: Vec<String> = self
            .app
            .targets()
            .iter()
            .take(20)
            .map(|target| target.id.clone())
            .collect();
        serde_json::json!({
            "ok": false,
            "error": format!("unknown target: {id}"),
            "available": available,
        })
    }

    /// 交互扫描：委托 [`nova_ui::App::sweep`]（纯界面逻辑，不需要 Shell 状态）。
    ///
    /// Shell 额外做两件事：
    /// - 扫描前记下窗口是否全屏（点 `player.fullscreen` 会真的切换窗口全屏），
    ///   扫描后恢复，避免调试把窗口留在全屏；
    /// - 扫描前没有播放器、扫描后有了（点到了 hero 的播放 / 详情页的播放），
    ///   收掉 mpv 会话，避免后台继续解码。
    fn sweep(&self, dead_only: bool) -> serde_json::Value {
        let was_fullscreen = self.app.player_fullscreen();
        let had_player = self.player.borrow().is_some();
        let report = self.app.sweep();
        if self.app.player_fullscreen() != was_fullscreen {
            self.app.ui().window().set_fullscreen(was_fullscreen);
        }
        if !had_player && self.player.borrow().is_some() {
            self.stop_playback();
        }
        let dead: Vec<serde_json::Value> = report
            .dead()
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "kind": entry.kind,
                    "label": entry.label,
                })
            })
            .collect();
        let shown: Vec<serde_json::Value> = report
            .entries
            .iter()
            .filter(|entry| !dead_only || !entry.changed)
            .map(|entry| {
                serde_json::json!({
                    "id": entry.id,
                    "kind": entry.kind,
                    "label": entry.label,
                    "changed": entry.changed,
                    "restored": entry.restored,
                    "before": entry.before,
                    "after": entry.after,
                })
            })
            .collect();
        serde_json::json!({
            "ok": true,
            "page": report.page,
            "count": report.entries.len(),
            "changed": report.entries.iter().filter(|entry| entry.changed).count(),
            "dead": dead,
            "aborted": report.aborted,
            "results": shown,
        })
    }

    fn request_images(&self, keys: Vec<ImageKey>) {
        request_images(
            &self.images,
            &self.local,
            keys,
            &self.runtime,
            &self.channel,
        );
    }

    /// 后台刷新一次，然后重建首页与（如果停在媒体库页）当前库的视图。
    ///
    /// 库视图按**发起刷新时**打开的库与排序在后台建好（用于图片预取）；真正
    /// 替换页面时 `apply_refresh` 会按**落地时**的库与排序再建一次，因为两者之间
    /// 用户可能切了库或换了排序。
    fn spawn_refresh(self: &Rc<Self>) {
        let local = Arc::clone(&self.local);
        let channel = Arc::clone(&self.channel);
        let images = Arc::clone(&self.images);
        let runtime = self.runtime.clone();
        let status = self.state.borrow().status.clone();
        let target = {
            let state = self.state.borrow();
            refresh_library_target(&self.app.current_route(), state.library.as_ref())
        };
        let sort = *self.library_sort.borrow();
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
                    // 不再用 `tabs().first()`：标签列表第一项是哨兵「首页」，
                    // 按它建出来的是顶层全量列表（`library_view` 的退化分支）。
                    let library = match target {
                        Some(id) => Some(build_local.library(&id, None, sort)?),
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

    /// 一次搜索的入口：本地缓存立刻出结果，需要时再打服务端。
    ///
    /// `debounce` 为真时（用户正在输入）会先等 [`SEARCH_DEBOUNCE`] 再联网；
    /// 每次调用都把"代次"加一，晚到的旧结果由 [`Shell::apply_search_message`] 丢弃。
    fn begin_search(&self, term: &str, debounce: bool) {
        // 先取消上一次防抖任务：快速输入时旧任务不再继续 sleep / 打网络。
        self.abort_search_debounce();
        let term = term.trim();
        if term.is_empty() {
            self.clear_search();
            return;
        }
        let generation = self.search_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let online = self.local.provider_dyn();
        // 1) 本地缓存即时反馈：命中就立刻是一屏结果；本地没有命中时先显示
        //    “搜索中…”，避免先闪一下“没有结果”再被服务端结果替换。
        match self.local.search_view(term, views::SEARCH_LIMIT) {
            Ok(view) => {
                let view = if view.is_empty() && online.is_some() {
                    SearchView::new(term)
                } else {
                    view
                };
                // 输入焦点跟着页面走：在顶栏敲第一个字符时，焦点落到搜索页的大输入框。
                self.show_search(view, true);
            }
            Err(error) => tracing::warn!(%error, term, "本地缓存搜索失败"),
        }
        // 2) 服务端：离线时本地结果就是最终结果。
        if let Some(provider) = online {
            self.spawn_search(term.to_owned(), generation, provider, debounce);
        }
    }

    /// 把搜索结果页推到界面（切页 + 粘图片）。
    ///
    /// `focus_input` 只在**同步**路径上为真（用户刚敲字）；异步结果到达时不动焦点，
    /// 否则用户按过 Enter 之后焦点会被拽回输入框。
    fn show_search(&self, view: SearchView, focus_input: bool) {
        let keys = view.image_keys();
        self.app.open_search(&view);
        self.state.borrow_mut().search = Some(view);
        if focus_input {
            self.app.ui().invoke_focus_search_input();
        }
        self.request_images(keys);
    }

    /// 后台打一次服务端搜索：成功/失败都回一条 [`UiMessage::Search`]。
    fn spawn_search(
        &self,
        term: String,
        generation: u64,
        provider: Arc<dyn MediaProvider>,
        debounce: bool,
    ) {
        let local = Arc::clone(&self.local);
        let channel = Arc::clone(&self.channel);
        let counter = Arc::clone(&self.search_generation);
        let handle = self.runtime.spawn(async move {
            if debounce {
                tokio::time::sleep(SEARCH_DEBOUNCE).await;
                // 防抖窗口里又敲了新字符：这次请求已经过期，连网络都不用发。
                if !debounce_still_current(&counter, generation) {
                    return;
                }
            }
            let query = ItemQuery {
                kinds: vec![MediaKind::Movie, MediaKind::Series],
                search: Some(term.clone()),
                limit: Some(SEARCH_LIMIT),
                ..ItemQuery::default()
            };
            let view = match provider.items(query).await {
                Ok(page) => {
                    let total = usize::try_from(page.total).unwrap_or(page.items.len());
                    let items = page.items;
                    let build_local = Arc::clone(&local);
                    let view_term = term.clone();
                    // 写缓存后再建视图：海报下载要按 `ImageKey` 回查条目。
                    let built = tokio::task::spawn_blocking(move || {
                        if let Err(error) = build_local.cache_items(&items) {
                            tracing::warn!(%error, "搜索结果写缓存失败");
                        }
                        views::search_view_from_items(&view_term, &items).with_total(total)
                    })
                    .await;
                    match built {
                        Ok(view) => view,
                        Err(error) => {
                            tracing::warn!(%error, "搜索结果构建任务失败");
                            SearchView::error(&term, "搜索失败，请稍后重试")
                        }
                    }
                }
                Err(nova_provider::ProviderError::Unauthorized) => {
                    local.mark_expired();
                    SearchView::error(&term, "登录已过期，请重新登录")
                }
                Err(error) => {
                    tracing::warn!(%error, term, "服务端搜索失败");
                    SearchView::error(&term, "搜索失败，请稍后重试")
                }
            };
            // 请求期间用户又改了查询词：丢掉这次结果，别乱序覆盖新结果。
            if !debounce_still_current(&counter, generation) {
                tracing::debug!(generation, "丢弃过期的搜索结果");
                return;
            }
            channel.post(UiMessage::Search {
                generation,
                view: Box::new(view),
            });
        });
        if debounce {
            // 只记录防抖任务：Enter / 重试的非防抖请求交给代次丢弃，不必 abort。
            *self.search_debounce.borrow_mut() = Some(handle);
        }
    }

    /// 搜索结果回到主线程：代次对得上、仍停在搜索页、查询词未被改过时才应用。
    ///
    /// 三个丢弃条件都是必需的：代次拦“旧查询”与“清空后到达”；路由拦“用户已经从
    /// 结果里点进了详情页”；查询词拦“输入框已经改了但新请求还没回来”——否则晚到
    /// 的响应会把界面从别的页面拽回搜索页，或用旧词覆盖当前输入。
    fn apply_search_message(&self, generation: u64, view: SearchView) {
        let current_generation = self.search_generation.load(Ordering::Relaxed);
        let on_search_route = matches!(self.app.current_route(), Some(Route::Search { .. }));
        let current_input = self.app.search_text();
        if !accept_search_result(
            current_generation,
            generation,
            on_search_route,
            &view.term,
            &current_input,
        ) {
            tracing::debug!(generation, "丢弃代次 / 路由 / 查询词不匹配的搜索结果");
            return;
        }
        self.show_search(view, false);
    }

    /// 清空搜索：取消未完成的防抖请求，回首页并恢复未过滤的首页（与清除前的行为一致）。
    fn clear_search(&self) {
        // 取消尚未发出（还在 300ms 睡眠里）的任务，别让它在清空后又联网。
        self.abort_search_debounce();
        // 代次加一：清空后到达的旧结果不能再把界面掀回搜索页。
        self.search_generation.fetch_add(1, Ordering::Relaxed);
        self.state.borrow_mut().search = None;
        self.apply_search("");
        self.app.show_home();
    }

    /// 搜索页「×」：清空关键词但留在搜索页（Esc 的“清空 + 回首页”见 [`Shell::clear_search`]）。
    ///
    /// 输入框与空视图已经由 `nova-ui` 侧换好（`App` 的 `search_clear_input` 回调）：
    /// 这里只同步本层的搜索状态。留着旧视图会让晚到的海报重贴回旧结果。
    /// 代次同样加一，清空前发出的请求回来时不能再把结果写回页面。
    fn clear_search_input(&self) {
        self.abort_search_debounce();
        self.search_generation.fetch_add(1, Ordering::Relaxed);
        self.state.borrow_mut().search = None;
    }

    /// 取消上一次防抖任务。
    ///
    /// 句柄在任务正常结束后**保留**到下一次 `begin_search` / 清空 / 关窗：
    /// 对已结束的任务 `abort()` 是 no-op，所以不需要后台任务反过来持有
    /// `Weak<Shell>` 来清理自身（否则要为这点清理额外维护一条反向引用）。
    fn abort_search_debounce(&self) {
        if let Some(handle) = self.search_debounce.borrow_mut().take() {
            handle.abort();
        }
    }

    /// 最近使用记忆：最近登录的一台服务器的地址与用户名（对话框预填用）。
    ///
    /// 只读 SQLite 里的 servers 表；**密码从不存储**，所以没有密码可预填。
    fn recent_server(&self) -> (String, String) {
        let recent = match self.local.db.servers() {
            Ok(servers) => servers
                .into_iter()
                .max_by_key(|server| server.created_at)
                .map(|server| {
                    (
                        server.info.base_url,
                        server.info.auth.user_name().unwrap_or_default().to_owned(),
                    )
                }),
            Err(error) => {
                tracing::warn!(%error, "读取最近服务器失败，按空表单打开");
                None
            }
        };
        recent.unwrap_or_default()
    }

    /// 打开“添加服务器”对话框：预填最近使用，附上钥匙环退化说明（如有）。
    fn open_add_server(&self) {
        if self.app.add_server_open() {
            return;
        }
        let (address, user) = self.recent_server();
        self.app
            .set_add_server_note(self.local.credential_note.as_deref().unwrap_or(""));
        self.app.open_add_server(&address, &user);
    }

    /// 提交：后台连接，界面立即回到“连接中”（不阻塞 UI 线程）。
    ///
    /// 密码在这里被包进 `SecretString` 并移入后台任务；之后不再出现在任何
    /// 本地变量、日志或数据库里。
    fn submit_add_server(self: &Rc<Self>, address: &str, user: &str, password: &str) {
        // 门控：连接中不受理重复提交（按钮本身也已被禁用）。
        if self.app.ui().get_add_server_state() == 1 {
            return;
        }
        let password = SecretString::new(password.to_owned());
        self.app.begin_add_server_connecting();
        let local = Arc::clone(&self.local);
        let channel = Arc::clone(&self.channel);
        let runtime = self.runtime.clone();
        let address = address.to_owned();
        let user = user.to_owned();
        runtime.spawn(async move {
            let outcome = connect(Arc::clone(&local), &address, &user, password).await;
            if let Err(error) = &outcome {
                // `error.message()` 是四类静态文案；detail 已由 provider 脱敏。
                tracing::warn!(message = error.message(), "应用内连接服务器失败");
            }
            channel.post(UiMessage::AddServer(Box::new(outcome)));
        });
    }

    /// 连接结果回到主线程：成功 → 关对话框、重建首页、后台刷新；失败 → 错误区。
    fn apply_add_server(self: &Rc<Self>, outcome: Result<ConnectedServer, AddServerError>) {
        match outcome {
            Ok(connected) => {
                self.app.close_add_server();
                let status = self.local.status();
                match self.local.home(None) {
                    Ok(home) => {
                        let keys = home.image_keys();
                        self.app.set_home(&home);
                        self.app.set_banner(status.banner_data());
                        {
                            let mut state = self.state.borrow_mut();
                            state.status = status;
                            state.home = home;
                            state.library = None;
                        }
                        self.request_images(keys);
                    }
                    Err(error) => tracing::warn!(%error, "登录成功但重建首页失败"),
                }
                // 钥匙环不可用：明确告知“本次会话有效”，不要静默。
                if !self.local.credentials_persistent {
                    self.app.set_banner(Some(BannerData::new(
                        format!(
                            "已连接 {}：系统密钥环不可用，本次会话有效，重启后需重新登录",
                            connected.info.name
                        ),
                        BannerLevel::Warning,
                    )));
                }
                // 图片下载器切到新会话，然后后台刷新。
                self.fetcher.set_provider(self.local.provider_dyn());
                self.spawn_refresh();
            }
            Err(error) => {
                self.app.set_add_server_error(error);
            }
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
        // 库页：按**落地时**打开的库与排序重建（后台刷新期间用户可能切库 / 换排序）。
        // 以前这里直接 `set_library(payload.library)`，而 payload 是“第一个标签 + 默认排序”
        // 建的，于是刷新一完成就把用户所在的库换成「首页」的顶层全量列表、排序条也回到
        // 默认值（R2-8）。
        let rebuilt = self.rebuild_library_after_refresh();
        // 图片预取：首页卡片 + 库页卡片（优先用刚重建的那份）。
        let keys = image_keys(&home, rebuilt.as_ref().or(library.as_ref()), None);
        {
            let mut state = self.state.borrow_mut();
            state.status = status;
            state.home = home;
            // 只在真的重建过库视图时更新缓存：不在库页时刷新不应当动它。
            if let Some(view) = rebuilt {
                state.library = Some(view);
            }
        }
        self.request_images(keys);
    }

    /// 刷新落地时重建当前媒体库视图（按当前排序）并写进界面。
    ///
    /// 不在媒体库页面、或还没打开过任何库时不动，返回 `None`。
    fn rebuild_library_after_refresh(&self) -> Option<LibraryView> {
        let id = {
            let state = self.state.borrow();
            refresh_library_target(&self.app.current_route(), state.library.as_ref())?
        };
        let sort = *self.library_sort.borrow();
        match self.local.library(&id, None, sort) {
            Ok(view) => {
                tracing::info!(
                    library = view.id,
                    sort = view.sort_label,
                    cards = view.cards.len(),
                    "刷新后重建媒体库视图"
                );
                // `set_library` 会按条目 id 保留焦点（见 `nova_ui::rebase_focus`）。
                self.app.set_library(&view);
                Some(view)
            }
            Err(error) => {
                tracing::warn!(%error, "刷新后重建媒体库视图失败");
                None
            }
        }
    }

    fn apply_detail(&self, detail: DetailView) {
        let keys = detail.image_keys();
        // 服务端回灌的详情永远把第一个版本标成选中（`detail_from_item` 的行为），
        // 会把用户在版本下拉里选的那个重置掉。这里补回用户的选择。
        let mut detail = detail;
        if let Some(selected) = self.selected_source.borrow().clone()
            && detail
                .sources
                .iter()
                .any(|source| source.source_id == selected)
        {
            for source in &mut detail.sources {
                source.selected = source.source_id == selected;
            }
        }
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
        let (home, library, detail, search) = {
            let state = self.state.borrow();
            (
                state.home.clone(),
                state.library.clone(),
                state.detail.clone(),
                state.search.clone(),
            )
        };
        self.app.set_home(&home);
        if let Some(library) = library {
            self.app.set_library(&library);
        }
        if let Some(detail) = detail {
            self.app.set_detail(&detail);
        }
        if let Some(search) = search {
            // 晚到的图片只在仍停留在搜索页时重贴：用户可能已经点进详情 / 播放页，
            // 这时 `set_search` 会把搜索页数据写回，并按旧查询词回填输入框。
            if matches!(self.app.current_route(), Some(Route::Search { .. })) {
                self.app.set_search(&search);
            }
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
        let Some(provider) = self.local.provider() else {
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
        let source = choose_source(
            self.pending_source.borrow_mut().take(),
            self.selected_source.borrow().clone(),
            matches!(
                self.app.current_route(),
                Some(Route::Detail { .. }) | Some(Route::Player { .. })
            ),
        );
        tracing::debug!(
            ?item_id,
            ?source,
            has_capture = capture.is_some(),
            "开始播放条目"
        );
        self.play_item(item_id, source, capture, &post);
        // 标题/进度需要真实条目；顺手刷新一次。
        self.spawn_detail(item_id.to_owned());
    }

    fn activate_tab(&self, library_id: &str) {
        // `HOME_TAB_ID` 是「首页」标签：库标签要等刷新完成才出现，没有这个入口
        // 用户进了媒体库就回不到首页。
        //
        // 这里必须**同时切页**：只重建首页数据的话，在媒体库/详情页点「首页」
        // 除了后台数据刷新什么都不会发生（交互扫描会把它记成"死按钮"）。
        if library_id == crate::local::HOME_TAB_ID {
            if let Ok(home) = self.local.home(None) {
                let keys = image_keys(&home, None, None);
                self.app.set_home(&home);
                self.state.borrow_mut().home = home;
                self.request_images(keys);
            }
            self.app.show_home();
            return;
        }
        let sort = *self.library_sort.borrow();
        match self.local.library(library_id, None, sort) {
            Ok(view) => {
                let keys = view.image_keys();
                let _ = self.app.open_library(&view);
                self.state.borrow_mut().library = Some(view);
                self.request_images(keys);
            }
            Err(error) => tracing::warn!(%error, "构建媒体库视图失败"),
        }
    }

    /// 排序条：`最近添加 → 标题 A→Z → 标题 Z→A → 年份从新到旧` 循环。
    ///
    /// 只对本地缓存的条目排序并重建视图（离线也必须立刻生效）；数据来自
    /// `Local::library`，它按当前 `library_sort` 排序。
    fn cycle_library_sort(&self) {
        let next = self.library_sort.borrow().next();
        *self.library_sort.borrow_mut() = next;
        let library_id = {
            let state = self.state.borrow();
            state.library.as_ref().map(|view| view.id.clone())
        };
        let Some(library_id) = library_id else {
            tracing::debug!(sort = next.label(), "还没有媒体库视图，排序只记录状态");
            return;
        };
        match self.local.library(&library_id, None, next) {
            Ok(view) => {
                tracing::info!(sort = next.label(), cards = view.cards.len(), "媒体库排序");
                self.app.set_library(&view);
                self.state.borrow_mut().library = Some(view);
            }
            Err(error) => tracing::warn!(%error, "按新排序重建媒体库失败"),
        }
    }

    /// 详情页「我的列表」：写本地库 → 更新视图（持久化，重开详情仍生效）。
    fn toggle_detail_mylist(&self) {
        let Some(mut detail) = self.state.borrow().detail.clone() else {
            return;
        };
        match self.local.toggle_mylist(&detail.item_id) {
            Ok(state) => {
                detail.in_my_list = state;
                self.app.set_detail(&detail);
                self.state.borrow_mut().detail = Some(detail);
            }
            Err(error) => tracing::warn!(%error, "切换「我的列表」失败"),
        }
    }

    fn select_source(&self, source_id: &str) {
        let Some(mut detail) = self.state.borrow().detail.clone() else {
            return;
        };
        for source in &mut detail.sources {
            source.selected = source.source_id == source_id;
        }
        *self.selected_source.borrow_mut() = Some(source_id.to_owned());
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
        // 搜索改为独立结果页：顶栏与搜索页的两个输入框走同一条链路。
        // 输入即切页 + 本地缓存即时出结果，停手 300ms 后再打服务端（防抖 + 代次）。
        let weak = Rc::downgrade(shell);
        shell.app.on_search_changed(move |text| {
            if let Some(shell) = weak.upgrade() {
                shell.begin_search(text, true);
            }
        });
        let weak = Rc::downgrade(shell);
        shell.app.on_search_input(move |text| {
            if let Some(shell) = weak.upgrade() {
                shell.begin_search(text, true);
            }
        });
        let weak = Rc::downgrade(shell);
        shell.app.on_search_submit(move || {
            if let Some(shell) = weak.upgrade() {
                let text = shell.app.search_text();
                shell.begin_search(&text, false);
            }
        });
        let weak = Rc::downgrade(shell);
        shell.app.on_search_retry(move || {
            if let Some(shell) = weak.upgrade() {
                let text = shell.app.search_text();
                shell.begin_search(&text, false);
            }
        });
        let weak = Rc::downgrade(shell);
        shell.app.on_search_clear(move || {
            if let Some(shell) = weak.upgrade() {
                shell.clear_search();
            }
        });
        // 搜索页的「×」（与 Esc 分开）：界面侧（`nova-ui`）已经清空输入框并贴上
        // 空视图，这里只同步本层状态，别让晚到的海报把旧结果重贴回去。
        let weak = Rc::downgrade(shell);
        shell.app.on_search_clear_input(move || {
            if let Some(shell) = weak.upgrade() {
                shell.clear_search_input();
            }
        });
        shell.app.on_search(|| {
            tracing::debug!("搜索框已聚焦");
        });
    }
    {
        // 横幅动作按钮：“重试”→ 重新刷新；“重新登录”（或其它）→ 打开添加服务器对话框。
        let weak = Rc::downgrade(shell);
        shell.app.on_banner_action(move || {
            if let Some(shell) = weak.upgrade() {
                let action = shell
                    .app
                    .banner()
                    .and_then(|banner| banner.action_label)
                    .unwrap_or_default();
                if action == "重试" {
                    tracing::info!("横幅动作：重新刷新");
                    shell.spawn_refresh();
                } else {
                    tracing::info!(action, "横幅动作：打开添加服务器对话框");
                    shell.open_add_server();
                }
            }
        });
    }
    {
        // 空态“连接服务器”/ Ctrl+N：打开对话框（纯界面操作，不阻塞）。
        let weak = Rc::downgrade(shell);
        shell.app.on_connect_server(move || {
            if let Some(shell) = weak.upgrade() {
                shell.open_add_server();
            }
        });
    }
    {
        // 提交：把表单交给后台连接流程；密码立即包进 SecretString。
        let weak = Rc::downgrade(shell);
        shell
            .app
            .on_add_server_submit(move |address, user, password| {
                if let Some(shell) = weak.upgrade() {
                    shell.submit_add_server(address, user, password);
                }
            });
    }
    {
        shell.app.on_add_server_close(|| {
            tracing::debug!("添加服务器对话框已取消");
        });
    }
    {
        // 排序条：点一下切到下一个排序方式，用本地缓存重建媒体库（不碰 provider）。
        let weak = Rc::downgrade(shell);
        shell.app.on_sort(move || {
            if let Some(shell) = weak.upgrade() {
                shell.cycle_library_sort();
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_detail_my_list_toggle(move || {
            if let Some(shell) = weak.upgrade() {
                shell.toggle_detail_mylist();
            }
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
        // 字幕延迟：界面上 ±0.5s 一行一按，引擎侧写 mpv 的 `sub-delay`（可负）。
        let weak = Rc::downgrade(shell);
        shell.app.on_player_subtitle_delay(move |seconds| {
            if let Some(shell) = weak.upgrade() {
                shell.with_player(|player| player.set_subtitle_delay(seconds));
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
                // 播放页退场：停止往窗口画 mpv，否则回到首页会是最后一帧视频。
                shell.host.borrow_mut().set_player_active(false);
            }
        });
    }
}

/// 页面名（调试回信里用，与 `docs/debugging.md` 的 `route` 说法一致）。
fn page_name(page: nova_ui::Page) -> &'static str {
    match page {
        nova_ui::Page::Home => "home",
        nova_ui::Page::Library => "library",
        nova_ui::Page::Detail => "detail",
        nova_ui::Page::Player => "player",
        nova_ui::Page::Search => "search",
    }
}

/// 一个目标 → 调试回信里的 JSON（字段名与任务书一致：`w` / `h`）。
fn target_json(target: &TargetInfo) -> serde_json::Value {
    serde_json::json!({
        "id": target.id,
        "kind": target.kind,
        "label": target.label,
        "x": target.x,
        "y": target.y,
        "w": target.width,
        "h": target.height,
    })
}

/// 调试命令里的键名 → Slint 的按键文本。
///
/// 命名键之外只接受**单个字符**（`"k"` / `"m"` / `"沙"` 这类）。
fn key_event_text(name: &str) -> Option<slint::SharedString> {
    let named = match name {
        "Enter" | "Return" => Some(Key::Return),
        "Escape" | "Esc" => Some(Key::Escape),
        "Left" | "LeftArrow" => Some(Key::LeftArrow),
        "Right" | "RightArrow" => Some(Key::RightArrow),
        "Up" | "UpArrow" => Some(Key::UpArrow),
        "Down" | "DownArrow" => Some(Key::DownArrow),
        "Space" => Some(Key::Space),
        "Tab" => Some(Key::Tab),
        "Backspace" => Some(Key::Backspace),
        _ => None,
    };
    if let Some(key) = named {
        return Some(key.into());
    }
    let mut characters = name.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) => Some(character.to_string().into()),
        _ => None,
    }
}

/// 调试通道的命令执行入口（在 UI 线程上被调用）。
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
        UiMessage::Search { generation, view } => shell.apply_search_message(generation, *view),
        UiMessage::Image { key, decoded } => shell.apply_image(key, &decoded),
        UiMessage::Playback(message) => shell.apply_playback(message),
        UiMessage::AddServer(outcome) => shell.apply_add_server(*outcome),
    }
}

/// 一条搜索结果是否应当上屏。
///
/// “代次 / 路由 / 查询词”三重判定只在这里实现一份：调用方只负责把当前值读出来。
/// 查询词比较前去掉首尾空白（`begin_search` 写视图时也是 trim 过的）。
pub(crate) fn accept_search_result(
    current_generation: u64,
    message_generation: u64,
    on_search_route: bool,
    message_term: &str,
    current_input: &str,
) -> bool {
    current_generation == message_generation
        && on_search_route
        && message_term.trim() == current_input.trim()
}

/// 播放时用哪个版本：显式 `--play --source` > 详情页里用户选中的版本。
///
/// `selected` 只在用户**停留在详情/播放页**时才算数：用户离开详情页后，
/// 那个选择属于上一条内容，带去播下一条是错的（`on_route` 就是这个判断）。
pub(crate) fn choose_source(
    explicit: Option<String>,
    selected: Option<String>,
    on_route: bool,
) -> Option<String> {
    explicit.or_else(|| selected.filter(|_| on_route))
}

/// 防抖醒来 / 请求返回时，代次是否仍然是最新的。
pub(crate) fn debounce_still_current(counter: &AtomicU64, generation: u64) -> bool {
    counter.load(Ordering::Relaxed) == generation
}

/// 后台刷新落地时该重建哪个媒体库视图（`None` = 不动库视图）。
///
/// 只有**停在媒体库页**且有当前库时才重建，并且用的是**当前打开的库**：以前
/// 无论在哪都用 `tabs().first()`（哨兵「首页」标签）+ 默认排序重建，刷新一完成
/// 就把用户所在的库页整包换成顶层全量列表、排序条也回到默认值。
pub(crate) fn refresh_library_target(
    route: &Option<Route>,
    current: Option<&LibraryView>,
) -> Option<String> {
    if !matches!(route, Some(Route::Library { .. })) {
        return None;
    }
    current.map(|view| view.id.clone())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{accept_search_result, debounce_still_current, refresh_library_target};
    use std::sync::atomic::AtomicU64;

    use nova_core::{
        AuthState, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo, ServerKind,
        UserData,
    };
    use nova_provider::Library;

    use super::{LibraryView, Route};
    use crate::views;

    #[test]
    fn stale_generation_is_rejected() {
        assert!(!accept_search_result(2, 1, true, "沙丘", "沙丘"));
    }

    #[test]
    fn result_arriving_after_clear_is_rejected() {
        // 清空搜索会把代次加一：旧代次的结果不能把界面掀回搜索页。
        assert!(!accept_search_result(3, 2, true, "沙丘", ""));
    }

    #[test]
    fn result_off_the_search_route_is_rejected() {
        assert!(!accept_search_result(1, 1, false, "沙丘", "沙丘"));
    }

    #[test]
    fn result_with_a_different_term_is_rejected() {
        assert!(!accept_search_result(1, 1, true, "沙丘", "沙丘：第二部"));
    }

    #[test]
    fn matching_result_is_accepted() {
        assert!(accept_search_result(7, 7, true, "沙丘", "沙丘"));
    }

    #[test]
    fn first_search_with_zero_generation_is_accepted() {
        assert!(accept_search_result(0, 0, true, "沙丘", "沙丘"));
    }

    #[test]
    fn debounce_only_continues_for_the_current_generation() {
        let counter = AtomicU64::new(3);
        assert!(debounce_still_current(&counter, 3));
        assert!(!debounce_still_current(&counter, 2));
    }

    // ---- 后台刷新：不能把用户所在的库页换成别的库 / 默认排序（R2-8）----

    #[test]
    fn refresh_target_is_the_open_library_only_on_the_library_page() {
        let view = LibraryView::new("lib-anime", "动画电影");
        let library = Route::Library {
            id: "lib-anime".to_owned(),
            title: "动画电影".to_owned(),
        };
        assert_eq!(
            refresh_library_target(&Some(library.clone()), Some(&view)),
            Some("lib-anime".to_owned()),
            "停在库页：重建当前打开的库（而不是标签列表第一项）"
        );
        // 不在库页：刷新不应当动库视图（否则会把首页/详情页的额外交互搞乱）。
        let no_view = LibraryView::new("lib-anime", "动画电影");
        assert_eq!(
            refresh_library_target(&Some(Route::Home), Some(&no_view)),
            None
        );
        assert_eq!(
            refresh_library_target(
                &Some(Route::Detail {
                    item_id: "m1".to_owned()
                }),
                Some(&no_view)
            ),
            None
        );
        // 库页但还没有库视图（刚启动）：不动。
        assert_eq!(refresh_library_target(&Some(library), None), None);
    }

    fn server_info() -> ServerInfo {
        ServerInfo {
            id: ServerId::new("srv-a"),
            kind: ServerKind::Emby,
            name: "测试".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            version: None,
            user_id: None,
            auth: AuthState::Unauthorized,
            metadata_language: Language::undetermined(),
        }
    }

    fn library(id: &str, name: &str) -> Library {
        Library {
            id: ItemId::new(id),
            name: name.to_owned(),
            kind: MediaKind::Movie,
            collection_type: Some("movies".to_owned()),
            item_count: None,
        }
    }

    fn item(id: &str, title: &str, parent: &str) -> MediaItem {
        MediaItem {
            id: ItemId::new(id),
            kind: MediaKind::Movie,
            title: title.to_owned(),
            sort_title: title.to_owned(),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime: None,
            parent_id: Some(ItemId::new(parent)),
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: nova_core::ImageSet::default(),
            user_data: UserData::default(),
        }
    }

    /// 终态回放：刷新落地后用「当前库 + 当前排序」重建，库与排序都不能变。
    ///
    /// 修复前 `spawn_refresh` 用的是 `tabs().first()`（哨兵「首页」）+ 默认排序，
    /// 这个用例里的目标就会变成 `@home`、排序回到「最近添加」、卡片变成顶层全量。
    #[test]
    fn refresh_rebuilds_the_open_library_with_the_users_sort() {
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        // 先把服务器与条目写进库，再 `Local::open`：`Local` 在打开时只读一次服务器。
        // （与 `refresh::tests` 里“先建库写数据、再开 Local”的做法一致。）
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server_info(), 1).expect("写服务器");
            db.upsert_items(
                &ServerId::new("srv-a"),
                &[
                    item("a2", "B 片", "lib-anime"),
                    item("a1", "A 片", "lib-anime"),
                    item("m1", "电影一", "lib-movie"),
                ],
                1,
            )
            .expect("写条目");
        }
        let local = crate::local::Local::open(&env, None).expect("打开状态目录");
        local.set_libraries(vec![
            library("lib-anime", "动画电影"),
            library("lib-movie", "电影"),
        ]);

        // 用户停在「动画电影」页，并且已经把排序切到「标题 A→Z」。
        let sort = views::LibrarySort::TitleAsc;
        let current = local.library("lib-anime", None, sort).expect("构建当前库");
        assert_eq!(current.title, "动画电影");
        assert_eq!(current.sort_label, sort.label());

        let route = Some(Route::Library {
            id: "lib-anime".to_owned(),
            title: "动画电影".to_owned(),
        });
        let target = refresh_library_target(&route, Some(&current)).expect("应当重建当前库");
        let rebuilt = local.library(&target, None, sort).expect("重建库视图");
        assert_eq!(rebuilt.id, "lib-anime", "不能换成别的标签的视图");
        assert_eq!(rebuilt.title, "动画电影");
        assert_eq!(
            rebuilt.sort_label, "排序：标题 A→Z",
            "用户的排序选择必须保留"
        );

        // 反面证据：修复前的目标（标签列表第一项 = 哨兵「首页」+ 默认排序）
        // 会拿到另一个库题的视图与默认排序。
        let old_target = local.tabs()[0].id.clone();
        assert_ne!(old_target, target, "修复前的目标就是标签列表第一项");
        let old_way = local
            .library(&old_target, None, views::LibrarySort::default())
            .expect("修复前的重建");
        assert_ne!(old_way.id, "lib-anime");
        assert_ne!(old_way.title, "动画电影", "修复前页面标题会变成「首页」");
        assert_eq!(old_way.sort_label, views::LibrarySort::default().label());
    }
}
