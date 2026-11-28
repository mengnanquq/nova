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

use nova_core::{ItemId, MediaKind, UserPreferences};
use nova_images::DecodedImage;
use nova_provider::{ItemQuery, MediaProvider};
use nova_secrets::SecretString;
use nova_ui::{
    AddServerError, App, AppWindow, BannerData, BannerLevel, DetailView, HomeView, ImageKey,
    LibraryView, Page, PlayerShellView, PlayerView, Route, SearchView, TargetInfo, TrackItem,
};
use slint::ComponentHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::connect::{ConnectedServer, connect};
use crate::error::AppError;
use crate::images::{
    IMAGE_STORE_BUDGET, ImageScope, ImageService, ProviderFetcher, image_from_decoded,
};
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
    Detail {
        /// 发起这次详情请求时的代次（见 [`accept_detail`]）。
        generation: u64,
        /// 重建好的详情页。
        view: Box<DetailView>,
    },
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
    /// 连接状态变了（例如一次真实 401 刚把会话标成过期）。
    Status(Status),
}

/// 挂起的一次窗口自截。
///
/// 有显示器时合成器每帧都回调，`wait` 还没到期就先用 `glReadPixels` 回读窗口真实
/// back buffer；没有显示器 / 锁屏时合成器不给帧回调（`request_redraw()` 出不了帧），
/// 宽限期一过改走 `take_snapshot()` 离屏渲染——两者都只截 Nova 自己的窗口。
struct PendingShot {
    path: std::path::PathBuf,
    wait: player::gl::ShotWait,
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
    /// 挂起的一次自截（`--screenshot` 与调试通道共用）：拿到帧就写到这个路径。
    pending_shot: RefCell<Option<PendingShot>>,
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
    /// 详情代次：`open_detail` / `spawn_detail` 每次 +1，晚到的旧响应按它丢弃。
    ///
    /// 详情刷新没有取消机制：点开 A 立刻又点开 B 时，A 的响应可能后到。
    /// 只靠代次还不够（路由可能已经离开详情页），所以 [`accept_detail`] 还要看路由。
    detail_generation: RefCell<u64>,
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
        enqueue(&self.queue, message);
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

    // 图片下载器每次都按 `Local` 的即时状态取 provider：启动时没有令牌、之后
    // 应用内登录成功，或者刷新把状态从过期变成已连接，下一张图就能下载。
    let images = Arc::new(ImageService::new(
        Arc::clone(&local.image_cache),
        Arc::new(ProviderFetcher::from_local(Arc::clone(&local))),
    ));
    let channel = Arc::new(UiChannel::new(app.ui().as_weak()));
    {
        // 状态一变就投一条消息：横幅与 `state.status` 立刻跟上（`mark_expired`
        // 发生在后台任务里，不能等下一次启动或下一次刷新）。
        //
        // 注意：回调在 `Local::set_status` 里被调用，**禁止**在回调里再调
        // `set_status`：那会在 `set_status` 已经持过状态锁之后再次改状态并递归
        // 通知监听器（无限递归 / 卡死）。这里只投一条消息，不碰 `Local`。
        let status_channel = Arc::clone(&channel);
        local.set_status_listener(Arc::new(move |status: &Status| {
            status_channel.post(UiMessage::Status(status.clone()));
        }));
    }

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
        detail_generation: RefCell::new(0),
    });
    SHELL.with(|slot| {
        *slot.borrow_mut() = Some(Rc::clone(&shell));
    });

    install_callbacks(&shell);
    shell.prefetch_images(home.image_keys());
    // 无论是不是 `--play` / `--capture`，都在后台刷一次：有令牌时状态是按
    // `Connected` 乐观启动的（见 `Local::open`），不验证的话「Expired + 有令牌」
    // 永远发现不了，要等到播放失败才看到一句模糊的错误。刷新只写缓存与状态，
    // 不切路由，也不改变播放页内容。
    shell.spawn_refresh(false);
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
            // 自截：回读到了 / 等帧超时改走离屏渲染，然后写盘。
            if let Some((path, result)) = shell.take_pending_shot() {
                match result {
                    Ok(frame) => match player::gl::write_png(&path, &frame) {
                        Ok(()) => tracing::info!(
                            path = %path.display(),
                            width = frame.width,
                            height = frame.height,
                            "调试自截已保存"
                        ),
                        Err(error) => tracing::warn!(%error, "调试自截写盘失败"),
                    },
                    Err(error) => tracing::warn!(%error, "调试自截失败"),
                }
            }
        });
        Some(timer)
    } else {
        None
    };

    // 窗口自截（`--screenshot`）：等界面稳定后拍一张自己的画面写 PNG 再退出。
    // 有显示器时先走 `glReadPixels`（与播放捕获同一条路径，读窗口真实 back buffer）；
    // 没有显示器 / 锁屏时合成器不给帧回调，宽限期一过改走 `take_snapshot()` 离屏渲染。
    // 两条路都只含 Nova 自己的窗口：不需要截屏协议、不碰用户屏幕、也不抢焦点。
    let screenshot_timer = if let Some((path, delay_ms)) = screenshot {
        let timer = slint::Timer::default();
        let weak = Rc::downgrade(&shell);
        let delay = std::time::Duration::from_millis(delay_ms);
        let mut waited = std::time::Duration::ZERO;
        let mut requested = false;
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                waited += std::time::Duration::from_millis(100);
                let Some(shell) = weak.upgrade() else {
                    return;
                };
                if !requested && waited >= delay {
                    requested = true;
                    shell.request_shot(path.clone());
                }
                let Some((path, result)) = shell.take_pending_shot() else {
                    return;
                };
                match result {
                    Ok(frame) => match player::gl::write_png(&path, &frame) {
                        Ok(()) => println!(
                            "screenshot: {}x{} -> {}",
                            frame.width,
                            frame.height,
                            path.display()
                        ),
                        Err(error) => eprintln!("写 PNG 失败：{error}"),
                    },
                    Err(error) => eprintln!("截图失败：{error}"),
                }
                let _ = slint::quit_event_loop();
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
    /// 离屏渲染一张当前窗口（`take_snapshot()` 会完整跑一遍 `RenderingSetup` /
    /// `BeforeRendering` / `AfterRendering`，视频纹理也照常合成）。
    ///
    /// 不 present、不碰用户屏幕，也不需要合成器给帧回调。
    fn snapshot_frame(&self) -> Result<player::gl::Frame, String> {
        let buffer = self
            .app
            .ui()
            .window()
            .take_snapshot()
            .map_err(|error| format!("离屏渲染失败：{error}"))?;
        player::gl::Frame::from_pixel_buffer(&buffer)
    }

    /// 发起一次窗口自截：先清掉上一轮残留的回读结果，再请求回读 + 重绘。
    ///
    /// 同一时刻只挂一张（后到的覆盖先到的）；宽限期内没等到 `AfterRendering` 时，
    /// [`Self::take_pending_shot`] 会改走离屏渲染。
    fn request_shot(&self, path: std::path::PathBuf) {
        // 上一轮离屏渲染也可能顺手把回读装满过，别让这一轮直接拿到旧帧。
        let _ = self.host.borrow_mut().take_shot();
        *self.pending_shot.borrow_mut() = Some(PendingShot {
            path,
            wait: player::gl::ShotWait::start(),
        });
        self.host.borrow_mut().request_capture();
        self.app.ui().window().request_redraw();
    }

    /// 取挂起的自截结果：回读到了就返回，等帧超时则离屏渲染一张。
    ///
    /// `None` 表示还要继续等（宽限期未到、回读也还没回来）。
    ///
    /// 注意：`take_snapshot()` 会**重入渲染回调**（`BeforeRendering` 里 mpv 要画视频帧，
    /// 回调自己会 `try_borrow_mut` 那个 `host`），所以调它前必须先把 `host` 的借用放掉。
    fn take_pending_shot(&self) -> Option<(std::path::PathBuf, Result<player::gl::Frame, String>)> {
        let mut slot = self.pending_shot.borrow_mut();
        let pending = slot.as_mut()?;
        let readback = self.host.borrow_mut().take_shot();
        let result = match readback {
            Some((frame, None)) => Ok(frame),
            // 回读失败（后端不是 GL 等）：离屏渲染还能救一次。
            Some((_frame, Some(error))) => self
                .snapshot_frame()
                .map_err(|snapshot_error| format!("{error}；{snapshot_error}")),
            // 宽限期内没等到 `AfterRendering`：合成器没给帧回调（无显示器 / 锁屏），
            // 自己离屏渲染一帧。
            None if pending.wait.snapshot_due(std::time::Instant::now()) => {
                tracing::debug!(
                    "{}ms 内没有 AfterRendering，改走 take_snapshot 离屏渲染",
                    player::gl::SHOT_FRAME_TIMEOUT.as_millis()
                );
                self.snapshot_frame()
            }
            None => return None,
        };
        let path = slot.take().map(|pending| pending.path)?;
        // 离屏渲染那一帧如果顺手把回读也装满了，丢掉：下次请求不该拿到旧帧。
        let _ = self.host.borrow_mut().take_shot();
        Some((path, result))
    }

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
        prefs: UserPreferences,
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
            player.begin(item_id, source_id, Some(prefs), post);
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
                    // 键盘焦点层（体检 R2-2b）：`content` / `hero` / `navbar`。
                    // 纯键盘 / 遥控器能不能到顶栏与 hero 按钮，就靠它 + `nav_focus`
                    // 在真机上验证；`nav_focus`：0..标签数-1 = 库标签，
                    // 标签数 = 搜索按钮，-1 = 不在顶栏。
                    "focus_layer": shell.app.focus_layer().name(),
                    "nav_focus": ui.get_nav_focus(),
                    // hero 按钮行的焦点（0 = 播放、1 = 更多信息）；只在
                    // `focus_layer == "hero"` 时有意义。
                    "hero_focus": ui.get_hero_focus(),
                    "nav_tabs": ui.get_debug_nav_tabs(),
                    // 顶栏搜索框是否真的拿到了焦点（Ctrl+F / 点击搜索后的交接态）。
                    // 以前只能靠输入字符是否落到框里间接判断（体检第二、三轮）。
                    "search_focused": ui.get_search_focused(),
                    // 网格滚动 / 焦点卡几何（`ui/library.slint`、`ui/search.slint` 的
                    // `probe-*`）：验证"方向键移动焦点后网格自动滚动、焦点卡完全
                    // 可见、有放大指示"。`focus_top/bottom` 是**视口相对**坐标
                    // （内容坐标 + `scroll_y`，体检 R3-2），所以"焦点卡在视口内"就是
                    // `0 <= top && bottom <= viewport_h`，不用再换算。
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
                    // 顶栏键盘焦点的几何（`navbar.slint` 的 `probe-*`）：窄窗口下焦点
                    // 移到被截断的标签上时，`focus_tab_left/right` 必须落在
                    // `0..tabs_viewport_w` 里，`tabs_scroll_x` 变成非 0。
                    "nav_geometry": {
                        "focus_tab_left": ui.get_nav_focus_tab_left(),
                        "focus_tab_right": ui.get_nav_focus_tab_right(),
                        "tabs_viewport_w": ui.get_nav_tabs_viewport_w(),
                        "tabs_scroll_x": ui.get_nav_tabs_scroll_x(),
                    },
                    "detail_drop_open": ui.get_detail_drop_open(),
                    // 首页整页滚动 / 继续观看行放大（`ui/home.slint` 的 `scroll-y` /
                    // `row0-zoom-on`）：验证滚轮真的滚了、悬停真的打开了放大层。
                    "home_geometry": {
                        "scroll_y": ui.get_home_scroll_y(),
                        "row0_zoom_on": ui.get_row0_zoom_on(),
                        // 键盘焦点行 / 焦点卡（体检 R2-2：首页纵向 + 行内横向跟随）。
                        // `focus_top/bottom`（纵向）与 `focus_left/right`（焦点行自己的
                        // 横向视口）都是**视口相对**坐标（体检 R3-2）：纵向加外层
                        // `content-y`，横向加该行 `content-x`。所以"焦点卡在视口内"就是
                        // `0 <= top && bottom <= viewport_h`（横向同理，上限换
                        // `row_viewport_w`）；`scroll_y` / `row_scroll_x` 只报滚动偏移。
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
                        "hero_item": state.home.hero.as_ref().map(|card| card.item_id.clone()),
                        "hero_overview_len": state.home.hero.as_ref()
                            .and_then(|card| card.overview.as_ref())
                            .map(|text| text.chars().count())
                            .unwrap_or(0),
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
                        // 是否已在「我的列表」（以界面属性为准：切换是即时生效的）。
                        "in_my_list": ui.get_detail_in_my_list(),
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
                        // 当前选中的音轨 / 字幕轨（引擎编号 + 菜单里的标签）。
                        // `null` = 没有选中（字幕关闭）或引擎还没枚举到轨道。
                        "audio_track": selected_track_probe(&view.audio_tracks),
                        "subtitle_track": selected_track_probe(&view.subtitle_tracks),
                    })),
                    // 「添加服务器」对话框：密码只给长度，不回显明文。
                    "dialog": dialog_probe(
                        ui.get_add_server_open(),
                        ui.get_add_server_address().as_str(),
                        ui.get_add_server_user().as_str(),
                        ui.get_add_server_password().as_str(),
                        ui.get_add_server_error().as_str(),
                    ),
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
                shell.request_shot(path.clone());
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
            Cmd::Type { text, target } => {
                // 缺省**不做预点击**：字符直接作为按键事件送给当前焦点（CJK 单字符
                // 与 `key` 命令同一条路）。以前在无输入目标的页面预点击会落空，
                // 看起来像"type 无效"（体检 R5 工具缺口）。只有显式给了 `target`
                // 才先点它一下（输入框不在当前焦点上时用）。
                let clicked = match target.as_deref() {
                    Some(target) => match shell.resolve_point(Some(target), None, None) {
                        Ok((x, y, resolved)) => match shell.pointer_at(x, y, true) {
                            Ok(()) => resolved.map(|target| target.id),
                            Err(error) => {
                                return serde_json::json!({ "ok": false, "error": error });
                            }
                        },
                        Err(reply) => return reply,
                    },
                    None => None,
                };
                let mut typed = 0u32;
                for character in text.chars() {
                    let key = slint::SharedString::from(character.to_string());
                    if shell.dispatch_key(key).is_err() {
                        break;
                    }
                    typed += 1;
                }
                serde_json::json!({
                    "ok": true,
                    "typed": typed,
                    "text": text,
                    "target": clicked,
                })
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

    /// 测试用装配：与 `run_window` 同一组字段，但不需要事件循环 / 渲染回调。
    ///
    /// 为什么需要它：扫描的副作用（点到播放器的音量 / 静音）只有走 [`Shell::sweep`]
    /// 才能真正测到。以前只测 `sweep_preserving_audio` 这个接缝，把 `Shell::sweep`
    /// 里的恢复调用删掉，测试照样绿。
    #[cfg(test)]
    fn for_test(app: Rc<App>, local: Arc<Local>, runtime: Handle) -> Self {
        let images = Arc::new(ImageService::new(
            Arc::clone(&local.image_cache),
            Arc::new(ProviderFetcher::from_local(Arc::clone(&local))),
        ));
        let home = local.home(None).unwrap_or_default();
        Self {
            channel: Arc::new(UiChannel::new(app.ui().as_weak())),
            state: RefCell::new(UiState {
                status: local.status(),
                home,
                library: None,
                detail: None,
                player: None,
                search: None,
            }),
            app,
            local,
            runtime,
            images,
            host: Rc::new(RefCell::new(player::Host::new(true))),
            pending_shot: RefCell::new(None),
            player: RefCell::new(None),
            timer: slint::Timer::default(),
            pending_capture: RefCell::new(None),
            pending_source: RefCell::new(None),
            selected_source: RefCell::new(None),
            capture_out: Rc::new(RefCell::new(None)),
            search_generation: Arc::new(AtomicU64::new(0)),
            search_debounce: RefCell::new(None),
            library_sort: RefCell::new(views::LibrarySort::default()),
            detail_generation: RefCell::new(0),
        }
    }

    /// 交互扫描：委托 [`nova_ui::App::sweep`]（纯界面逻辑，不需要 Shell 状态）。
    ///
    /// Shell 额外做三件事：
    /// - 扫描前记下窗口是否全屏（点 `player.fullscreen` 会真的切换窗口全屏），
    ///   扫描后恢复，避免调试把窗口留在全屏；
    /// - 扫描前记下音量 / 静音（点 `player.volume` / `player.mute` 是真实副作用），
    ///   扫描后恢复（见 [`sweep_preserving_audio`]），否则体检一次就改了用户的偏好；
    /// - 扫描前没有播放器、扫描后有了（点到了 hero 的播放 / 详情页的播放），
    ///   收掉 mpv 会话，避免后台继续解码。
    fn sweep(&self, dead_only: bool) -> serde_json::Value {
        let was_fullscreen = self.app.player_fullscreen();
        let had_player = self.player.borrow().is_some();
        let (restored, report) = sweep_preserving_audio(&self.app, || self.app.sweep());
        // `restored` = **扫描结束时音频与扫描前一致**（体检 R9-C3）：没被改动过、无需
        // 恢复的页面也算 `true`。以前它是“恢复器有没有写过界面”，于是无音频的页面
        // 永远报 `false`，被误读成恢复失败。真正的失败（扫完仍与扫描前不同）已由
        // [`sweep_preserving_audio`] 打了 warn。
        if self.app.player_fullscreen() != was_fullscreen {
            self.app.ui().window().set_fullscreen(was_fullscreen);
        }
        if !had_player && self.player.borrow().is_some() {
            self.stop_playback();
        }
        // 恢复器写回的是 `nova-ui` 的视图；调试通道 `view.search` 读的是本层镜像。
        // 扫描点到 `search.clear` 时本层镜像被 `clear_search` 清成 `None`，不同步的话
        // 扫描结束明明回到了搜索页，`view.search` 还是 `null`（看不出查询词回来了没有）。
        //
        // 只在**仍在搜索页**时回填：离开搜索页（Esc 回首页 / 返回）不会清 `nova-ui`
        // 的搜索视图，而本层镜像已经被 `clear_search` 清成 `None`；不加路由门控的话，
        // 在首页 / 详情页跑一次 `sweep` 就会把旧的 SearchView 又写回镜像。
        if matches!(self.app.current_route(), Some(Route::Search { .. })) {
            let search_view = self.app.search_view();
            self.state.borrow_mut().search = (!search_view.term.is_empty()).then_some(search_view);
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
                    "skipped": entry.skipped,
                    "no_op": entry.no_op,
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
            "abort_reason": report.abort_reason,
            "audio_restored": restored,
            "results": shown,
        })
    }

    /// 上报某个页面当前可见窗口的图片键（滚动时补发新进入视口的卡）。
    fn request_images(&self, scope: ImageScope, keys: Vec<ImageKey>) {
        request_images(
            &self.images,
            &self.local,
            scope,
            keys,
            &self.runtime,
            &self.channel,
        );
    }

    /// 首屏预热：只发前 [`crate::images::MAX_PREFETCH`] 张，其余由可见窗口补发。
    fn prefetch_images(&self, keys: Vec<ImageKey>) {
        prefetch_images(
            &self.images,
            &self.local,
            keys,
            &self.runtime,
            &self.channel,
        );
    }

    /// 首页某一行（`row = -1` 是 hero）当前的可见图片窗口。
    ///
    /// 区间 `(first, count)` 由 `.slint` 侧按行内 `content-x` / 卡宽与页面纵向
    /// 可见行算好；这里只做"行号 → 卡片"的映射。**空窗口（`count == 0`）在离屏
    /// 时也要处理**：那是页面离屏时行的上报，用来清掉 `HomeRow` 的可见集合；
    /// 非空窗口只在首页路由下受理。
    fn request_home_images(&self, row: i32, first: i32, count: i32) {
        let on_home = matches!(self.app.current_route(), Some(Route::Home) | None);
        if !on_home && count > 0 {
            return;
        }
        let (scope, keys) = {
            let state = self.state.borrow();
            if row < 0 {
                let keys = if count > 0 {
                    state.home.hero_image_keys()
                } else {
                    Vec::new()
                };
                (ImageScope::HomeHero, keys)
            } else {
                let row = row as usize;
                let first = first.max(0) as usize;
                let count = count.max(0) as usize;
                (
                    ImageScope::HomeRow(row),
                    state.home.row_image_keys(row, first, count),
                )
            }
        };
        self.request_images(scope, keys);
    }

    /// 后台刷新一次，然后重建首页与（如果停在媒体库页）当前库的视图。
    ///
    /// 库视图按**发起刷新时**打开的库与排序在后台建好（用于图片预取）；真正
    /// 替换页面时 `apply_refresh` 会按**落地时**的库与排序再建一次，因为两者之间
    /// 用户可能切了库或换了排序。
    ///
    /// `force` 只给横幅上的「重试」用：此时状态是 `RefreshFailed`，普通刷新会
    /// 直接早退（一次请求都不发），强制模式会真的重试一次。
    fn spawn_refresh(self: &Rc<Self>, force: bool) {
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
            let report = if force {
                refresh::refresh_forced(Arc::clone(&local), status).await
            } else {
                refresh::refresh(Arc::clone(&local), status).await
            };
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
                    prefetch_images(&images, &local, keys, &runtime, &channel);
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
                self.prefetch_images(home.image_keys());
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
        // 离线（未登录 / 令牌过期 / 没有服务器）时不发服务端搜索：本地命中就是
        // 最终结果。以前照样发一次注定失败的请求，它的错误页会在几秒后把本地
        // 结果整页替换成“搜索失败”——而本地搜索正是离线时唯一的搜索。
        let online = online_provider(&self.local.status(), self.local.provider_dyn());
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
        // 图片不再在这里按整页请求：`open_search` 落地时会按网格可见窗口上报
        // （`App::on_search_images`），大结果集也只请求视口里的那几十张。
        self.app.open_search(&view);
        self.state.borrow_mut().search = Some(view);
        if focus_input {
            self.app.ui().invoke_focus_search_input();
        }
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
                        if let Err(error) = build_local.cache_search_items(&items) {
                            tracing::warn!(%error, "搜索结果写缓存失败");
                        }
                        // 服务端给的 `total` 只在**疑似被截断**时用本地计数兜底：
                        // `total == items.len()` 且条数正好等于请求的 limit，说明服务端
                        // 的 total 很可能就是返回条数（R6-10）。其它情况（total 明显
                        // 大于返回条数）直接用服务端的值，不白跑一次本地计数。
                        let truncated = total == items.len()
                            && items.len() >= usize::try_from(SEARCH_LIMIT).unwrap_or(usize::MAX);
                        let local_total = if truncated {
                            build_local.search_count(&view_term)
                        } else {
                            0
                        };
                        views::search_view_from_items(&view_term, &items)
                            .with_total(total.max(local_total))
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
                Err(nova_provider::ProviderError::NoToken) => {
                    // 门控之后正常走不到这里；即使走到，也只是“本机没有令牌”，
                    // 不能把会话标成过期。
                    SearchView::error(&term, "未登录：请先连接服务器")
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
                        self.app.set_home(&home);
                        self.apply_status(status);
                        {
                            let mut state = self.state.borrow_mut();
                            state.home = home.clone();
                            state.library = None;
                        }
                        self.prefetch_images(home.image_keys());
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
                // 图片下载器跟着 `Local` 的即时状态取 provider，不必在这里切换；
                // 直接后台刷新。
                self.spawn_refresh(false);
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
        self.apply_status(status);
        // 库页：按**落地时**打开的库与排序重建（后台刷新期间用户可能切库 / 换排序）。
        // 以前这里直接 `set_library(payload.library)`，而 payload 是“第一个标签 + 默认排序”
        // 建的，于是刷新一完成就把用户所在的库换成「首页」的顶层全量列表、排序条也回到
        // 默认值（R2-8）。
        let rebuilt = self.rebuild_library_after_refresh();
        // 图片预取：首页卡片 + 库页卡片（优先用刚重建的那份）。
        let keys = image_keys(&home, rebuilt.as_ref().or(library.as_ref()), None);
        {
            let mut state = self.state.borrow_mut();
            state.home = home;
            // 只在真的重建过库视图时更新缓存：不在库页时刷新不应当动它。
            if let Some(view) = rebuilt {
                state.library = Some(view);
            }
        }
        self.prefetch_images(keys);
    }

    /// 状态变化落地：横幅与 `state.status` 一起更新。
    ///
    /// 令牌过期是后台任务里发现的（`Local::mark_expired`），不经过刷新也会到达这里
    /// （见 `run_window` 注册的状态监听器）——横幅不必等下一次启动。
    fn apply_status(&self, status: Status) {
        // 会话恢复在线（重新登录 / 刷新成功）：之前失败的海报不再是“坏图”，
        // 清掉失败记录并重新排一次预取（只前 `MAX_PREFETCH` 张，其余在用户滚到
        // 它们时由可见窗口重新上报），否则它们会永远是首字母占位。
        if recovered_session(&self.state.borrow().status, &status) {
            self.images.clear_failures();
            self.prefetch_images(self.current_image_keys());
        }
        self.app.set_banner(status.banner_data());
        self.state.borrow_mut().status = status;
    }

    /// 当前界面持有的视图里的全部图片键（首页 + 库页 + 详情页）。
    fn current_image_keys(&self) -> Vec<ImageKey> {
        let state = self.state.borrow();
        image_keys(&state.home, state.library.as_ref(), state.detail.as_ref())
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

    fn apply_detail(&self, generation: u64, detail: DetailView) {
        let route = self.app.current_route();
        if !accept_detail(
            *self.detail_generation.borrow(),
            generation,
            route.as_ref(),
            &detail.item_id,
        ) {
            tracing::debug!(
                item = detail.item_id,
                generation,
                ?route,
                "丢弃过期 / 与当前路由不一致的详情结果"
            );
            return;
        }
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
        self.request_images(ImageScope::Detail, keys);
    }

    /// 一批解码完成的图片：先全部放进 store，最后**只重贴当前路由**的视图一次。
    ///
    /// 以前每收到一张图就把 home / library / detail / search 全量重贴一遍，
    /// 深滚海外电影（4546 张）时 `convert::library_data` 每张图都重建一次整库
    /// 卡片；一批 N 张图现在只重建 1 次（其余页面的视图在切回时由 `set_page` 补上）。
    fn apply_images(&self, images: Vec<(ImageKey, DecodedImage)>) {
        for (key, decoded) in images {
            // 纹理上传属于界面线程：像素转 `slint::Image` 必须在这里做。
            let Some(image) = image_from_decoded(&decoded) else {
                continue;
            };
            self.app.set_image(&key, image);
        }
        // 内存上限：按字节预算淘汰最旧的图，但在屏上的图（各来源可见窗口的并集）
        // 一律保护。被淘汰的键解除"已请求"标记，滚回来时能重新装载。
        let evicted = {
            let protect = self.images.visible_keys();
            self.app.trim_images(&protect, IMAGE_STORE_BUDGET)
        };
        if !evicted.is_empty() {
            tracing::debug!(count = evicted.len(), "图片表超预算，淘汰旧图");
            self.images.forget(&evicted);
        }
        // 只重贴当前路由：离屏页面的模型不在这里重建。
        let route = self.app.current_route();
        let (home, library, detail, search) = {
            let state = self.state.borrow();
            (
                state.home.clone(),
                state.library.clone(),
                state.detail.clone(),
                state.search.clone(),
            )
        };
        match route {
            Some(Route::Home) | None => self.app.set_home(&home),
            Some(Route::Library { .. }) => {
                if let Some(library) = library {
                    self.app.set_library(&library);
                }
            }
            Some(Route::Detail { .. }) | Some(Route::Player { .. }) => {
                if let Some(detail) = detail {
                    self.app.set_detail(&detail);
                }
            }
            Some(Route::Search { .. }) => {
                if let Some(search) = search {
                    self.app.set_search(&search);
                }
            }
        }
    }

    /// 页面切换后释放离开页面的图片可见窗口（`set_page` 回调）。
    ///
    /// 不释放的话 `ImageScope::HomeRow(n)` / Library / Search / Detail 的 `visible`
    /// 集合永远停在离开时的值：`is_wanted` 恒为真、排队中的请求不会取消，这些
    /// 键在 `ImageStore::trim` 里永远被当成保护对象。重新进入页面时视图重新上报，
    /// 按磁盘缓存装载（不重复下载）。
    fn release_images_for_page(&self, page: Page) {
        if page != Page::Home {
            self.images.release_home_rows();
            // 预取只在首页有意义；离开时作废排队中的预取（只占 1 个独立名额，
            // 但仍然不该在用户打开大库时继续下载首页的图）。
            self.images.cancel_prefetch();
        }
        if page != Page::Library {
            self.images.release_scope(ImageScope::Library);
        }
        if page != Page::Search {
            self.images.release_scope(ImageScope::Search);
        }
        if page != Page::Detail {
            self.images.release_scope(ImageScope::Detail);
        } else {
            // 回到详情页（例如从播放页返回）：Detail scope 在离开时被释放了，
            // 这里重新上报一次当前详情的图（磁盘缓存命中，不重复下载）。
            // 只在路由里的条目与已持有的详情一致时才报：从首页点开另一部时，
            // `set_page` 先于 `Shell::open_detail` 取到新详情，这里报旧键是没用的
            // （新详情落地时 `open_detail` 自己会请求）。
            let keys = match self.app.current_route() {
                Some(Route::Detail { item_id }) => self
                    .state
                    .borrow()
                    .detail
                    .as_ref()
                    .filter(|detail| detail.item_id == item_id)
                    .map(DetailView::image_keys)
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            if !keys.is_empty() {
                self.request_images(ImageScope::Detail, keys);
            }
        }
    }

    fn open_detail(&self, item_id: &str) {
        if item_id == STATUS_ITEM_ID {
            return;
        }
        let generation = self.next_detail_generation();
        match self.local.detail(item_id) {
            Ok(Some(detail)) => {
                let keys = detail.image_keys();
                self.app.set_detail(&detail);
                self.state.borrow_mut().detail = Some(detail);
                self.request_images(ImageScope::Detail, keys);
            }
            Ok(None) => {
                let placeholder = detail_placeholder(item_id);
                self.app.set_detail(&placeholder);
                self.state.borrow_mut().detail = Some(placeholder);
            }
            Err(error) => tracing::warn!(%error, "读取详情缓存失败"),
        }
        self.spawn_detail(item_id.to_owned(), generation);
    }

    /// 下一次详情请求的代次。
    fn next_detail_generation(&self) -> u64 {
        let mut generation = self.detail_generation.borrow_mut();
        *generation = generation.wrapping_add(1);
        *generation
    }

    /// 详情页按需联网：`item()` + `sources()` 写回缓存后重建详情视图。
    fn spawn_detail(&self, item_id: String, generation: u64) {
        // 门控读 `Local` 的即时状态（不是 `self.state` 里可能更旧的快照）：
        // 离线时不联网，详情页继续用缓存渲染。
        let Some(provider) = online_provider(&self.local.status(), self.local.provider_dyn())
        else {
            return;
        };
        let local = Arc::clone(&self.local);
        let channel = Arc::clone(&self.channel);
        self.runtime.spawn(async move {
            let id = ItemId::new(item_id.clone());
            let item = match provider.item(&id).await {
                Ok(item) => item,
                Err(nova_provider::ProviderError::NoToken) => {
                    tracing::debug!(item = id.as_str(), "本机没有令牌，跳过详情联网刷新");
                    return;
                }
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
                Err(nova_provider::ProviderError::NoToken) => {
                    tracing::debug!(item = id.as_str(), "本机没有令牌，跳过播放源刷新");
                    return;
                }
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
                Ok(Ok(Some(detail))) => channel.post(UiMessage::Detail {
                    generation,
                    view: Box::new(detail),
                }),
                Ok(Ok(None)) => tracing::debug!(item = item_id, "详情写回后仍读不到条目"),
                Ok(Err(error)) => tracing::warn!(%error, "详情写缓存失败"),
                Err(error) => tracing::warn!(%error, "详情构建任务失败"),
            }
        });
    }

    fn play(self: &Rc<Self>, item_id: &str) {
        // 起播条目直接用**回调传进来的 id**：nova-ui 的 `play()` 已经按路由纠正过
        // （当前路由是 `Detail{id}` 时用路由里的 id，压 `Player{target}` 之后才回调），
        // 这里再按路由重算只会得到同一个值（以前那个“从播放页路由里取 id”的
        // 校验是循环的，等于没有防线）。
        //
        // 两道防线保证这个 id 是对的：
        // 1. [`accept_detail`]：迟到的详情在写回模型之前就被丢掉，界面不会贴上一部
        //    片子的 `detail.item-id`；
        // 2. nova-ui 的 `play()`：就算界面数据还差一帧没跟上，它也按当前路由纠正
        //    起播条目（体检 R7-D3）。
        if item_id == STATUS_ITEM_ID {
            return;
        }
        // 偏好只读一次：播放页初始位置与起播的续播位置 / 轨道用**同一份**，
        // 运行中改过偏好时两边不会分叉（体检 R7-D2）。
        let prefs = views::preferences_or_default(&self.local.db);
        let generation = self.next_detail_generation();
        match self.local.player_with_preferences(item_id, &prefs) {
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
        self.play_item(item_id, source, capture, prefs, &post);
        // 标题/进度需要真实条目；顺手刷新一次。
        self.spawn_detail(item_id.to_owned(), generation);
    }

    fn activate_tab(&self, library_id: &str) {
        // `HOME_TAB_ID` 是「首页」标签：库标签要等刷新完成才出现，没有这个入口
        // 用户进了媒体库就回不到首页。
        //
        // 这里必须**同时切页**：只重建首页数据的话，在媒体库/详情页点「首页」
        // 除了后台数据刷新什么都不会发生（交互扫描会把它记成"死按钮"）。
        if library_id == crate::local::HOME_TAB_ID {
            if let Ok(home) = self.local.home(None) {
                self.app.set_home(&home);
                self.prefetch_images(home.image_keys());
                self.state.borrow_mut().home = home;
            }
            self.app.show_home();
            return;
        }
        let sort = *self.library_sort.borrow();
        match self.local.library(library_id, None, sort) {
            Ok(view) => {
                // 图片由 `open_library` 内部的 `sync_grid_window` 按可见窗口上报。
                let _ = self.app.open_library(&view);
                self.state.borrow_mut().library = Some(view);
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

/// 详情页还没来得及读缓存时先贴的占位（标题「正在加载…」）。
///
/// **必须不可播放**：占位不知道条目类型，`DetailView::default()` 的 `playable` 是
/// `true`，直接拿它会先闪一下能点的「播放」（剧集第一次打开时尤其明显）。真实详情
/// 一到就整页替换。
fn detail_placeholder(item_id: &str) -> DetailView {
    let mut placeholder = DetailView::new(item_id, "正在加载…");
    placeholder.playable = false;
    placeholder
}

fn install_callbacks(shell: &Rc<Shell>) {
    {
        // 图片按可见窗口请求：网格 / 首页行的可见区间变了就补发新进入视口的图。
        let weak = Rc::downgrade(shell);
        shell.app.on_library_images(move |keys| {
            if let Some(shell) = weak.upgrade() {
                shell.request_images(ImageScope::Library, keys.to_vec());
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_search_images(move |keys| {
            if let Some(shell) = weak.upgrade() {
                shell.request_images(ImageScope::Search, keys.to_vec());
            }
        });
    }
    {
        let weak = Rc::downgrade(shell);
        shell.app.on_home_images(move |row, first, count| {
            if let Some(shell) = weak.upgrade() {
                shell.request_home_images(row, first, count);
            }
        });
    }
    {
        // 页面切走：释放离开页面的图片可见窗口（否则这些键永远被当成在屏上的图）。
        let weak = Rc::downgrade(shell);
        shell.app.on_page_changed(move |page| {
            if let Some(shell) = weak.upgrade() {
                shell.release_images_for_page(page);
            }
        });
    }
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
        let weak = Rc::downgrade(shell);
        shell.app.on_search(move || {
            if let Some(shell) = weak.upgrade() {
                // Ctrl+F 在详情页 / 播放页会先切到搜索页（nova-ui 直接打开空查询页）。
                // 上层的搜索镜像也得跟着清掉：否则晚到的图片（`apply_image` 按
                // "仍在搜索页"重贴 `state.search`）会把上一次的旧结果贴回空搜索页。
                if matches!(shell.app.current_route(), Some(Route::Search { .. })) {
                    let view = shell.app.search_view();
                    shell.state.borrow_mut().search = (!view.term.is_empty()).then_some(view);
                }
                tracing::debug!("搜索框已聚焦");
            }
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
                    // 状态就在 `RefreshFailed`，普通刷新会早退，必须强制重试。
                    shell.spawn_refresh(true);
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

/// `view.player` 的当前轨道探针：选中项的引擎编号（mpv 的 `aid` / `sid`）与标签。
///
/// `null` 表示没有选中（字幕“关闭”）或引擎还没枚举到轨道。以前选择状态只能
/// 从菜单行 label 猜，“字幕轨再进同一部片是否保留”没法直接验证（体检第四轮）。
fn selected_track_probe(tracks: &[TrackItem]) -> serde_json::Value {
    match tracks.iter().find(|track| track.selected) {
        Some(track) => serde_json::json!({
            "index": track.index,
            "label": track.label,
        }),
        None => serde_json::Value::Null,
    }
}

/// `view.dialog`：添加服务器对话框的探针。
///
/// 密码**只给长度**（按字符数，不是字节数）：回包会进脚本与日志，回显明文
/// 等于把凭据写出去。
fn dialog_probe(
    open: bool,
    address: &str,
    user: &str,
    password: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "open": open,
        "address": address,
        "user": user,
        "password_len": password.chars().count(),
        "error": error,
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
    scope: ImageScope,
    keys: Vec<ImageKey>,
    runtime: &Handle,
    channel: &Arc<UiChannel>,
) {
    let deliver_channel = Arc::clone(channel);
    images.request_window(local, scope, &keys, runtime, move |key, decoded| {
        deliver_channel.post(UiMessage::Image { key, decoded });
    });
}

fn prefetch_images(
    images: &Arc<ImageService>,
    local: &Arc<Local>,
    keys: Vec<ImageKey>,
    runtime: &Handle,
    channel: &Arc<UiChannel>,
) {
    let deliver_channel = Arc::clone(channel);
    images.request_prefetch(local, keys, runtime, move |key, decoded| {
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
    let (images, others) = {
        let mut queued = lock(queue);
        partition_queue(std::mem::take(&mut *queued))
    };
    // 非图片消息先落地（它们可能替换整页视图），再把这一批图片一次贴上去：
    // 一批 N 张图只触发 1 次视图重建（深滚大库时以前是 N 次整库重建）。
    for message in others {
        apply_message(message);
    }
    if !images.is_empty() {
        apply_images(images);
    }
}

/// 把一条消息放进队列（满了丢队首）。
///
/// 队列是有损的（`MAX_QUEUED_MESSAGES` 满了就丢最旧的一条），所以**状态消息不能
/// 跟图片抢队尾**：一次刷新落地时可能有几千张图在队列里，状态一旦被挤掉，横幅就
/// 永远停在上一个状态。这里对状态做一次合并：入队前把旧的 `Status` 全删掉，
/// 保证队列里最多一条，且一定是最新的那条。
fn enqueue(queue: &Arc<Mutex<VecDeque<UiMessage>>>, message: UiMessage) {
    let mut queue = lock(queue);
    if matches!(message, UiMessage::Status(_)) {
        queue.retain(|queued| !matches!(queued, UiMessage::Status(_)));
    }
    if queue.len() >= MAX_QUEUED_MESSAGES {
        queue.pop_front();
    }
    queue.push_back(message);
}

/// 把队列拆成"图片批次"与"其余消息"：一批图只重贴一次界面。
fn partition_queue(
    mut queue: VecDeque<UiMessage>,
) -> (Vec<(ImageKey, DecodedImage)>, Vec<UiMessage>) {
    let mut images = Vec::new();
    let mut others = Vec::new();
    while let Some(message) = queue.pop_front() {
        match message {
            UiMessage::Image { key, decoded } => images.push((key, decoded)),
            other => others.push(other),
        }
    }
    (images, others)
}

fn apply_message(message: UiMessage) {
    let Some(shell) = SHELL.with(|slot| slot.borrow().clone()) else {
        return;
    };
    match message {
        UiMessage::Refreshed(payload) => shell.apply_refresh(*payload),
        UiMessage::Detail { generation, view } => shell.apply_detail(generation, *view),
        UiMessage::Search { generation, view } => shell.apply_search_message(generation, *view),
        // `drain` 会把图片收进批次统一处理；这里保留单张的回退路径。
        UiMessage::Image { key, decoded } => shell.apply_images(vec![(key, decoded)]),
        UiMessage::Playback(message) => shell.apply_playback(message),
        UiMessage::AddServer(outcome) => shell.apply_add_server(*outcome),
        UiMessage::Status(status) => shell.apply_status(status),
    }
}

/// 一批图片 → 放进 store 并重贴当前路由（见 [`Shell::apply_images`]）。
fn apply_images(images: Vec<(ImageKey, DecodedImage)>) {
    let Some(shell) = SHELL.with(|slot| slot.borrow().clone()) else {
        return;
    };
    shell.apply_images(images);
}

/// 播放器的音量 / 静音快照。
///
/// 体检 R7-D4：`sweep` 会点到播放器控制层的 `player.volume` 滑杆与 `player.mute`。
/// 扫完不放回去的话，用户会发现片子被静音了、音量也被改到了别的值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerAudio {
    /// 音量 `0.0..=1.0`。
    pub volume: f32,
    /// 是否静音。
    pub muted: bool,
}

impl PlayerAudio {
    /// 从播放页视图取当前音量 / 静音。
    pub fn from_view(view: &PlayerView) -> Self {
        Self {
            volume: view.volume,
            muted: view.muted,
        }
    }
}

/// 把音量 / 静音恢复到快照；返回**扫描结束时音频是否与扫描前一致**。
///
/// 走界面的公开回调（与用户拖音量 / 点静音同一条路），引擎侧的音量也跟着回去。
/// 顺序有讲究：设非零音量会自动取消静音（见 `player_volume_set`），
/// 所以先恢复音量、再按需切回静音。
///
/// 返回值的语义是“**结果**”而不是“有没有动过”（体检 R9-C3）：什么都没变、无需恢复
/// 时也返回 `true`（与扫描前一致）。以前它报“恢复器写过界面没有”，于是库里 / 搜索页 /
/// 详情页这些根本没有音频改动的页面永远报 `audio_restored=false`，体检里看起来像
/// 恢复失败。真实失败只有一个判据：扫完仍与快照不同。
pub fn restore_player_audio(app: &App, snapshot: PlayerAudio) -> bool {
    if (app.player_view().volume - snapshot.volume).abs() > f32::EPSILON {
        app.ui().invoke_player_volume_set(snapshot.volume);
    }
    if app.player_view().muted != snapshot.muted {
        app.ui().invoke_player_mute_toggle();
    }
    PlayerAudio::from_view(&app.player_view()) == snapshot
}

/// 扫描的保护包装：先给音量 / 静音拍快照，跑扫描，再把快照恢复回去。
///
/// 抽成接受回调的接缝，测试才能验证「`Shell::sweep` 的包装确实调用了恢复」；
/// 只单独测 [`restore_player_audio`] 的话，把这里的恢复调用删掉测试照样过，
/// 等于没有覆盖。
pub(crate) fn sweep_preserving_audio<R>(app: &App, sweep: impl FnOnce() -> R) -> (bool, R) {
    // 恢复动作的返回值不用：判定统一在 [`sweep_with_audio_guard`] 里做（扫完的音频
    // 与快照比），避免“恢复器动过界面没有”与“结果是否一致”两套口径并存。
    sweep_with_audio_guard(
        app,
        |app, snapshot| {
            restore_player_audio(app, snapshot);
        },
        sweep,
    )
}

/// [`sweep_preserving_audio`] 的可测内核：`restore` 是恢复动作的接缝（真实路径是
/// [`restore_player_audio`]）。
///
/// 返回的 `restored` = **扫描结束时音量 / 静音与扫描前一致**（体检 R9-C3），
/// 而不是“恢复器动过界面没有”。恢复之后仍与快照不同就是**恢复失败**——以前
/// `Shell::sweep` 把 `restored` 丢掉，用户偏好被一次体检改掉也不会留下任何日志。
fn sweep_with_audio_guard<R>(
    app: &App,
    restore: impl FnOnce(&App, PlayerAudio),
    sweep: impl FnOnce() -> R,
) -> (bool, R) {
    let snapshot = PlayerAudio::from_view(&app.player_view());
    let result = sweep();
    restore(app, snapshot);
    let restored = PlayerAudio::from_view(&app.player_view()) == snapshot;
    if !restored {
        tracing::warn!("交互扫描改了音量 / 静音，恢复后仍与扫描前不同：用户偏好可能已被改掉");
    }
    (restored, result)
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

/// 一条详情结果是否应当上屏。
///
/// “代次 + 路由”双重判定只在这里实现一份：调用方只负责把当前值读出来。
///
/// 详情刷新没有取消机制：点开 A 又立刻点开 B 时，A 的响应可能后到。以前
/// `apply_detail` 不看路由也不看代次，迟到的 A 会直接盖掉 B 的详情页（海报 /
/// 标题 /
/// 播放按钮全指向 A），点「播放」就播错片（体检 R7-D3）。
///
/// 路由允许是 `Player`：起播后还会顺手刷一次详情（标题 / 播放文案），那时已经在
/// 播放页了。
pub(crate) fn accept_detail(
    current_generation: u64,
    message_generation: u64,
    route: Option<&Route>,
    item_id: &str,
) -> bool {
    if current_generation != message_generation {
        return false;
    }
    match route {
        Some(Route::Detail { item_id: current } | Route::Player { item_id: current }) => {
            current == item_id
        }
        _ => false,
    }
}

/// 这次请求要不要真的联网：只有已连接（令牌可用）时才用 provider。
///
/// 搜索、详情按需刷新、图片下载共用这一道门。离线（未登录 / 令牌过期 / 没有
/// 服务器）时 `Local` 里可能仍然有 provider（构造它不要求有令牌），但它只会返回
/// [`ProviderError::NoToken`]；把这种本地状态当成服务端 401 会误写 `Expired`。
///
/// 搜索侧：本地缓存命中就是最终结果。以前照样发一次注定失败的请求，它的错误页
/// 会在几秒后把本地结果整页替换成“搜索失败”——而本地搜索正是离线时唯一的搜索
/// （也是输入时的即时反馈）。
pub(crate) fn online_provider(
    status: &Status,
    provider: Option<Arc<dyn MediaProvider>>,
) -> Option<Arc<dyn MediaProvider>> {
    if status.is_connected() {
        provider
    } else {
        None
    }
}

/// 这次状态变化是不是“从离线恢复成在线”（重新登录 / 刷新成功）。
///
/// 只有这个方向才作废图片的永久失败记录（见 [`ImageService::clear_failures`]）：
/// 反过来（在线 → 离线）不动，否则断网时一次状态广播就把失败记录全洗掉，
/// 恢复在线后又去重试那些真的是坏图（404）的键。
pub(crate) fn recovered_session(previous: &Status, next: &Status) -> bool {
    !previous.is_connected() && next.is_connected()
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
    use super::{LibraryView, Route, Status, online_provider, recovered_session};
    use super::{
        PlayerAudio, accept_search_result, debounce_still_current, refresh_library_target,
        sweep_preserving_audio,
    };
    use super::{accept_detail, sweep_with_audio_guard};
    use std::rc::Rc;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    use nova_ui::{App, AppWindow, Page};

    use nova_core::{
        AuthState, ItemId, Language, MediaItem, MediaKind, ServerId, ServerInfo, ServerKind,
        UserData,
    };
    use nova_provider::Library;

    use slint::ComponentHandle;

    use crate::views;

    #[test]
    fn stale_generation_is_rejected() {
        assert!(!accept_search_result(2, 1, true, "沙丘", "沙丘"));
    }

    /// 加载占位不能自己带一个「播放」按钮：`DetailView::default()` 默认能播，
    /// 剧集第一次打开时会先闪一下可点的播放（R9-A2 复查）。
    #[test]
    fn the_detail_loading_placeholder_is_not_playable() {
        let placeholder = super::detail_placeholder("s");
        assert_eq!(placeholder.item_id, "s");
        assert_eq!(placeholder.title, "正在加载…");
        assert!(!placeholder.playable, "加载占位不该有播放入口");
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

    /// 体检 R7-D3：路由已经是 B 时，迟到的 A 详情必须被丢掉。
    ///
    /// 以前 `apply_detail` 不看路由也不看代次：点开 A 又立刻点开 B，A 的响应后到就会
    /// 把 B 的详情页（标题 / 海报 / 播放按钮）整页盖成 A。
    #[test]
    fn detail_for_another_item_is_rejected() {
        let route = Some(Route::Detail {
            item_id: "b".to_owned(),
        });
        assert!(!accept_detail(1, 1, route.as_ref(), "a"));
        assert!(accept_detail(1, 1, route.as_ref(), "b"));
    }

    /// 同一路由下的早先一次请求也不能事后翻盘（两次点开同一个条目）。
    #[test]
    fn stale_detail_generation_is_rejected() {
        let route = Some(Route::Detail {
            item_id: "b".to_owned(),
        });
        assert!(!accept_detail(2, 1, route.as_ref(), "b"));
        assert!(accept_detail(2, 2, route.as_ref(), "b"));
    }

    /// 已经离开详情页：迟到结果不能写回任何模型（下次打开别的条目会短暂显示它）。
    #[test]
    fn detail_off_the_detail_route_is_rejected() {
        let home = Some(Route::Home);
        assert!(!accept_detail(1, 1, home.as_ref(), "b"));
        assert!(!accept_detail(1, 1, None, "b"));
    }

    /// 起播后还会顺手刷一次详情，那时路由是播放页 —— 必须接受，否则标题 / 播放文案不更新。
    #[test]
    fn detail_is_accepted_on_the_player_route_for_the_same_item() {
        let player = Some(Route::Player {
            item_id: "b".to_owned(),
        });
        assert!(accept_detail(1, 1, player.as_ref(), "b"));
        assert!(!accept_detail(1, 1, player.as_ref(), "a"));
    }

    /// 体检 R7-D4：`Shell::sweep` 的保护包装必须真的调用恢复。
    ///
    /// 这里直接走包装（与 `Shell::sweep` 同一条实现），模拟扫描点到音量滑杆 /
    /// 静音按钮的副作用，再断言包装在返回扫描结果前把两者都放回去了。
    /// 删掉包装里的 `restore_player_audio` 调用，这条用例会失败。
    #[test]
    fn sweep_wrapper_restores_the_player_audio() {
        init_test_backend();
        let app = App::new_with(AppWindow::new().expect("窗口"));
        let mut view = app.player_view();
        view.volume = 0.42;
        view.muted = true;
        app.set_player(&view);
        let before = PlayerAudio::from_view(&app.player_view());

        let (restored, sweep_result) = sweep_preserving_audio(&app, || {
            app.ui().invoke_player_volume_set(0.93);
            app.ui().invoke_player_mute_toggle();
            7
        });

        assert_eq!(sweep_result, 7, "包装要原样返回扫描结果");
        assert!(restored, "扫描改了音量 / 静音，包装必须调用恢复");
        assert_eq!(
            PlayerAudio::from_view(&app.player_view()),
            before,
            "扫描结束后音量与静音都要回到扫描前的值"
        );
    }

    /// 体检 R7-D4：走真实 `Shell::sweep`（不是它内部的接缝）确认音频被放回去了。
    ///
    /// 以前只测 `sweep_preserving_audio`：把 `Shell::sweep` 里那次调用删掉，
    /// 测试依旧绿。这里直接调 `Shell::sweep`，断它返回的 `audio_restored`
    /// 与扫描后的音量 / 静音——删掉调用就会失败（音量留在扫描点到的新值上）。
    #[test]
    fn shell_sweep_restores_the_player_audio_it_clicks() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(crate::local::Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        let mut view = app.player_view();
        view.volume = 0.42;
        view.muted = true;
        app.set_player(&view);
        app.show_page(Page::Player);
        app.ui().show().expect("显示窗口");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1200));
        // 目标登记要跑两轮（同 `interaction_sweep` 的 `scan`）。
        for _ in 0..2 {
            app.refresh_targets();
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(40));
        }
        assert!(
            app.target("player.mute").is_some(),
            "前提：扫描真的会点到静音按钮"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let shell = super::Shell::for_test(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
        );
        let before = PlayerAudio::from_view(&app.player_view());

        let report = shell.sweep(false);

        assert_eq!(report["ok"], serde_json::json!(true), "{report}");
        assert_eq!(
            report["audio_restored"],
            serde_json::json!(true),
            "扫描改过音量 / 静音，`Shell::sweep` 必须真的恢复（否则用户偏好被体检改掉）：{report}"
        );
        assert_eq!(
            PlayerAudio::from_view(&app.player_view()),
            before,
            "扫描后音量与静音都要回到扫描前的值"
        );
    }

    /// 体检 R9-C3：`audio_restored` 的语义是“**扫描结束时音频与扫描前一致**”。
    ///
    /// 扫描根本没碰音量 / 静音（库页 / 搜索页 / 详情页没有播放器）时也必须报 `true`。
    /// 以前它是“恢复器写过界面没有”，这些页面恒定报 `false`，体检里看起来像恢复失败。
    #[test]
    fn shell_sweep_reports_audio_consistent_when_nothing_touched_it() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(crate::local::Local::open(&env, None).expect("打开本地状态"));
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        app.show_page(Page::Library);
        app.ui().show().expect("显示窗口");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(200));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let shell = super::Shell::for_test(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
        );
        let before = PlayerAudio::from_view(&app.player_view());

        let report = shell.sweep(false);

        assert_eq!(report["ok"], serde_json::json!(true), "{report}");
        assert_eq!(
            report["audio_restored"],
            serde_json::json!(true),
            "扫描没动过音量 / 静音时音频与扫描前一致，必须报 true（R9-C3）：{report}"
        );
        assert_eq!(
            PlayerAudio::from_view(&app.player_view()),
            before,
            "库页扫描不该改音量 / 静音"
        );
        assert_eq!(
            report["aborted"],
            serde_json::json!(false),
            "库页扫描不该提前收尾：{report}"
        );
        assert_eq!(
            report["abort_reason"],
            serde_json::Value::Null,
            "没提前收尾时没有原因：{report}"
        );
    }

    /// `sweep` 恢复搜索视图后，调试通道的 `view.search` 摘要也要跟着回填。
    ///
    /// `view.search` 读的是 `nova-app` 本层的 `UiState.search` 镜像；恢复器写的是
    /// `nova-ui` 的视图模型。扫描点到 `search.clear` 时镜像被 `Shell::clear_search`
    /// 清成 `None`，不同步的话扫描结束明明回到了搜索页，`view.search` 还是 `null`。
    #[test]
    fn shell_sweep_refills_the_search_summary_after_restore() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server_info(), 1).expect("写服务器");
            db.upsert_items(
                &ServerId::new("srv-a"),
                &[item("dune", "沙丘", "lib-movies")],
                1,
            )
            .expect("写条目");
        }
        let local = Arc::new(crate::local::Local::open(&env, None).expect("打开本地状态"));
        local.set_libraries(vec![library("lib-movies", "电影")]);
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        app.ui().set_window_w(1280.0);
        app.ui().set_window_h(800.0);
        app.set_home(&local.home(None).expect("首页"));
        let view = local.search_view("沙丘", 60).expect("搜索视图");
        assert!(!view.cards.is_empty(), "前提：「沙丘」有结果");
        app.open_search(&view);
        app.ui().show().expect("显示窗口");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1200));
        for _ in 0..2 {
            app.refresh_targets();
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(40));
        }
        assert!(
            app.target("search.clear").is_some(),
            "前提：扫描真的会点到清空（镜像会被清成 None）"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let shell = super::Shell::for_test(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
        );

        let report = shell.sweep(false);
        assert_eq!(report["ok"], serde_json::json!(true), "{report}");
        let view = shell.debug_execute(crate::debug::Command::View);
        assert_eq!(
            view["search"]["term"],
            serde_json::json!("沙丘"),
            "sweep 恢复搜索视图后，`view.search` 摘要也要同步回填：{view}"
        );
        assert_eq!(
            view["search"]["count"],
            serde_json::json!(1),
            "恢复后的结果数也要跟视图一致：{view}"
        );
    }

    /// 离开搜索页后，在首页跑 `sweep` 不能把旧的 SearchView 写回镜像。
    ///
    /// 修复前 `Shell::sweep` 无条件拿 `App::search_view()` 回填
    /// `UiState.search`：`clear_search`（Esc 回首页）只清本层镜像、不碰
    /// `nova-ui` 的搜索视图，于是首页扫一次就会把上一次的旧查询又写回
    /// `view.search`，调试通道看上去像"搜索还开着"。
    #[test]
    fn shell_home_sweep_keeps_the_search_summary_empty() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        {
            let db = nova_storage::Database::open(dir.path().join("nova.db")).expect("建库");
            db.upsert_server(&server_info(), 1).expect("写服务器");
            db.upsert_items(
                &ServerId::new("srv-a"),
                &[item("dune", "沙丘", "lib-movies")],
                1,
            )
            .expect("写条目");
        }
        let local = Arc::new(crate::local::Local::open(&env, None).expect("打开本地状态"));
        local.set_libraries(vec![library("lib-movies", "电影")]);
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        app.ui().set_window_w(1280.0);
        app.ui().set_window_h(800.0);
        app.set_home(&local.home(None).expect("首页"));
        let view = local.search_view("沙丘", 60).expect("搜索视图");
        assert!(!view.cards.is_empty(), "前提：「沙丘」有结果");
        app.open_search(&view);
        app.ui().show().expect("显示窗口");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1200));
        for _ in 0..2 {
            app.refresh_targets();
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(40));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let shell = super::Shell::for_test(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
        );

        // Esc 回首页：本层镜像被清空，`nova-ui` 的搜索视图还留着上一次的结果。
        shell.clear_search();
        assert!(
            !app.search_view().term.is_empty(),
            "前提：`nova-ui` 还留着旧视图"
        );
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1200));
        for _ in 0..2 {
            app.refresh_targets();
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(40));
        }

        let report = shell.sweep(false);
        assert_eq!(report["ok"], serde_json::json!(true), "{report}");
        let view = shell.debug_execute(crate::debug::Command::View);
        assert_eq!(
            view["search"],
            serde_json::json!(null),
            "离开搜索页后跑 sweep 不能把旧 SearchView 写回镜像：{view}"
        );
    }

    /// 体检 R8-A2：调试通道 `tab` 切库后，`view.route` 必须报**新库**。
    ///
    /// 修复前 `App::open_library` 只在"栈顶不是 `Route::Library`"时压栈，切库后
    /// 栈顶仍是第一个进的库：页面、`current_tab`、标题都对，只有 `view.route`
    /// （`App::current_route()` 的 `Debug` 输出）停在旧 id 上。那是真的路由栈没更新，
    /// 不只是探针取值不对——快照重建之类的"当前路由 == 某库"判断会因此对着旧库做。
    #[test]
    fn tab_command_moves_the_route_to_the_new_library() {
        init_test_backend();
        let dir = tempfile::tempdir().expect("临时目录");
        let env = crate::env::Env {
            data_dir: dir.path().to_path_buf(),
            credentials: crate::env::CredentialMode::Memory,
        };
        let local = Arc::new(crate::local::Local::open(&env, None).expect("打开本地状态"));
        // 两个库标签：离线也能走 `activate_tab` → `Local::library` 这条真实路径。
        local.set_libraries(vec![
            library("lib-a", "动画电影"),
            library("lib-b", "海外电影"),
        ]);
        let app = Rc::new(App::new_with(AppWindow::new().expect("窗口")));
        app.ui()
            .window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        app.ui().set_window_w(1280.0);
        app.ui().set_window_h(800.0);
        app.set_home(&local.home(None).expect("首页"));
        app.show_home();
        app.ui().show().expect("显示窗口");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let shell = super::Shell::for_test(
            Rc::clone(&app),
            Arc::clone(&local),
            runtime.handle().clone(),
        );

        let route = |id: &str, title: &str| {
            format!(
                "{:?}",
                Some(Route::Library {
                    id: id.to_owned(),
                    title: title.to_owned(),
                })
            )
        };

        for (id, title) in [("lib-a", "动画电影"), ("lib-b", "海外电影")] {
            let report = shell.debug_execute(crate::debug::Command::Tab {
                library: id.to_owned(),
            });
            assert_eq!(report["ok"], serde_json::json!(true), "{report}");
            let view = shell.debug_execute(crate::debug::Command::View);
            assert_eq!(
                view["route"],
                serde_json::json!(route(id, title)),
                "`tab {id}` 之后 `view.route` 必须是新库（页面与高亮已经切过去了）：{view}"
            );
            assert_eq!(shell.app.page(), Page::Library);
        }

        // 每次切库都是替换栈顶：返回仍然只回一层（首页）。
        assert_eq!(shell.app.navigator().len(), 2, "栈里是 [首页, 库]");
        assert!(shell.app.go_back());
        assert_eq!(shell.app.page(), Page::Home, "返回落在首页，不需要先回旧库");
    }

    /// 体检 R7-D4：扫描改了音量 / 静音而恢复**没生效**时必须留下 warn。
    ///
    /// `Shell::sweep` 需要真实事件循环才能构造，所以这里走它调用的同一个包装。
    /// 以前 `restored` 被直接丢掉，恢复失败完全静默（用户偏好被体检改掉也没人知道）。
    #[test]
    fn sweep_warns_when_the_audio_never_came_back() {
        init_test_backend();
        let app = App::new_with(AppWindow::new().expect("窗口"));
        let mut view = app.player_view();
        view.volume = 0.42;
        app.set_player(&view);

        // 恢复动作故意“失败”（不写回界面）：与真实恢复走同一条判定。
        let logs = capture_logs(|| {
            let (restored, _) = sweep_with_audio_guard(
                &app,
                |_app, _snapshot| {},
                || app.ui().invoke_player_volume_set(0.93),
            );
            assert!(!restored, "扫完仍与扫描前不同时不能返回 true");
        });
        assert!(
            logs.contains("恢复后仍与扫描前不同"),
            "恢复失败必须留下 warn：{logs}"
        );

        // 正常路径（真实恢复）不报警：否则每次体检都刷一条假失败。
        let quiet = capture_logs(|| {
            let (restored, _) = sweep_preserving_audio(&app, || {
                app.ui().invoke_player_volume_set(0.31);
            });
            assert!(restored);
        });
        assert!(quiet.is_empty(), "正常恢复不该有 warn：{quiet}");
    }

    /// 收集一段代码产生的日志（`with_default` 只在本线程与当前 scope 生效）。
    fn capture_logs(run: impl FnOnce()) -> String {
        /// 把日志写进共享缓冲。
        #[derive(Clone, Default)]
        struct LogSink(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for LogSink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let mut buffer = self
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                buffer.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = LogSink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(sink.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        let bytes = sink
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn init_test_backend() {
        let backend = i_slint_backend_testing::TestingBackend::new(
            i_slint_backend_testing::TestingBackendOptions {
                mock_time: true,
                threading: false,
                renderer_name: Some("software".into()),
            },
        );
        let _ = slint::platform::set_platform(Box::new(backend));
    }

    fn dummy_provider() -> Arc<dyn nova_provider::MediaProvider> {
        use nova_provider::{Flavor, Provider, ProviderConfig};
        let store: Arc<dyn nova_secrets::CredentialStore> =
            Arc::new(nova_secrets::MemoryStore::new());
        Arc::new(
            Provider::new(
                Flavor::Jellyfin,
                ProviderConfig::new(ServerId::new("srv-test"), "http://127.0.0.1:8096"),
                store,
            )
            .expect("构造测试 provider"),
        )
    }

    /// 内存凭证模式（`NOVA_CREDENTIALS=memory`）下没有令牌，但本地缓存完整：
    /// 打开详情 / 起播只能读缓存，不能把“本机没令牌”当成服务端 401 写进本地库。
    #[test]
    fn offline_detail_and_playback_keep_the_auth_state() {
        use crate::env::{CredentialMode, Env};
        use crate::local::Local;
        use nova_storage::Database;

        let dir = tempfile::tempdir().expect("临时目录");
        let server_id = "srv-offline-detail";
        {
            let db = Database::open(dir.path().join("nova.db")).expect("建库");
            let mut server = server_info();
            server.id = ServerId::new(server_id);
            server.auth = AuthState::Authorized {
                user_name: "阿明".to_owned(),
                credential_key: format!("jellyfin/{server_id}/token"),
            };
            db.upsert_server(&server, 1).expect("写服务器");
            db.upsert_items(
                &ServerId::new(server_id),
                &[item("m1", "沙丘", "movies")],
                1,
            )
            .expect("写条目");
            db.upsert_sources(
                &ServerId::new(server_id),
                &[nova_core::MediaSource {
                    id: nova_core::SourceId::new("src-m1"),
                    item_id: ItemId::new("m1"),
                    name: None,
                    container: None,
                    size: None,
                    bitrate: None,
                    runtime: None,
                    streams: Vec::new(),
                }],
            )
            .expect("写播放源");
        }
        let env = Env {
            data_dir: dir.path().to_path_buf(),
            credentials: CredentialMode::Memory,
        };
        let local = Local::open(&env, None).expect("打开状态目录");
        assert!(
            matches!(local.status(), Status::MissingToken { .. }),
            "没有令牌应当是 MissingToken：{:?}",
            local.status()
        );
        assert!(
            local.provider_dyn().is_some(),
            "provider 允许没有令牌（否则应用内登录也没法复用）"
        );

        // 打开详情 / 起播时的那道门：没有已连接状态就不应该拿到 provider。
        assert!(
            online_provider(&local.status(), local.provider_dyn()).is_none(),
            "离线时详情 / 起播不能联网"
        );
        // 缓存仍然能渲染出详情与播放页（离线浏览是设计目标）。
        let detail = local.detail("m1").expect("读详情缓存").expect("有详情");
        assert_eq!(detail.item_id, "m1");
        let player = local.player("m1").expect("读播放页缓存").expect("有播放页");
        assert_eq!(player.title, "沙丘");

        // 一次假 401 都不应该发生：数据库里的授权状态原样保留。
        let stored = local
            .db
            .server(&ServerId::new(server_id))
            .expect("读服务器")
            .expect("服务器还在");
        assert!(
            stored.info.auth.is_authorized(),
            "离线浏览不能把会话写成 Expired：{:?}",
            stored.info.auth
        );
    }

    /// 离线时不再发服务端搜索：否则它的错误页会把本地命中整页替换掉
    /// （本地搜索是离线时唯一的搜索）。
    #[test]
    fn offline_status_never_starts_a_server_search() {
        let provider = dummy_provider();
        for status in [
            Status::NoServer,
            Status::AmbiguousServer { count: 2 },
            Status::MissingToken {
                url: "http://127.0.0.1:8096".to_owned(),
                note: None,
            },
            Status::Expired {
                url: "http://127.0.0.1:8096".to_owned(),
            },
            Status::RefreshFailed {
                message: "网络错误".to_owned(),
            },
        ] {
            assert!(
                online_provider(&status, Some(Arc::clone(&provider))).is_none(),
                "{status:?} 不该发起服务端搜索"
            );
        }
        let connected = Status::Connected {
            server: "测试".to_owned(),
            base_url: "http://127.0.0.1:8096".to_owned(),
        };
        assert!(
            online_provider(&connected, Some(provider)).is_some(),
            "已连接时才发服务端搜索"
        );
        assert!(
            online_provider(&connected, None).is_none(),
            "没有 provider 时也不发"
        );
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

    // ---- 调试通道 `view` 的序列化（体检第四轮的工具缺口）----

    #[test]
    fn view_player_reports_the_selected_tracks() {
        use nova_ui::TrackItem;

        let audio = vec![
            TrackItem::new(1, "国语 (EAC3)"),
            TrackItem::new(2, "英语 (AAC)").with_selected(true),
        ];
        assert_eq!(
            super::selected_track_probe(&audio),
            serde_json::json!({ "index": 2, "label": "英语 (AAC)" }),
            "音轨探针要给出选中项的引擎编号与标签"
        );

        // 字幕选“关闭”时没有任何选中项，而且空列表也不能编出一个假轨道。
        let subtitles = vec![TrackItem::new(3, "简体中文 (SRT)")];
        assert_eq!(
            super::selected_track_probe(&subtitles),
            serde_json::Value::Null
        );
        assert_eq!(super::selected_track_probe(&[]), serde_json::Value::Null);
    }

    #[test]
    fn view_dialog_reports_only_the_password_length() {
        let probe = super::dialog_probe(
            true,
            "http://127.0.0.1:8096",
            "alice",
            "pässwörd",
            "地址不可达",
        );
        assert_eq!(probe["open"], true);
        assert_eq!(probe["address"], "http://127.0.0.1:8096");
        assert_eq!(probe["user"], "alice");
        assert_eq!(
            probe["password_len"], 8,
            "密码长度按字符数算（不是 UTF-8 字节数）"
        );
        assert_eq!(probe["error"], "地址不可达");
        let text = probe.to_string();
        assert!(!text.contains("pässwörd"), "回包里不能有明文密码：{text}");
        assert!(
            !text.contains("\"password\""),
            "只允许 password_len：{text}"
        );
    }

    #[test]
    fn debug_key_text_accepts_cjk_single_characters() {
        // 调试通道 `{"cmd":"key","key":"沙"}`：CJK 单字符必须原样变成按键文本，
        // 才能作为字符事件送进当前聚焦的输入框（与 `type` 同一条注入路径）。
        assert_eq!(super::key_event_text("沙").as_deref(), Some("沙"));
        assert_eq!(super::key_event_text("A").as_deref(), Some("A"));
        assert_eq!(
            super::key_event_text("Enter"),
            Some(slint::platform::Key::Return.into())
        );
        // 多字符不是单键；未知命名键也不行（`key_event_text` 返回 None）。
        assert_eq!(super::key_event_text("沙丘"), None);
        assert_eq!(super::key_event_text("F13"), None);
    }

    /// 只有“离线 → 在线”才需要作废图片失败记录（重新登录 / 刷新成功）。
    #[test]
    fn only_an_offline_to_online_transition_counts_as_recovery() {
        let offline = Status::RefreshFailed {
            message: "网络错误".to_owned(),
        };
        let expired = Status::Expired {
            url: "http://127.0.0.1:8096".to_owned(),
        };
        let connected = Status::Connected {
            server: "测试".to_owned(),
            base_url: "http://127.0.0.1:8096".to_owned(),
        };
        assert!(recovered_session(&offline, &connected));
        assert!(recovered_session(&expired, &connected));
        assert!(recovered_session(&Status::NoServer, &connected));
        assert!(
            !recovered_session(&connected, &offline),
            "在线 → 离线不作废失败记录，否则断网一次就把真坏图（404）也洗掉了"
        );
        assert!(!recovered_session(&connected, &connected), "状态没变不用清");
    }

    /// 状态消息走的是有损队列：多次 `post` 之后只能留下最后一条（否则横幅会停在
    /// 旧状态）。
    #[test]
    fn status_messages_are_coalesced_in_the_queue() {
        use std::collections::VecDeque;
        use std::sync::Mutex;

        use nova_images::DecodedImage;
        use nova_ui::ImageKey;

        use super::{MAX_QUEUED_MESSAGES, UiMessage, enqueue, lock};

        // 队列塞满（模拟刷新落地时几千张海报晚到）：状态仍然要能落到最终队列里。
        let queue: Arc<Mutex<VecDeque<UiMessage>>> = Arc::new(Mutex::new(VecDeque::new()));
        for index in 0..MAX_QUEUED_MESSAGES {
            enqueue(
                &queue,
                UiMessage::Image {
                    key: ImageKey::poster(format!("p{index}")),
                    decoded: DecodedImage {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    },
                },
            );
        }
        enqueue(
            &queue,
            UiMessage::Status(Status::RefreshFailed {
                message: "第一次".to_owned(),
            }),
        );
        enqueue(
            &queue,
            UiMessage::Status(Status::Expired {
                url: "http://127.0.0.1:8096".to_owned(),
            }),
        );

        let queued = lock(&queue);
        let statuses: Vec<&UiMessage> = queued
            .iter()
            .filter(|message| matches!(message, UiMessage::Status(_)))
            .collect();
        assert_eq!(statuses.len(), 1, "状态只保留最后一条");
        assert!(
            matches!(statuses[0], UiMessage::Status(Status::Expired { .. })),
            "留下的必须是最新状态（旧的晚到会把横幅覆盖回旧值）"
        );
        assert_eq!(queued.len(), MAX_QUEUED_MESSAGES, "仍然受队列上限约束");
    }

    /// 💡1：一批 N 张图只触发 **1 次**视图重建。
    ///
    /// 修复前 `drain` 每弹出 `UiMessage::Image` 就调一次 `apply_image`，而
    /// `apply_image` 会把 home / library / detail / search 全部重贴一遍（
    /// `convert::library_data` 每次重建整库 4546 张卡）——深滚时每张图一次。
    /// 现在 `partition_queue` 把队列里的图全部收成**一个**批次，`drain` 只调用
    /// 一次 `apply_images`，所以“批次数 = 1”就是“视图重建次数 = 1”。
    #[test]
    fn a_batch_of_images_is_applied_once() {
        use std::collections::VecDeque;

        use nova_images::DecodedImage;
        use nova_ui::{DetailView, ImageKey};

        use super::{UiMessage, partition_queue};

        fn decoded() -> DecodedImage {
            DecodedImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
            }
        }

        let mut queue = VecDeque::new();
        for index in 0..5 {
            queue.push_back(UiMessage::Image {
                key: ImageKey::poster(format!("p{index}")),
                decoded: decoded(),
            });
        }
        queue.push_back(UiMessage::Detail {
            generation: 1,
            view: Box::new(DetailView::new("x", "X")),
        });

        let (images, others) = partition_queue(queue);
        assert_eq!(images.len(), 5, "5 张图必须收成同一个批次");
        assert_eq!(others.len(), 1, "非图片消息保持原样");
        assert_eq!(images[0].0, ImageKey::poster("p0"));
        assert_eq!(images[4].0, ImageKey::poster("p4"));
    }
}
