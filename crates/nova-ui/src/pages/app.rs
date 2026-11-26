// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 应用绑定：把 Slint 回调接到 Rust，维护焦点索引与导航栈。
//!
//! 页面组件本身只负责摆放；键盘意图、路由、焦点移动都在这里与
//! `viewmodel` 中完成，`.slint` 里不写业务判断。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use i_slint_core::window::WindowInner;
use slint::{ComponentHandle, Model};

use crate::pages::{add_server, assets::ImageStore, banner, detail, home, library, player, search};
use crate::viewmodel::{
    AddServerError, AddServerView, BannerData, DetailView, HomeView, ImageKey, LibraryView,
    Navigator, PlayerView, Route, SearchView, TargetInfo, move_focus, rebase_focus,
};
use crate::{AppWindow, Target, TargetBus};

/// 页面编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    /// 首页。
    Home = 0,
    /// 媒体库。
    Library = 1,
    /// 详情页。
    Detail = 2,
    /// 播放外壳。
    Player = 3,
    /// 搜索页（独立结果页）。
    Search = 4,
}

impl Page {
    /// Slint 里的页面编号。
    pub const fn index(self) -> i32 {
        self as i32
    }

    /// 从 Slint 的页面编号还原；未知值按首页处理。
    pub const fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Library,
            2 => Self::Detail,
            3 => Self::Player,
            4 => Self::Search,
            _ => Self::Home,
        }
    }
}

/// Slint 的按键意图编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCommand {
    /// ← 行内左移。
    Left,
    /// → 行内右移。
    Right,
    /// ↑ 上移一行。
    Up,
    /// ↓ 下移一行。
    Down,
    /// Enter / Space：打开或播放。
    Accept,
    /// Esc / Backspace：返回。
    Back,
    /// Ctrl+F：聚焦搜索。
    Search,
    /// Ctrl+N：打开“添加服务器”对话框。
    ConnectServer,
}

impl KeyCommand {
    /// 把 `.slint` 里的意图编号转成命令；未知编号返回 `None`。
    pub const fn from_intent(code: i32) -> Option<Self> {
        match code {
            1 => Some(Self::Left),
            2 => Some(Self::Right),
            3 => Some(Self::Up),
            4 => Some(Self::Down),
            5 => Some(Self::Accept),
            6 => Some(Self::Back),
            7 => Some(Self::Search),
            8 => Some(Self::ConnectServer),
            _ => None,
        }
    }
}

/// 播放页的按键意图（`app.slint` 只做键位 → 编号的映射）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlayerKey {
    /// `Space` / `K`：播放暂停。
    TogglePlay,
    /// `←`：退 15 秒。
    SeekBack15,
    /// `→`：进 15 秒。
    SeekForward15,
    /// `Shift+←`：退 60 秒。
    SeekBack60,
    /// `Shift+→`：进 60 秒。
    SeekForward60,
    /// `↑`：音量 +5%。
    VolumeUp,
    /// `↓`：音量 -5%。
    VolumeDown,
    /// `M`：静音开关。
    Mute,
    /// `A`：音轨菜单。
    AudioMenu,
    /// `S`：字幕菜单。
    SubtitleMenu,
    /// `F`：全屏开关。
    Fullscreen,
    /// `Esc` / `Backspace`：先提交进度再退出。
    Exit,
}

impl PlayerKey {
    /// 把 `.slint` 里的播放页意图编号转成命令；未知编号返回 `None`。
    pub const fn from_intent(code: i32) -> Option<Self> {
        match code {
            1 => Some(Self::TogglePlay),
            2 => Some(Self::SeekBack15),
            3 => Some(Self::SeekForward15),
            4 => Some(Self::SeekBack60),
            5 => Some(Self::SeekForward60),
            6 => Some(Self::VolumeUp),
            7 => Some(Self::VolumeDown),
            8 => Some(Self::Mute),
            9 => Some(Self::AudioMenu),
            10 => Some(Self::SubtitleMenu),
            11 => Some(Self::Fullscreen),
            12 => Some(Self::Exit),
            _ => None,
        }
    }
}

/// 倍速循环顺序。
const RATE_STEPS: [f32; 5] = [1.0, 1.25, 1.5, 2.0, 0.75];

type StrHandler = Rc<dyn Fn(&str)>;
type SubmitHandler = Rc<dyn Fn(&str, &str, &str)>;
type VoidHandler = Rc<dyn Fn()>;
type BoolHandler = Rc<dyn Fn(bool)>;
type FloatHandler = Rc<dyn Fn(f32)>;
type SeekHandler = Rc<dyn Fn(Duration)>;
type TrackHandler = Rc<dyn Fn(u32)>;
type SubtitleHandler = Rc<dyn Fn(Option<u32>)>;
type SecondsHandler = Rc<dyn Fn(f64)>;
type MarginHandler = Rc<dyn Fn(i64)>;

#[derive(Default)]
struct Callbacks {
    open_detail: Option<StrHandler>,
    play: Option<StrHandler>,
    /// 详情页「我的列表」切换。
    detail_my_list_toggle: Option<VoidHandler>,
    back: Option<VoidHandler>,
    search: Option<VoidHandler>,
    search_changed: Option<StrHandler>,
    search_submit: Option<VoidHandler>,
    search_clear: Option<VoidHandler>,
    /// 搜索页「×」：清空关键词但留在搜索页（与 Esc 的 `search_clear` 分开）。
    search_clear_input: Option<VoidHandler>,
    // 搜索页自己的输入框 / 重试按钮（顶栏的输入框仍走上面的 search_changed / submit）。
    search_input: Option<StrHandler>,
    search_retry: Option<VoidHandler>,
    banner_action: Option<VoidHandler>,
    sort: Option<VoidHandler>,
    tab: Option<StrHandler>,
    source: Option<StrHandler>,
    // 播放页
    player_play_pause: Option<BoolHandler>,
    player_seek: Option<SeekHandler>,
    player_volume: Option<FloatHandler>,
    player_muted: Option<BoolHandler>,
    player_rate: Option<FloatHandler>,
    player_audio_track: Option<TrackHandler>,
    player_subtitle_track: Option<SubtitleHandler>,
    player_retry: Option<VoidHandler>,
    player_fullscreen: Option<BoolHandler>,
    player_exit: Option<SeekHandler>,
    player_subtitle_delay: Option<SecondsHandler>,
    player_subtitle_margin: Option<MarginHandler>,
    connect_server: Option<VoidHandler>,
    // 添加服务器：参数是（地址，用户名，密码明文）——密码只在这一次函数调用里存在。
    add_server_submit: Option<SubmitHandler>,
    add_server_close: Option<VoidHandler>,
}

#[derive(Default)]
struct FocusState {
    home: usize,
    library: usize,
    detail: usize,
    search: usize,
}

impl FocusState {
    fn get(&self, page: Page) -> usize {
        match page {
            Page::Home => self.home,
            Page::Library => self.library,
            Page::Detail => self.detail,
            Page::Search => self.search,
            Page::Player => 0,
        }
    }

    fn set(&mut self, page: Page, index: usize) {
        match page {
            Page::Home => self.home = index,
            Page::Library => self.library = index,
            Page::Detail => self.detail = index,
            Page::Search => self.search = index,
            Page::Player => {}
        }
    }
}

struct State {
    nav: Navigator,
    focus: FocusState,
    home: HomeView,
    library: LibraryView,
    detail: DetailView,
    player: PlayerView,
    search: SearchView,
    // 当前顶栏标签下标：首页恒为 0，媒体库取 `LibraryView::current_tab`。
    // 搜索页没有自己的标签数据，顶栏高亮沿用离开前的那一项。
    nav_current_tab: usize,
    player_fullscreen: bool,
    // 当前顶部横幅；`None` 表示隐藏。
    banner: Option<BannerData>,
    // 上一次推给引擎的字幕抬升量（`sub-margin-y`）：不变就不重复发。
    subtitle_margin: Option<i64>,
    // 与 `App::images` 同一个 `Rc`：播放页刷新时要从这里取背景图（首帧前的铺底画面）。
    images: Rc<RefCell<ImageStore>>,
    add_server: AddServerView,
    callbacks: Callbacks,
}

impl Default for State {
    fn default() -> Self {
        Self {
            nav: Navigator::new(Route::Home),
            focus: FocusState::default(),
            home: HomeView::new(),
            library: LibraryView::default(),
            detail: DetailView::default(),
            player: PlayerView::default(),
            search: SearchView::default(),
            nav_current_tab: 0,
            player_fullscreen: false,
            banner: None,
            subtitle_margin: None,
            images: Rc::new(RefCell::new(ImageStore::new())),
            add_server: AddServerView::new(),
            callbacks: Callbacks::default(),
        }
    }
}

/// 页面命名空间：可交互目标的 id 前缀（`"library.sort"` 的 `"library."`）。
///
/// 目标清单在 Rust 侧按当前页前缀过滤，隐藏页面的登记因此不会混进来。
pub(crate) fn target_prefix(page: Page) -> &'static str {
    match page {
        Page::Home => "home.",
        Page::Library => "library.",
        Page::Detail => "detail.",
        Page::Search => "search.",
        Page::Player => "player.",
    }
}

impl From<Target> for TargetInfo {
    fn from(target: Target) -> Self {
        Self {
            id: target.id.to_string(),
            kind: target.kind.to_string(),
            label: target.label.to_string(),
            x: target.x,
            y: target.y,
            width: target.width,
            height: target.height,
        }
    }
}

/// 一次交互扫描的单项结果。
#[derive(Clone, Debug, PartialEq)]
pub struct SweepEntry {
    /// 目标 id。
    pub id: String,
    /// 控件类型。
    pub kind: String,
    /// 界面上的文字。
    pub label: String,
    /// 点击前后状态摘要是否不同。
    pub changed: bool,
    /// 点击后是否成功把应用恢复原状（页面 / 路由 / 对话框）。
    pub restored: bool,
    /// 点击前的状态摘要。
    pub before: String,
    /// 点击后的状态摘要。
    pub after: String,
}

/// 一次交互扫描的结果。
#[derive(Clone, Debug, PartialEq)]
pub struct SweepReport {
    /// 被扫描的页面名。
    pub page: String,
    /// 每一项的结果。
    pub entries: Vec<SweepEntry>,
    /// 是否因为"页面回不去"而提前收尾。
    pub aborted: bool,
}

impl SweepReport {
    /// 点完状态完全没变的目标（"死按钮"候选）。
    pub fn dead(&self) -> Vec<&SweepEntry> {
        self.entries.iter().filter(|entry| !entry.changed).collect()
    }
}

/// 扫描开始时的对话框快照：`sweep` 每轮拿它把对话框恢复成同一个起点。
struct DialogSnapshot {
    address: String,
    user: String,
    password: String,
}

/// 密码在状态摘要里的表示：只有长度。
///
/// 状态摘要会通过 `--debug-socket` 的 `sweep` 输出，所以这里**只放长度**。
///
/// 曾经放过 FNV-1a 32 位指纹：它不是加密散列，短密码可以在几分钟内暴力还原，
/// 等于把密码写进调试输出。`sweep` 只需要判断"点了之后有没有变化"，
/// 而点密码框只会移动焦点、不会改内容，长度足以区分。
fn password_fingerprint(password: &str) -> String {
    format!("len={}", password.chars().count())
}

/// 界面应用：拥有一个 `AppWindow`、导航栈、焦点与图片表。
pub struct App {
    ui: AppWindow,
    state: Rc<RefCell<State>>,
    images: Rc<RefCell<ImageStore>>,
    /// 重复元素 / 条件子树登记上来的可交互目标（按 id 去重）。
    ///
    /// Slint 1.19 的数组属性没有推导与拼接，这类目标没法写进父组件的
    /// `targets` 数组，只能经 `TargetBus` 报到 Rust（见 `docs/ui-architecture.md`）。
    targets: Rc<RefCell<BTreeMap<String, TargetInfo>>>,
    /// 上一代的登记结果（双缓冲的另一半）。
    ///
    /// 登记发生在**下一个事件循环回合**，而 `refresh_targets` 会推进代次让所有
    /// 元素重新上报。如果推进时把注册表直接清空，中间就有一段"清单是空的"的
    /// 空窗期——这时候按 id 点任何东西都会得到 `unknown target`（调试通道的
    /// 定时刷新每 50ms 就制造一次这种空窗，表现为"点一会儿行一会儿不行"）。
    ///
    /// 所以改成双缓冲：推进代次时把上一代结果留在 `targets`，新一代写进 `staging`，
    /// 读取时两者合并（新一代优先）。这样任何时刻清单都是"至少有上一帧的完整内容"。
    staging: Rc<RefCell<BTreeMap<String, TargetInfo>>>,
}

impl App {
    /// 创建窗口并安装回调。
    pub fn new() -> Result<Self, slint::PlatformError> {
        let ui = AppWindow::new()?;
        Ok(Self::new_with(ui))
    }

    /// 用已有窗口创建（测试用）。
    pub fn new_with(ui: AppWindow) -> Self {
        let images = Rc::new(RefCell::new(ImageStore::new()));
        let state = Rc::new(RefCell::new(State::default()));
        state.borrow_mut().images = images.clone();
        wire(&ui, &state);
        let targets: Rc<RefCell<BTreeMap<String, TargetInfo>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let staging: Rc<RefCell<BTreeMap<String, TargetInfo>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        {
            // 登记回调：只存下来，不做任何界面操作（调试通道取清单时才读）。
            let sink = Rc::clone(&staging);
            ui.global::<TargetBus>().on_register(move |target| {
                let info = TargetInfo::from(target);
                sink.borrow_mut().insert(info.id.clone(), info);
            });
        }
        Self {
            ui,
            state,
            images,
            targets,
            staging,
        }
    }

    /// 让所有登记型目标（卡片 / 标签 / 菜单行 / 条件子树）重新上报一次。
    ///
    /// 顺序不能颠倒：
    ///
    /// 1. **先实例化重复元素**（见 [`App::instantiate_items`]）。`for` 项是惰性创建的，
    ///    推进 `gen` 只让**已存在**的元素重新求值，不会把实例创建出来；
    /// 2. 再把 `staging` 推进 `targets`、推进 `TargetBus.gen`：Slint 会重新求值所有
    ///    依赖它的绑定并触发登记回调——调用方必须等一拍（真实窗口是调试定时器的
    ///    下一拍，测试是 `mock_elapsed_time`）再读 [`App::targets`]。
    ///
    /// 双缓冲：把上一代的结果推进 `targets`，清空 `staging` 收新一代，
    /// 再推进 `TargetBus.gen` 触发表达式重新求值。**不清空 `targets`**——
    /// 空窗期会让按 id 的点击随机失败（原因见 `staging` 字段的注释）。
    pub fn refresh_targets(&self) {
        self.instantiate_items();
        {
            let mut staging = self.staging.borrow_mut();
            if !staging.is_empty() {
                *self.targets.borrow_mut() = std::mem::take(&mut *staging);
            }
        }
        let bus = self.ui.global::<TargetBus>();
        bus.set_gen(bus.get_gen().wrapping_add(1));
    }

    /// 把 item tree 里的重复元素实例化出来（惰性 `for` 项不会自己出现）。
    ///
    /// 为什么要这一步：Slint 1.19 的 `for`（`RepeatedItemTree`）是**惰性实例化**的，
    /// 只有 item tree 被遍历——渲染、命中测试、或后端在出帧/处理输入前调用的
    /// `ensure_instantiated`——才会 new 出实例。而 `TargetBus.gen` 只是让**已存在**的
    /// 元素重新求值，它不会触发遍历。于是「刚切过去、还没渲染过一帧的页面」里
    /// 一张卡都不存在，推进多少次代次也登记不上，调试通道按 id 点击就报
    /// `unknown target`（实测：同一份代码，窗口出帧时正常；窗口被遮挡 / 最小化 /
    /// 尚未出首帧时整页卡片在清单里“不存在”，而任意一次指针事件（命中测试）
    /// 之后又立刻恢复）。
    ///
    /// 为什么直接调 `i-slint-core`：Slint 没有公开的「请布局 / 请实例化」API，
    /// `WindowInner::ensure_tree_instantiated` 正是后端自己在渲染前用的那个入口
    /// （它会创建 `for` 的实例，并把因此触发的 `changed` 回调跑掉）。
    fn instantiate_items(&self) {
        WindowInner::from_pub(self.ui.window()).ensure_tree_instantiated();
    }

    /// 当前页面的可交互目标（声明式清单 + 登记总线，按 id 去重并按 id 排序）。
    ///
    /// 过滤规则：
    /// - 只保留**当前页命名空间**（`home.` / `library.` / …）与浮层（`banner.`）的目标
    ///   ——隐藏页面里已登记的元素不会混进来；
    /// - 宽高必须为正，中心点必须落在窗口内（横向溢出视口一半的卡点不到中心）；
    /// - 「添加服务器」对话框打开时只保留 `dialog.` 目标（下层被遮罩挡住，
    ///   点它们只会得到"没反应"的假结论）；关闭后 `dialog.` 目标一律不列。
    ///   登记总线是只增不减的：关框后旧的 `dialog.*` 还在 `staging`/`targets` 里，
    ///   不看 `add-server-open` 就会在页面上凭空多出几个“对话框目标”（实测：
    ///   点「取消」后 `targets` 仍返回 5 个 dialog.*）。
    pub fn targets(&self) -> Vec<TargetInfo> {
        // 窗口尺寸是物理像素，目标坐标是逻辑像素：用缩放因子换算。
        let scale = self.ui.window().scale_factor();
        let size = self.ui.window().size();
        let (window_w, window_h) = (size.width as f32 / scale, size.height as f32 / scale);
        let mut merged: BTreeMap<String, TargetInfo> = BTreeMap::new();
        // 先放登记总线（重复元素 / 条件子树），再用声明式清单覆盖同 id：
        // 声明式清单是活绑定，标签与坐标永远是最新的；登记上来的是一次快照。
        for (id, info) in self.targets.borrow().iter() {
            merged.insert(id.clone(), info.clone());
        }
        // 新一代（上一帧推进代次后回填的）覆盖旧值——它反映最新的可见性与坐标。
        for (id, info) in self.staging.borrow().iter() {
            merged.insert(id.clone(), info.clone());
        }
        let declared = self.ui.get_targets();
        for index in 0..declared.row_count() {
            if let Some(target) = declared.row_data(index) {
                let info = TargetInfo::from(target);
                merged.insert(info.id.clone(), info);
            }
        }
        let dialog_open = self.ui.get_add_server_open();
        let prefix = target_prefix(self.page());
        // 条件子树里的目标只在条件为真时才存在于界面上，但登记总线只增不减
        // （双缓冲为了避开"清单是空的"空窗期，会把上一代的结果继续合并进来）。
        // 所以这里按视图模型的实际状态过滤一遍：否则"播放失败"过去的
        // `player.retry` 会在错误卡消失后继续被列出来，变成"看得见、点了没反应"
        // 的死按钮（体检 R2-7）。
        let retry_available = self.ui.get_player().has_error;
        let mut out: Vec<TargetInfo> = merged
            .into_values()
            .filter(|target| target.is_clickable())
            .filter(|target| target.id != "player.retry" || retry_available)
            .filter(|target| target.center_in_window(window_w, window_h))
            .filter(|target| {
                if dialog_open {
                    target.id.starts_with("dialog.")
                } else {
                    target.id.starts_with(prefix) || target.id.starts_with("banner.")
                }
            })
            .collect();
        out.sort_by(|left, right| left.id.cmp(&right.id));
        out
    }

    /// 按 id 找一目标（调试通道）。
    pub fn target(&self, id: &str) -> Option<TargetInfo> {
        self.targets().into_iter().find(|target| target.id == id)
    }

    /// 在窗口坐标上点一下（移动 → 按下 → 松开）。
    ///
    /// 走 Slint 自己的 `dispatch_event`：不需要操作系统级注入，也不打扰用户桌面。
    pub fn click_at(&self, x: f32, y: f32) -> Result<(), String> {
        self.pointer_at(x, y, true)
    }

    /// 只把指针移到窗口坐标（悬停）。
    pub fn hover_at(&self, x: f32, y: f32) -> Result<(), String> {
        self.pointer_at(x, y, false)
    }

    /// 点一个目标（按 id）；不存在时返回 `None`。
    pub fn click_target(&self, id: &str) -> Option<TargetInfo> {
        let target = self.target(id)?;
        let (x, y) = target.center();
        self.click_at(x, y).ok()?;
        Some(target)
    }

    /// 发一次按键（按下 + 松开）。
    pub fn press_key(&self, text: slint::SharedString) -> Result<(), String> {
        let window = self.ui.window();
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::KeyPressed {
                text: text.clone(),
            })
            .map_err(|error| error.to_string())?;
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::KeyReleased { text })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// 状态摘要：`sweep` 用它判断"点了有没有反应"。
    ///
    /// 只放**可观测且稳定**的字段（页面 / 路由 / 焦点 / 搜索词 / 排序 / 菜单 /
    /// 对话框 / 播放器状态 / 滚动偏移）。悬停态、动画中间帧这类噪声字段不进来，
    /// 否则每个按钮都会"有变化"，死按钮就淹没了。
    pub fn state_digest(&self) -> String {
        let ui = &self.ui;
        let state = self.state.borrow();
        // 视图模型总是存在（没有数据时是默认值），所以这里不区分 Option：
        // 摘要里出现空库/空搜索页是正常的，只要"变没变"。
        let library = {
            let view = &state.library;
            format!(
                "id={};sort={};count={};first={}",
                view.id,
                view.sort_label,
                view.cards.len(),
                view.cards
                    .first()
                    .map(|card| card.title.as_str())
                    .unwrap_or("")
            )
        };
        let detail = format!(
            "item={};title={};drop_open={};in_my_list={}",
            state.detail.item_id,
            state.detail.title,
            ui.get_detail_drop_open(),
            ui.get_detail_in_my_list()
        );
        let search = format!(
            "term={};state={:?};count={}",
            state.search.term,
            state.search.state,
            state.search.cards.len()
        );
        let player = &state.player;
        let player_line = format!(
            "playing={};volume={:.3};muted={};rate={:.2};audio={};subs={};controls={};error={}",
            player.playing,
            player.volume,
            player.muted,
            player.rate,
            player.audio_menu_open,
            player.subtitle_menu_open,
            player.controls_visible,
            player.error.as_deref().unwrap_or("")
        );
        // 对话框状态：地址 / 用户名 / 密码（只放指纹，不放明文）/ 当前聚焦的输入框。
        // 没有这些字段时，`sweep` 在“添加服务器”对话框上整页都是 dead：点输入框
        // 只改 `TextInput` 内部的焦点与光标，`add-server-open` 等属性保持不变。
        let dialog = format!(
            "open={};state={};error={};address={};user={};password={};focus={}",
            ui.get_add_server_open(),
            ui.get_add_server_state(),
            ui.get_add_server_error(),
            ui.get_add_server_address(),
            ui.get_add_server_user(),
            password_fingerprint(ui.get_add_server_password().as_str()),
            ui.get_add_server_focus(),
        );
        format!(
            "page={};route={:?};focus={},{},{},{};nav_active={};search_focused={};             search_text={};banner={}/{};dialog=[{}];library=[{}];detail=[{}];             search=[{}];player=[{}]",
            self.page().index(),
            self.current_route(),
            ui.get_home_focus(),
            ui.get_library_focus(),
            ui.get_detail_focus(),
            ui.get_search_focus(),
            ui.get_nav_active(),
            ui.get_search_focused(),
            self.search_text(),
            ui.get_banner_visible(),
            ui.get_banner_text(),
            dialog,
            library,
            detail,
            search,
            player_line
        )
    }

    /// 交互扫描：对当前页每个目标"悬停 → 点击"，记录状态摘要是否变化。
    ///
    /// 每点一项就把应用恢复原状（对话框回到扫描前的打开态与字段 / 从详情或
    /// 播放页退回原路由），否则后续目标的坐标会落在已经换掉的页面上，得到一堆
    /// 假的"没反应"。典型例子：点「取消」真的关掉了对话框，后面的
    /// `dialog.connect` / `dialog.user` 坐标就落到被遮罩的页面上，看上去全 dead。
    pub fn sweep(&self) -> SweepReport {
        let page = self.page();
        let route = self.current_route();
        // 对话框是浮层：扫描前开着就得每轮重新打开，而不是像以前那样在
        // `restore_after_click` 里无条件关掉（那会让第一个目标之后的全部失效）。
        let dialog = self.ui.get_add_server_open().then(|| DialogSnapshot {
            address: self.ui.get_add_server_address().to_string(),
            user: self.ui.get_add_server_user().to_string(),
            password: self.add_server_password(),
        });
        let targets = self.targets();
        let mut entries = Vec::new();
        let mut aborted = false;
        for target in &targets {
            let before = self.state_digest();
            let (x, y) = target.center();
            let outcome = self.hover_at(x, y).and_then(|()| self.click_at(x, y));
            if let Err(error) = outcome {
                entries.push(SweepEntry {
                    id: target.id.clone(),
                    kind: target.kind.clone(),
                    label: target.label.clone(),
                    changed: false,
                    restored: true,
                    before,
                    after: format!("error: {error}"),
                });
                continue;
            }
            let after = self.state_digest();
            let changed = before != after;
            let restored = self.restore_after_click(page, &route, dialog.as_ref());
            entries.push(SweepEntry {
                id: target.id.clone(),
                kind: target.kind.clone(),
                label: target.label.clone(),
                changed,
                restored,
                before,
                after,
            });
            if !restored {
                // 页面回不去，后面每个目标的坐标都不可信：提前收尾并说明原因。
                aborted = true;
                break;
            }
        }
        SweepReport {
            page: target_prefix(page).trim_end_matches('.').to_owned(),
            entries,
            aborted,
        }
    }

    /// 把应用恢复到扫描开始时的页面 / 路由 / 对话框状态；返回是否恢复成功。
    ///
    /// 恢复顺序：
    /// 1. 对话框：扫描前开着就关闭后按快照重新打开（地址 / 用户名 / 密码回填，
    ///    状态机复位到空闲、焦点回到地址框）；扫描前没开就关掉被点开的；
    /// 2. 先尝试 `go_back`（点卡片进详情、点标签切库、点播放进播放页都是压栈）；
    /// 3. 栈已经回不去（例如点「返回」直接把详情页弹掉了）就按原路由重建栈顶
    ///    （详情 / 媒体库 / 搜索 / 首页都能从当前视图模型重建）；
    /// 4. 最后强制切回原页面。
    ///
    /// 播放页是例外：播放会话（mpv）不是视图模型能凭空重建的，所以只要页面回到
    /// 播放页就算恢复成功（路由可能已经变成上一条）；调用方（`nova-app` 的
    /// `sweep`）会在扫描结束后收掉误开的播放器。
    fn restore_after_click(
        &self,
        page: Page,
        route: &Option<Route>,
        dialog: Option<&DialogSnapshot>,
    ) -> bool {
        match dialog {
            // 扫描前开着：先关（复位状态机 / 清密码）再按快照重开，保证每个目标
            // 的起点完全一致（焦点 = 地址框、状态 = 空闲、字段不变）。
            Some(snapshot) => {
                self.close_add_server();
                self.open_add_server(&snapshot.address, &snapshot.user);
                if !snapshot.password.is_empty() {
                    self.ui
                        .set_add_server_password(snapshot.password.as_str().into());
                }
            }
            None => {
                if self.ui.get_add_server_open() {
                    self.close_add_server();
                }
            }
        }
        for _ in 0..4 {
            if &self.current_route() == route {
                break;
            }
            if !self.go_back() {
                break;
            }
        }
        if &self.current_route() != route {
            match route {
                Some(Route::Detail { item_id }) => {
                    let _ = self.open_detail(item_id);
                }
                Some(Route::Library { .. }) => {
                    let view = self.state.borrow().library.clone();
                    let _ = self.open_library(&view);
                }
                Some(Route::Search { .. }) => {
                    let view = self.state.borrow().search.clone();
                    let _ = self.open_search(&view);
                }
                Some(Route::Home) => self.show_home(),
                // 播放会话无法重建，交给下面的页面检查。
                Some(Route::Player { .. }) | None => {}
            }
        }
        if self.page() != page {
            self.show_page(page);
        }
        if self.page() != page {
            return false;
        }
        &self.current_route() == route || page == Page::Player
    }

    fn pointer_at(&self, x: f32, y: f32, click: bool) -> Result<(), String> {
        let window = self.ui.window();
        let position = slint::LogicalPosition::new(x, y);
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::PointerMoved { position })
            .map_err(|error| error.to_string())?;
        if click {
            for event in [
                slint::platform::WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
                slint::platform::WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
            ] {
                window
                    .dispatch_event_with_result(event)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    /// 底层 Slint 窗口。
    pub fn ui(&self) -> &AppWindow {
        &self.ui
    }

    /// 当前页面。
    pub fn page(&self) -> Page {
        Page::from_index(self.ui.get_page())
    }

    /// 当前页面的焦点索引。
    pub fn focus_index(&self) -> usize {
        self.state.borrow().focus.get(self.page())
    }

    /// 导航栈快照。
    pub fn navigator(&self) -> Navigator {
        self.state.borrow().nav.clone()
    }

    /// 当前路由。
    pub fn current_route(&self) -> Option<Route> {
        self.state.borrow().nav.current().cloned()
    }

    /// 推入一张图片；调用方随后再 `set_*` 刷新页面。
    pub fn set_image(&self, key: &ImageKey, image: slint::Image) {
        self.images.borrow_mut().set(key.clone(), image);
    }

    /// 推入一张海报。
    pub fn set_poster(&self, item_id: impl Into<String>, image: slint::Image) {
        self.images.borrow_mut().set_poster(item_id, image);
    }

    /// 推入一张背景图。
    pub fn set_backdrop(&self, item_id: impl Into<String>, image: slint::Image) {
        self.images.borrow_mut().set_backdrop(item_id, image);
    }

    /// 设置首页视图并刷新界面。
    pub fn set_home(&self, view: &HomeView) {
        {
            let images = self.images.borrow();
            home::apply(&self.ui, view, &images);
        }
        let mut state = self.state.borrow_mut();
        state.home = view.clone();
        // 首页顶栏常驻高亮第一个标签（「首页」）。
        state.nav_current_tab = 0;
        if state.focus.home >= view.card_count() {
            state.focus.home = 0;
        }
        let focus = state.focus.home;
        drop(state);
        self.ui.set_home_focus(focus as i32);
    }

    /// 设置媒体库视图并刷新界面。
    ///
    /// 替换视图时把焦点**按条目 id** 重新定位（[`rebase_focus`]）：后台刷新会补齐
    /// 条目、重排顺序，沿用旧下标会让焦点落到另一条内容上。
    pub fn set_library(&self, view: &LibraryView) {
        {
            let images = self.images.borrow();
            library::apply(&self.ui, view, &images);
        }
        let mut state = self.state.borrow_mut();
        let focus = rebase_focus(&state.library, state.focus.library, view);
        state.library = view.clone();
        // 顶栏高亮跟随媒体库当前选中的标签，供后续搜索页沿用。
        state.nav_current_tab = view.current_tab;
        // `rebase_focus` 已经保证下标落在新卡片范围内（新列表为空时是 0）。
        state.focus.library = focus;
        drop(state);
        self.ui.set_library_focus(focus as i32);
    }

    /// 设置搜索页视图并刷新界面。
    ///
    /// 查询词与搜索框文本保持一致（回填）：只有值真的不同才写，
    /// 否则每次输入都会重置输入框光标。
    pub fn set_search(&self, view: &SearchView) {
        // 先只读地取出顶栏数据：`state` 的 borrow 在进入 `apply` 之前结束，
        // 保证这里不会同时持有 `state` 与 `images` 两个 borrow。
        let (tabs, current_tab) = {
            let state = self.state.borrow();
            (state.home.library_tabs.clone(), state.nav_current_tab)
        };
        {
            let images = self.images.borrow();
            search::apply(&self.ui, view, &tabs, current_tab, &images);
        }
        let mut state = self.state.borrow_mut();
        state.search = view.clone();
        if state.focus.search >= view.cards.len() {
            state.focus.search = 0;
        }
        let focus = state.focus.search;
        drop(state);
        self.ui.set_search_focus(focus as i32);
        // 用户正在输入框里打字时不回填：视图可能晚到一拍，回填会把
        // 光标位置与已输入文本重置成旧查询词。
        if !self.ui.get_search_focused() && self.ui.get_search_text() != view.term {
            self.ui.set_search_text(view.term.as_str().into());
        }
    }

    /// 当前搜索页视图模型的快照（测试与上层读取用）。
    pub fn search_view(&self) -> SearchView {
        self.state.borrow().search.clone()
    }

    /// 设置详情页视图并刷新界面。换条目时把焦点收回播放按钮。
    pub fn set_detail(&self, view: &DetailView) {
        {
            let images = self.images.borrow();
            detail::apply(&self.ui, view, &images);
        }
        let reset_focus = {
            let mut state = self.state.borrow_mut();
            let changed = state.detail.item_id != view.item_id;
            if changed {
                state.focus.detail = 0;
            }
            state.detail = view.clone();
            changed
        };
        if reset_focus {
            self.ui.set_detail_focus(0);
        }
    }

    /// 设置播放页视图并刷新界面。
    pub fn set_player(&self, view: &PlayerView) {
        self.state.borrow_mut().player = view.clone();
        player_refresh(&self.ui, &self.state);
    }

    /// 当前播放页视图模型的快照。
    pub fn player_view(&self) -> PlayerView {
        self.state.borrow().player.clone()
    }

    /// 播放页当前是否全屏（`F` 切换，实际窗口动作交给上层回调）。
    pub fn player_fullscreen(&self) -> bool {
        self.state.borrow().player_fullscreen
    }

    /// 推进播放页的虚拟时间（控制层自动隐藏）。
    ///
    /// 真实窗口由 `player_overlay.slint` 的 `Timer` 每 250ms 调一次；
    /// testing backend 不跑 Timer，所以测试直接调这个方法灌虚拟时间。
    /// 返回控制层可见性是否变化（没变化时不重刷界面，避免无谓的属性写入）。
    pub fn tick_player(&self, delta: Duration) -> bool {
        player_tick(&self.ui, &self.state, delta)
    }

    /// 标记一次用户输入（任何键 / 鼠标移动）：立刻显示控制层并重置 3 秒计时。
    pub fn player_note_input(&self) {
        player_note_input(&self.ui, &self.state);
    }

    /// 拖动进度条开始：只显示控制层，不发 seek。
    pub fn player_scrub_start(&self) {
        player_scrub_start(&self.ui, &self.state);
    }

    /// 拖动中：只更新时间气泡文本，不动播放位置。
    pub fn player_scrub_move(&self, ratio: f32) {
        player_scrub_move(&self.ui, &self.state, ratio);
    }

    /// 松手：这里才真正提交 seek。
    pub fn player_scrub_commit(&self, ratio: f32) {
        player_scrub_commit(&self.ui, &self.state, ratio);
    }

    /// 处理一个播放页按键；返回是否消费了该键。
    pub fn handle_player_key(&self, key: PlayerKey) -> bool {
        handle_player_key(&self.ui, &self.state, key)
    }

    /// 打开详情：压栈、切页、通知上层填数据。
    pub fn open_detail(&self, item_id: &str) -> bool {
        open_detail(&self.ui, &self.state, item_id)
    }

    /// 开始播放：压栈、切到播放外壳、通知上层。
    pub fn play(&self, item_id: &str) -> bool {
        play(&self.ui, &self.state, item_id)
    }

    /// 直接切到某个页面（不改变导航栈；演示与测试用）。
    pub fn show_page(&self, page: Page) {
        set_page(&self.ui, page);
    }

    /// 回到首页并把导航栈重置为根。
    pub fn show_home(&self) {
        self.state.borrow_mut().nav.reset(Route::Home);
        set_page(&self.ui, Page::Home);
    }

    /// 打开媒体库：刷新数据、压栈、切页。已经停在媒体库时不重复压栈。
    pub fn open_library(&self, view: &LibraryView) -> bool {
        self.set_library(view);
        let mut state = self.state.borrow_mut();
        if !matches!(state.nav.current(), Some(Route::Library { .. })) {
            state.nav.push(Route::Library {
                id: view.id.clone(),
                title: view.title.clone(),
            });
        }
        drop(state);
        set_page(&self.ui, Page::Library);
        true
    }

    /// 打开搜索页：刷新数据、压栈（已在搜索页时只更新栈顶查询词）、切页。
    pub fn open_search(&self, view: &SearchView) -> bool {
        self.set_search(view);
        let mut state = self.state.borrow_mut();
        match state.nav.current() {
            Some(Route::Search { term }) if *term == view.term => {}
            Some(Route::Search { .. }) => {
                // 查询词变了：替换栈顶，不重复压栈（返回仍然只回一层）。
                state.nav.pop();
                state.nav.push(Route::Search {
                    term: view.term.clone(),
                });
            }
            _ => state.nav.push(Route::Search {
                term: view.term.clone(),
            }),
        }
        drop(state);
        set_page(&self.ui, Page::Search);
        true
    }

    /// 返回上一级；栈空时返回 `false`。
    pub fn go_back(&self) -> bool {
        go_back(&self.ui, &self.state)
    }

    /// 处理一个按键命令；返回是否消费了该键。
    pub fn handle_key(&self, command: KeyCommand) -> bool {
        handle_key(&self.ui, &self.state, command)
    }

    /// 设置顶部横幅；`None` 隐藏。
    pub fn set_banner(&self, banner: Option<BannerData>) {
        self.state.borrow_mut().banner = banner.clone();
        banner::apply(&self.ui, banner.as_ref());
    }

    /// 当前横幅快照。
    pub fn banner(&self) -> Option<BannerData> {
        self.state.borrow().banner.clone()
    }

    /// 当前搜索框文本。
    pub fn search_text(&self) -> String {
        self.ui.get_search_text().to_string()
    }

    /// 设置搜索框文本（不触发 `search-changed`，测试与上层回填用）。
    pub fn set_search_text(&self, text: &str) {
        self.ui.set_search_text(text.into());
    }

    /// 打开详情回调。
    pub fn on_open_detail(&self, handler: impl Fn(&str) + 'static) {
        self.state.borrow_mut().callbacks.open_detail = Some(Rc::new(handler));
    }

    /// 播放回调。
    pub fn on_play(&self, handler: impl Fn(&str) + 'static) {
        self.state.borrow_mut().callbacks.play = Some(Rc::new(handler));
    }

    /// 返回回调。
    pub fn on_back(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.back = Some(Rc::new(handler));
    }

    /// 搜索回调（聚焦请求，如点击胶囊）。
    pub fn on_search(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.search = Some(Rc::new(handler));
    }

    /// 搜索文本变化回调（输入即时过滤；空串表示清空）。
    pub fn on_search_changed(&self, handler: impl Fn(&str) + 'static) {
        self.state.borrow_mut().callbacks.search_changed = Some(Rc::new(handler));
    }

    /// Enter 提交搜索回调。
    pub fn on_search_submit(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.search_submit = Some(Rc::new(handler));
    }

    /// 搜索页输入框的编辑回调（与顶栏的 `search-changed` 同一套行为）。
    pub fn on_search_input(&self, handler: impl Fn(&str) + 'static) {
        self.state.borrow_mut().callbacks.search_input = Some(Rc::new(handler));
    }

    /// 搜索页错误态的“重试”回调。
    pub fn on_search_retry(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.search_retry = Some(Rc::new(handler));
    }

    /// 清除搜索文本回调（导航栏的“×”按钮）。
    pub fn on_search_clear(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.search_clear = Some(Rc::new(handler));
    }

    /// 搜索页「×」：清空关键词、留在搜索页（Esc 的“清空 + 回首页”仍走
    /// [`App::on_search_clear`]）。
    pub fn on_search_clear_input(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.search_clear_input = Some(Rc::new(handler));
    }

    /// 横幅动作按钮回调。
    pub fn on_banner_action(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.banner_action = Some(Rc::new(handler));
    }

    /// 排序条回调。
    pub fn on_sort(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.sort = Some(Rc::new(handler));
    }

    /// 顶部媒体库标签回调。
    pub fn on_tab_activated(&self, handler: impl Fn(&str) + 'static) {
        self.state.borrow_mut().callbacks.tab = Some(Rc::new(handler));
    }

    /// 版本选择回调。
    /// 详情页「我的列表」切换。
    pub fn on_detail_my_list_toggle(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.detail_my_list_toggle = Some(Rc::new(handler));
    }

    pub fn on_source_selected(&self, handler: impl Fn(&str) + 'static) {
        self.state.borrow_mut().callbacks.source = Some(Rc::new(handler));
    }

    /// 播放 / 暂停回调（参数是切换后的播放状态）。
    pub fn on_player_play_pause(&self, handler: impl Fn(bool) + 'static) {
        self.state.borrow_mut().callbacks.player_play_pause = Some(Rc::new(handler));
    }

    /// seek 回调：参数是**目标位置**（已夹到 `0..=时长`）。
    pub fn on_player_seek(&self, handler: impl Fn(Duration) + 'static) {
        self.state.borrow_mut().callbacks.player_seek = Some(Rc::new(handler));
    }

    /// 音量变化回调（`0.0..=1.0`）。
    pub fn on_player_volume(&self, handler: impl Fn(f32) + 'static) {
        self.state.borrow_mut().callbacks.player_volume = Some(Rc::new(handler));
    }

    /// 静音状态变化回调。
    pub fn on_player_muted(&self, handler: impl Fn(bool) + 'static) {
        self.state.borrow_mut().callbacks.player_muted = Some(Rc::new(handler));
    }

    /// 倍速变化回调。
    pub fn on_player_rate(&self, handler: impl Fn(f32) + 'static) {
        self.state.borrow_mut().callbacks.player_rate = Some(Rc::new(handler));
    }

    /// 音轨选择回调（引擎侧切轨，不重启播放）。
    pub fn on_player_audio_track(&self, handler: impl Fn(u32) + 'static) {
        self.state.borrow_mut().callbacks.player_audio_track = Some(Rc::new(handler));
    }

    /// 字幕选择回调；`None` 表示关闭字幕。
    pub fn on_player_subtitle_track(&self, handler: impl Fn(Option<u32>) + 'static) {
        self.state.borrow_mut().callbacks.player_subtitle_track = Some(Rc::new(handler));
    }

    /// 错误卡"重试"回调。
    pub fn on_player_retry(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.player_retry = Some(Rc::new(handler));
    }

    /// 全屏切换回调（真正动窗口的是上层）。
    pub fn on_player_fullscreen(&self, handler: impl Fn(bool) + 'static) {
        self.state.borrow_mut().callbacks.player_fullscreen = Some(Rc::new(handler));
    }

    /// 退出播放回调：**先**提交进度（参数是最终位置），再由界面返回上一页。
    pub fn on_player_exit(&self, handler: impl Fn(Duration) + 'static) {
        self.state.borrow_mut().callbacks.player_exit = Some(Rc::new(handler));
    }

    /// 字幕延迟步进回调（秒，**可负**：`-0.5` / `+0.5`）。
    ///
    /// 累加是引擎侧的事（mpv 的 `sub-delay` 可负），界面只发这一步。
    pub fn on_player_subtitle_delay(&self, handler: impl Fn(f64) + 'static) {
        self.state.borrow_mut().callbacks.player_subtitle_delay = Some(Rc::new(handler));
    }

    /// "加载外挂字幕…"回调：文件选择在上层（界面不碰文件系统）。
    /// 空状态的“连接服务器”回调（真正的连接流程在上层）。
    pub fn on_connect_server(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.connect_server = Some(Rc::new(handler));
    }

    /// 提交“添加服务器”：参数是（地址，用户名，密码）。
    ///
    /// 密码只在这里作为字符串出现一次；上层应立刻包进 `SecretString`。
    pub fn on_add_server_submit(&self, handler: impl Fn(&str, &str, &str) + 'static) {
        self.state.borrow_mut().callbacks.add_server_submit = Some(Rc::new(handler));
    }

    /// 对话框被取消（取消按钮 / Esc / 遮罩点击）。
    pub fn on_add_server_close(&self, handler: impl Fn() + 'static) {
        self.state.borrow_mut().callbacks.add_server_close = Some(Rc::new(handler));
    }

    /// 对话框当前是否打开。
    pub fn add_server_open(&self) -> bool {
        self.ui.get_add_server_open()
    }

    /// 打开“添加服务器”对话框（可选预填最近使用的地址与用户名）。
    ///
    /// 纯界面操作：不阻塞、不发网络请求。
    pub fn open_add_server(&self, address: &str, user: &str) {
        let view = self
            .state
            .borrow()
            .add_server
            .clone()
            .open_with(address, user);
        add_server::apply(&self.ui, &view);
        self.state.borrow_mut().add_server = view;
        self.ui.invoke_focus_add_server_address();
    }

    /// 关闭对话框：清空密码（用完即弃），复位状态机与错误。
    pub fn close_add_server(&self) {
        let view = self.state.borrow().add_server.clone().close();
        add_server::apply(&self.ui, &view);
        self.state.borrow_mut().add_server = view;
        // 密码只存在于输入框属性里，关框即清。
        self.ui.set_add_server_password("".into());
    }

    /// 表单下方的常驻提示（如“密钥环不可用，本次会话有效”）。
    pub fn set_add_server_note(&self, note: &str) {
        let mut view = self.state.borrow().add_server.clone();
        view.note = (!note.is_empty()).then(|| note.to_owned());
        add_server::apply(&self.ui, &view);
        self.state.borrow_mut().add_server = view;
    }

    /// 进入“连接中”：禁用重复提交。
    pub fn begin_add_server_connecting(&self) {
        let view = self.state.borrow().add_server.clone().begin_connecting();
        add_server::apply(&self.ui, &view);
        self.state.borrow_mut().add_server = view;
    }

    /// 连接失败：回到空闲并把错误展示在错误区。
    pub fn set_add_server_error(&self, error: AddServerError) {
        let view = self.state.borrow().add_server.clone().fail(error);
        add_server::apply(&self.ui, &view);
        self.state.borrow_mut().add_server = view;
    }

    /// 当前输入的地址（测试与上层读取用）。
    pub fn add_server_address(&self) -> String {
        self.ui.get_add_server_address().to_string()
    }

    /// 当前输入的用户名。
    pub fn add_server_user(&self) -> String {
        self.ui.get_add_server_user().to_string()
    }

    /// 当前输入的密码（仅提交瞬间读取；测试里也只断言“清空后为空”）。
    pub fn add_server_password(&self) -> String {
        self.ui.get_add_server_password().to_string()
    }

    /// 字幕安全区回调：控制层可见时抬起 `y` px（mpv 的 `sub-margin-y`），隐藏时为 `0`。
    ///
    /// 只在数值真正变化时发（`blend-subtitles=video` 的字幕烧在视频层，控制层会盖住它）。
    pub fn on_player_subtitle_margin(&self, handler: impl Fn(i64) + 'static) {
        self.state.borrow_mut().callbacks.player_subtitle_margin = Some(Rc::new(handler));
    }
}

fn page_of_route(route: Option<&Route>) -> Page {
    match route {
        Some(Route::Home) | None => Page::Home,
        Some(Route::Library { .. }) => Page::Library,
        Some(Route::Detail { .. }) => Page::Detail,
        Some(Route::Player { .. }) => Page::Player,
        Some(Route::Search { .. }) => Page::Search,
    }
}

fn wire(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_key_intent(move |code| {
            if let Some(ui) = weak.upgrade() {
                if let Some(command) = KeyCommand::from_intent(code) {
                    handle_key(&ui, &state, command);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_open_item(move |id| {
            if let Some(ui) = weak.upgrade() {
                open_detail(&ui, &state, id.as_str());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_play_item(move |id| {
            if let Some(ui) = weak.upgrade() {
                play(&ui, &state, id.as_str());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_hero_play(move || {
            if let Some(ui) = weak.upgrade() {
                let item_id = state
                    .borrow()
                    .home
                    .hero
                    .as_ref()
                    .map(|hero| hero.item_id.clone());
                if let Some(item_id) = item_id {
                    play(&ui, &state, &item_id);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_back(move || {
            if let Some(ui) = weak.upgrade() {
                go_back(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_search(move || {
            if let Some(ui) = weak.upgrade() {
                focus_search(&ui, &state);
            }
        });
    }
    {
        // 输入即时过滤：文本变化原样转出，业务在上层。
        let state = state.clone();
        ui.on_search_changed(move |text| {
            let handler = state.borrow().callbacks.search_changed.clone();
            if let Some(handler) = handler {
                handler(text.as_str());
            }
        });
    }
    {
        let state = state.clone();
        ui.on_search_submit(move || {
            let handler = state.borrow().callbacks.search_submit.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    {
        // 搜索页的大输入框：默认复用「输入变化」那一条链路（行为完全一致）。
        let state = state.clone();
        ui.on_search_input(move |text| {
            let handler = {
                let state = state.borrow();
                state
                    .callbacks
                    .search_input
                    .clone()
                    .or_else(|| state.callbacks.search_changed.clone())
            };
            if let Some(handler) = handler {
                handler(text.as_str());
            }
        });
    }
    {
        // 搜索页错误态的“重试”。
        let state = state.clone();
        ui.on_search_retry(move || {
            let handler = state.borrow().callbacks.search_retry.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    {
        // “×”按钮：清空文本并把空串发出去（等价于一次清空后的过滤）。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_search_clear(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.set_search_text("".into());
            let clear = state.borrow().callbacks.search_clear.clone();
            if let Some(handler) = clear {
                handler();
            }
            let changed = state.borrow().callbacks.search_changed.clone();
            if let Some(handler) = changed {
                handler("");
            }
        });
    }
    {
        // 搜索页的“×”：清空关键词但留在搜索页（Esc 才回首页，走上面的 search_clear）。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_search_clear_input(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            clear_search_input(&ui, &state);
        });
    }
    {
        // 横幅动作按钮：动作在上层（重新登录 / 重试…）。
        let state = state.clone();
        ui.on_banner_action(move || {
            let handler = state.borrow().callbacks.banner_action.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    {
        // 空状态的“连接服务器”：对话框 / 网络在上层，这里只把意图转出去。
        let state = state.clone();
        ui.on_connect_server(move || {
            let handler = state.borrow().callbacks.connect_server.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    {
        // 取消（取消按钮 / Esc / 遮罩点击）：先复位界面状态，再通知上层。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_add_server_close(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let view = state.borrow().add_server.clone().close();
            add_server::apply(&ui, &view);
            state.borrow_mut().add_server = view;
            ui.set_add_server_password("".into());
            let handler = state.borrow().callbacks.add_server_close.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    {
        // 提交：把三个字段原样转给上层；门控（连接中 / 地址不合规）在这里拦住。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_add_server_submit(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let (open, can_submit) = {
                let view = state.borrow().add_server.clone();
                (view.open, view.can_submit())
            };
            if !open || !can_submit {
                return;
            }
            let handler = state.borrow().callbacks.add_server_submit.clone();
            if let Some(handler) = handler {
                let address = ui.get_add_server_address().to_string();
                let user = ui.get_add_server_user().to_string();
                let password = ui.get_add_server_password().to_string();
                handler(&address, &user, &password);
            }
        });
    }
    {
        // 地址实时校验：输入即更新内联提示与 http 警示条。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_add_server_address_edited(move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut view = state.borrow().add_server.clone();
            view.address = text.to_string();
            state.borrow_mut().add_server = view.clone();
            add_server::apply_derived(&ui, &view);
        });
    }
    {
        let state = state.clone();
        ui.on_sort(move || {
            let handler = state.borrow().callbacks.sort.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    {
        let state = state.clone();
        ui.on_tab_activated(move |id| {
            let handler = {
                let mut state = state.borrow_mut();
                state.focus.library = 0;
                state.callbacks.tab.clone()
            };
            if let Some(handler) = handler {
                handler(id.as_str());
            }
        });
    }
    {
        let state = state.clone();
        ui.on_source_selected(move |id| {
            let handler = state.borrow().callbacks.source.clone();
            if let Some(handler) = handler {
                handler(id.as_str());
            }
        });
    }
    {
        let state = state.clone();
        ui.on_detail_my_list_toggle(move || {
            let handler = state.borrow().callbacks.detail_my_list_toggle.clone();
            if let Some(handler) = handler {
                handler();
            }
        });
    }
    // ---- 播放页 ----
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_key(move |code| {
            if let Some(ui) = weak.upgrade() {
                if let Some(key) = PlayerKey::from_intent(code) {
                    handle_player_key(&ui, &state, key);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_back(move || {
            if let Some(ui) = weak.upgrade() {
                player_exit(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_play_pause(move || {
            if let Some(ui) = weak.upgrade() {
                player_play_pause(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_seek_relative(move |seconds| {
            if let Some(ui) = weak.upgrade() {
                player_seek_relative(&ui, &state, i64::from(seconds));
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_scrub_start(move || {
            if let Some(ui) = weak.upgrade() {
                player_scrub_start(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_scrub_move(move |ratio| {
            if let Some(ui) = weak.upgrade() {
                player_scrub_move(&ui, &state, ratio);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_scrub_commit(move |ratio| {
            if let Some(ui) = weak.upgrade() {
                player_scrub_commit(&ui, &state, ratio);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_scrub_cancel(move || {
            if let Some(ui) = weak.upgrade() {
                player_scrub_cancel(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_input(move || {
            if let Some(ui) = weak.upgrade() {
                player_note_input(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_tick(move || {
            if let Some(ui) = weak.upgrade() {
                player_tick(&ui, &state, player::TICK_INTERVAL);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_audio_toggle(move || {
            if let Some(ui) = weak.upgrade() {
                player_toggle_menu(&ui, &state, true);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_subtitle_toggle(move || {
            if let Some(ui) = weak.upgrade() {
                player_toggle_menu(&ui, &state, false);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_audio_selected(move |index| {
            if let Some(ui) = weak.upgrade() {
                player_audio_selected(&ui, &state, index);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_subtitle_selected(move |index| {
            if let Some(ui) = weak.upgrade() {
                player_subtitle_selected(&ui, &state, index);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_retry(move || {
            if let Some(ui) = weak.upgrade() {
                player_retry(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_fullscreen_toggle(move || {
            if let Some(ui) = weak.upgrade() {
                player_toggle_fullscreen(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_rate_cycle(move || {
            if let Some(ui) = weak.upgrade() {
                player_cycle_rate(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_time_toggle(move || {
            if let Some(ui) = weak.upgrade() {
                player_toggle_time_mode(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_mute_toggle(move || {
            if let Some(ui) = weak.upgrade() {
                player_toggle_mute(&ui, &state);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_volume_set(move |volume| {
            if let Some(ui) = weak.upgrade() {
                player_volume_set(&ui, &state, volume);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_subtitle_delay(move |seconds| {
            if let Some(ui) = weak.upgrade() {
                player_subtitle_delay(&ui, &state, f64::from(seconds));
            }
        });
    }
}

fn focus_rows(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) -> Vec<usize> {
    let columns = ui.get_grid_columns().max(1) as usize;
    let state = state.borrow();
    match page {
        Page::Home => home::focus_rows(&state.home, columns),
        Page::Library => library::focus_rows(&state.library, columns),
        Page::Detail => detail::focus_rows(&state.detail),
        Page::Search => search::focus_rows(&state.search, columns),
        Page::Player => Vec::new(),
    }
}

fn set_focus(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page, index: usize) {
    state.borrow_mut().focus.set(page, index);
    match page {
        Page::Home => ui.set_home_focus(index as i32),
        Page::Library => ui.set_library_focus(index as i32),
        Page::Detail => ui.set_detail_focus(index as i32),
        Page::Search => ui.set_search_focus(index as i32),
        Page::Player => {}
    }
}

fn handle_key(ui: &AppWindow, state: &Rc<RefCell<State>>, command: KeyCommand) -> bool {
    // 对话框打开时：键盘不透给被遮罩的首页（Esc 已由 .slint 直接关框）。
    if ui.get_add_server_open() {
        return true;
    }
    let page = Page::from_index(ui.get_page());
    match command {
        // 播放页没有焦点环：Enter / Space 交给"播放/暂停"，不重新起播。
        KeyCommand::Accept if page == Page::Player => {
            player_play_pause(ui, state);
            true
        }
        KeyCommand::Left => move_focus_by(ui, state, page, -1, 0),
        KeyCommand::Right => move_focus_by(ui, state, page, 1, 0),
        KeyCommand::Up => move_focus_by(ui, state, page, 0, -1),
        KeyCommand::Down => move_focus_by(ui, state, page, 0, 1),
        KeyCommand::Accept => activate(ui, state),
        // 搜索页的 Esc 与页内输入框里的 Esc 同义：清空查询并回首页。
        // 只回上一级会让查询词留在框里、首页也还是未过滤的，两种入口行为不一致。
        KeyCommand::Back if page == Page::Search => {
            let handler = state.borrow().callbacks.search_clear.clone();
            match handler {
                Some(handler) => {
                    ui.set_search_text("".into());
                    handler();
                    true
                }
                None => go_back(ui, state),
            }
        }
        KeyCommand::Back => go_back(ui, state),
        KeyCommand::Search => {
            focus_search(ui, state);
            true
        }
        // Ctrl+N：与空态按钮、横幅动作同一回调（打开对话框的决策在上层）。
        KeyCommand::ConnectServer => {
            let handler = state.borrow().callbacks.connect_server.clone();
            if let Some(handler) = handler {
                handler();
            }
            true
        }
    }
}

/// 当前页的焦点索引，**以 UI 属性为准**。
///
/// 悬停会直接改 `.slint` 里的 `focus-index`（网格 `PosterGrid`、首页行
/// `CardRow.focus-hover`），而 Rust 的 `state.focus` 只在键盘移动时写。如果方向键
/// 从 `state.focus` 继续，悬停第 5 张卡之后按 Right 会从"上一次键盘位置 +1"开始
/// （体检 R2-3）。读 UI 并顺手回写 `state.focus`，两边在下一次按键时重新对齐。
fn current_focus(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) -> usize {
    let index = match page {
        Page::Home => ui.get_home_focus(),
        Page::Library => ui.get_library_focus(),
        Page::Detail => ui.get_detail_focus(),
        Page::Search => ui.get_search_focus(),
        Page::Player => 0,
    }
    .max(0) as usize;
    state.borrow_mut().focus.set(page, index);
    index
}

fn move_focus_by(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page, dx: i32, dy: i32) -> bool {
    if page == Page::Player {
        return false;
    }
    let rows = focus_rows(ui, state, page);
    if rows.is_empty() {
        return false;
    }
    let current = current_focus(ui, state, page);
    let next = move_focus(&rows, current, dx, dy);
    if next != current {
        set_focus(ui, state, page, next);
    }
    // 焦点可能被移出可视区，滚动让焦点卡完全可见。
    // 焦点数学在 Rust，滚动几何在 `.slint`（`scroll-to-index`），两份不重算。
    // 首页有两条视觉行（继续观看 / 最近添加），纵向滚动 + 行内横向滚动都在
    // `HomePage.scroll-to-index` 里完成（R2-2：以前 Home 分支是空的）。
    match page {
        Page::Home => ui.invoke_scroll_home_to_index(next as i32),
        Page::Library => ui.invoke_scroll_library_to_index(next as i32),
        Page::Search => ui.invoke_scroll_search_to_index(next as i32),
        Page::Detail | Page::Player => {}
    }
    true
}

fn activate(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    let page = Page::from_index(ui.get_page());
    // 与方向键同一口径：悬停过的卡就是 Enter 要打开的那张（R2-3）。
    let index = current_focus(ui, state, page);
    let item_id = {
        let state = state.borrow();
        match page {
            Page::Home => state.home.card_at(index).map(|card| card.item_id.clone()),
            Page::Library => state
                .library
                .card_at(index)
                .map(|card| card.item_id.clone()),
            Page::Search => state.search.card_at(index).map(|card| card.item_id.clone()),
            Page::Detail | Page::Player => None,
        }
    };
    if let Some(item_id) = item_id {
        return open_detail(ui, state, &item_id);
    }
    if matches!(page, Page::Detail | Page::Player) {
        return play_current(ui, state);
    }
    false
}

fn open_detail(ui: &AppWindow, state: &Rc<RefCell<State>>, item_id: &str) -> bool {
    let handler = {
        let mut state = state.borrow_mut();
        state.nav.push(Route::Detail {
            item_id: item_id.to_string(),
        });
        state.focus.detail = 0;
        state.callbacks.open_detail.clone()
    };
    set_focus(ui, state, Page::Detail, 0);
    set_page(ui, Page::Detail);
    if let Some(handler) = handler {
        handler(item_id);
    }
    true
}

fn play_current(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    let item_id = {
        let state = state.borrow();
        match state.nav.current() {
            Some(Route::Detail { item_id }) | Some(Route::Player { item_id }) => {
                Some(item_id.clone())
            }
            _ => None,
        }
    };
    match item_id {
        Some(item_id) => play(ui, state, &item_id),
        None => false,
    }
}

fn play(ui: &AppWindow, state: &Rc<RefCell<State>>, item_id: &str) -> bool {
    let handler = {
        let mut state = state.borrow_mut();
        let already_playing = matches!(state.nav.current(), Some(Route::Player { .. }));
        if !already_playing {
            state.nav.push(Route::Player {
                item_id: item_id.to_string(),
            });
        }
        state.callbacks.play.clone()
    };
    set_page(ui, Page::Player);
    if let Some(handler) = handler {
        handler(item_id);
    }
    true
}

fn go_back(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    let (page, handler) = {
        let mut state = state.borrow_mut();
        let route = match state.nav.back() {
            Some(route) => route.clone(),
            None => return false,
        };
        (page_of_route(Some(&route)), state.callbacks.back.clone())
    };
    set_page(ui, page);
    if let Some(handler) = handler {
        handler();
    }
    true
}

/// 切页并把键盘焦点还给全局 `keyboard` FocusScope。
///
/// 为什么不放在 `.slint` 的 `changed page =>` 里：Slint 的 `changed` 处理器是
/// **延迟**执行的（属性变脏后，等当前事件 / 属性批处理跑完才调用）。`open_search`
/// 之后上层紧接着 `invoke_focus_search_input()` 把焦点交给搜索页大输入框，延迟的
/// `changed page` 会把焦点又抢回 `keyboard`——实测顶栏搜索交接后后续字符全部丢失
/// （`search_handoff.rs`）。所以在 Rust 的统一切页入口同步还焦：换页时先还给全局
/// 键盘，需要输入框的路径（搜索交接 / Ctrl+F）随后聚焦输入框，后者是最后一步。
///
/// **只有页面真的变了才还焦**：`open_search` / `open_library` 在每次输入、每次
/// 异步结果到达时都会被调用（页面没变）；无条件还焦会把正在打字的搜索框里的
/// 焦点抢走（实测：调试通道 `search page:true` 之后输入字符全部丢失）。
fn set_page(ui: &AppWindow, page: Page) {
    let changed = Page::from_index(ui.get_page()) != page;
    ui.set_page(page.index());
    if changed {
        ui.invoke_focus_global();
    }
}

fn focus_search(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    ui.set_search_focused(true);
    ui.invoke_focus_search_input();
    let handler = state.borrow().callbacks.search.clone();
    if let Some(handler) = handler {
        handler();
    }
}

/// 搜索页「×」：清空关键词但**留在搜索页**，焦点交回输入框。
///
/// 与 Esc 分开：Esc（`search-clear`）是“清空 + 回首页”，而点「×」只是
/// 撤掉这次查询——把用户从自己刚打开的结果页踢走不是那个按钮的意思。
/// 路由不变，只换一份空视图（Idle）：否则清空后结果网格还留着上一轮的卡片。
fn clear_search_input(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // 先只读地取出顶栏数据，`state` 的 borrow 在 `apply` 前结束（同 `set_search`）。
    let (tabs, current_tab, images) = {
        let state = state.borrow();
        (
            state.home.library_tabs.clone(),
            state.nav_current_tab,
            state.images.clone(),
        )
    };
    let view = SearchView::default();
    search::apply(ui, &view, &tabs, current_tab, &images.borrow());
    {
        let mut state = state.borrow_mut();
        state.search = view;
        state.focus.search = 0;
        // 路由顶端的查询词跟着清空：否则从结果点进详情再返回，栈顶还挂着旧词。
        if matches!(state.nav.current(), Some(Route::Search { .. })) {
            state.nav.pop();
            state.nav.push(Route::Search {
                term: String::new(),
            });
        }
    }
    ui.set_search_focus(0);
    ui.set_search_text("".into());
    // 真焦点被交回输入框，`search-focused` 必须跟着为真：否则晚到的视图
    // （`set_search`）会按“用户没在输入”把旧查询词回填进来。
    ui.set_search_focused(true);
    ui.invoke_focus_search_input();
    let handler = state.borrow().callbacks.search_clear_input.clone();
    if let Some(handler) = handler {
        handler();
    }
}

// ---- 播放页：状态推进与按键行为 ----

fn player_item_id(state: &State) -> String {
    match state.nav.current() {
        Some(Route::Player { item_id } | Route::Detail { item_id }) => item_id.clone(),
        _ => String::new(),
    }
}

fn player_refresh(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let (view, item_id, images) = {
        let state = state.borrow();
        (
            state.player.clone(),
            player_item_id(&state),
            state.images.clone(),
        )
    };
    player::apply_for(ui, &view, &images.borrow(), &item_id);
    player_push_subtitle_margin(ui, state, &view);
}

/// 把字幕抬升量推给引擎（`sub-margin-y`）：控制层可见时抬起，隐藏时归零。
///
/// 只在数值真正变化时发，所以 250ms 的 tick 不会刷屏。
fn player_push_subtitle_margin(ui: &AppWindow, state: &Rc<RefCell<State>>, view: &PlayerView) {
    let scale = ui.window().scale_factor();
    let physical_height = f64::from(ui.window().size().height);
    let height = if scale > 0.0 {
        (physical_height / f64::from(scale)) as f32
    } else {
        physical_height as f32
    };
    let margin = view.subtitle_margin(height);
    let handler = {
        let mut state = state.borrow_mut();
        if state.subtitle_margin == Some(margin) {
            return;
        }
        state.subtitle_margin = Some(margin);
        state.callbacks.player_subtitle_margin.clone()
    };
    if let Some(handler) = handler {
        handler(margin);
    }
}

fn player_tick(ui: &AppWindow, state: &Rc<RefCell<State>>, delta: Duration) -> bool {
    let changed = state.borrow_mut().player.tick(delta);
    if changed {
        player_refresh(ui, state);
    }
    changed
}

fn player_note_input(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let changed = {
        let mut state = state.borrow_mut();
        let before = state.player.controls_visible;
        state.player.note_input();
        before != state.player.controls_visible
    };
    if changed {
        player_refresh(ui, state);
    }
}

fn player_play_pause(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let (playing, handler) = {
        let mut state = state.borrow_mut();
        state.player.playing = !state.player.playing;
        state.player.note_input();
        (
            state.player.playing,
            state.callbacks.player_play_pause.clone(),
        )
    };
    player_refresh(ui, state);
    if let Some(handler) = handler {
        handler(playing);
    }
}

fn player_seek_to(ui: &AppWindow, state: &Rc<RefCell<State>>, target: Duration) {
    {
        let mut state = state.borrow_mut();
        state.player.position = target;
        state.player.note_input();
        state.player.close_menus();
    }
    player::clear_scrub(ui);
    player_refresh(ui, state);
    let handler = state.borrow().callbacks.player_seek.clone();
    if let Some(handler) = handler {
        handler(target);
    }
}

fn player_seek_relative(ui: &AppWindow, state: &Rc<RefCell<State>>, seconds: i64) {
    let target = state.borrow().player.shift_position(seconds);
    player_seek_to(ui, state, target);
}

fn player_scrub_start(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    player_note_input(ui, state);
}

fn player_scrub_move(ui: &AppWindow, state: &Rc<RefCell<State>>, ratio: f32) {
    player_note_input(ui, state);
    let view = state.borrow().player.clone();
    player::apply_scrub(ui, &view, ratio);
}

fn player_scrub_commit(ui: &AppWindow, state: &Rc<RefCell<State>>, ratio: f32) {
    let target = {
        let state = state.borrow();
        state
            .player
            .position_at_ratio(ratio)
            .unwrap_or(state.player.position)
    };
    player_seek_to(ui, state, target);
}

fn player_scrub_cancel(ui: &AppWindow, _state: &Rc<RefCell<State>>) {
    player::clear_scrub(ui);
}

/// 音量滑杆：把目标音量（0..1）直接写进视图模型。
///
/// 非零音量视为解除静音（与键盘加音量同一口径），其余收尾见 [`player_apply_volume`]。
fn player_volume_set(ui: &AppWindow, state: &Rc<RefCell<State>>, volume: f32) {
    let volume = volume.clamp(0.0, 1.0);
    player_apply_volume(ui, state, volume, volume > 0.0);
}

/// 键盘 `↑` / `↓`：在上一次的音量上加减。
///
/// 只有**上调**才解除静音（下调不该悄悄把静音取消掉）。
fn player_volume_by(ui: &AppWindow, state: &Rc<RefCell<State>>, delta: f32) {
    let volume = (state.borrow().player.volume + delta).clamp(0.0, 1.0);
    player_apply_volume(ui, state, volume, delta > 0.0);
}

/// 音量收尾（滑杆 / 键盘共用）：写视图模型、按需解除静音、短时数字、重绘、通知引擎。
///
/// 解除静音**必须同时通知引擎**（`on_player_muted(false)`）：宿主每帧把自己的
/// `muted` 推回界面（`player/mod.rs` 的 `view.muted = self.muted`），只改界面这一份
/// 的话下一次 tick 就会被刷回“静音”，而 mpv 的 `mute` 属性也还是 `yes`
/// ——界面显示已取消静音、实际却依然没声音。真机实测：先按 M 静音，再用滑杆把音量
/// 拖到 50%，`view` 里 `muted` 仍是 true、音量却变成了 0.5。
fn player_apply_volume(ui: &AppWindow, state: &Rc<RefCell<State>>, volume: f32, unmute: bool) {
    let (volume, unmuted, volume_handler, mute_handler) = {
        let mut state = state.borrow_mut();
        state.player.volume = volume;
        let unmuted = unmute && state.player.muted;
        if unmuted {
            state.player.muted = false;
        }
        // 数字文本只在调节时短时显示。
        state.player.note_volume_change();
        (
            volume,
            unmuted,
            state.callbacks.player_volume.clone(),
            state.callbacks.player_muted.clone(),
        )
    };
    player_refresh(ui, state);
    if let Some(handler) = volume_handler {
        handler(volume);
    }
    if unmuted {
        if let Some(handler) = mute_handler {
            handler(false);
        }
    }
}

fn player_toggle_time_mode(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    state.borrow_mut().player.toggle_time_mode();
    player_refresh(ui, state);
}

fn player_subtitle_delay(ui: &AppWindow, state: &Rc<RefCell<State>>, seconds: f64) {
    player_note_input(ui, state);
    let handler = state.borrow().callbacks.player_subtitle_delay.clone();
    if let Some(handler) = handler {
        handler(seconds);
    }
}

fn player_toggle_mute(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let (muted, handler) = {
        let mut state = state.borrow_mut();
        state.player.muted = !state.player.muted;
        // 静音 / 取消静音也算一次音量调节：数字文本短时显示。
        state.player.note_volume_change();
        (state.player.muted, state.callbacks.player_muted.clone())
    };
    player_refresh(ui, state);
    if let Some(handler) = handler {
        handler(muted);
    }
}

fn player_toggle_menu(ui: &AppWindow, state: &Rc<RefCell<State>>, audio: bool) {
    {
        let mut state = state.borrow_mut();
        if audio {
            state.player.toggle_audio_menu();
        } else {
            state.player.toggle_subtitle_menu();
        }
    }
    player_refresh(ui, state);
}

fn player_audio_selected(ui: &AppWindow, state: &Rc<RefCell<State>>, index: i32) {
    if index < 0 {
        return;
    }
    let Ok(index) = u32::try_from(index) else {
        return;
    };
    let changed = state.borrow_mut().player.select_audio_track(index);
    if !changed {
        return;
    }
    player_refresh(ui, state);
    let handler = state.borrow().callbacks.player_audio_track.clone();
    if let Some(handler) = handler {
        handler(index);
    }
}

fn player_subtitle_selected(ui: &AppWindow, state: &Rc<RefCell<State>>, index: i32) {
    let selection = if index < 0 {
        None
    } else {
        match u32::try_from(index) {
            Ok(index) => Some(index),
            Err(_) => return,
        }
    };
    let changed = state.borrow_mut().player.select_subtitle_track(selection);
    if !changed {
        return;
    }
    player_refresh(ui, state);
    let handler = state.borrow().callbacks.player_subtitle_track.clone();
    if let Some(handler) = handler {
        handler(selection);
    }
}

fn player_cycle_rate(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let (rate, handler) = {
        let mut state = state.borrow_mut();
        let current = state.player.rate;
        let next = RATE_STEPS
            .iter()
            .copied()
            .find(|rate| *rate > current + 0.001)
            .unwrap_or(RATE_STEPS[0]);
        state.player.rate = next;
        state.player.note_input();
        (next, state.callbacks.player_rate.clone())
    };
    player_refresh(ui, state);
    if let Some(handler) = handler {
        handler(rate);
    }
}

fn player_toggle_fullscreen(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let (fullscreen, handler) = {
        let mut state = state.borrow_mut();
        state.player_fullscreen = !state.player_fullscreen;
        state.player.note_input();
        (
            state.player_fullscreen,
            state.callbacks.player_fullscreen.clone(),
        )
    };
    ui.set_player_fullscreen(fullscreen);
    player_refresh(ui, state);
    if let Some(handler) = handler {
        handler(fullscreen);
    }
}

fn player_retry(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    player_note_input(ui, state);
    let handler = state.borrow().callbacks.player_retry.clone();
    if let Some(handler) = handler {
        handler();
    }
}

/// `Esc` / `Backspace`：菜单展开时先收菜单，否则提交进度并退出。
///
/// 两段语义都走同一个意图（`.slint` 只负责把键映射成编号），判断留在 Rust：
/// 与详情页版本下拉的 `N4` 修复同一个口径——第一下只收下拉/菜单，再按一下才返回。
fn player_exit_or_close_menus(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    let menus_were_open = {
        let mut state = state.borrow_mut();
        let open = state.player.menus_open();
        if open {
            state.player.close_menus();
        }
        open
    };
    if menus_were_open {
        // 菜单收起算一次输入（控制层保持可见，别刚点完 Esc 就淡出）。
        player_refresh(ui, state);
        return true;
    }
    player_exit(ui, state)
}

/// `Esc`：先提交一次进度，再返回上一页（顺序是硬要求）。
fn player_exit(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    let on_player = matches!(state.borrow().nav.current(), Some(Route::Player { .. }));
    if !on_player {
        return false;
    }
    let position = state.borrow().player.position;
    let handler = state.borrow().callbacks.player_exit.clone();
    if let Some(handler) = handler {
        handler(position);
    }
    // 菜单是播放页的局部状态：退出时必须复位，否则下次进来（或调试通道读
    // `view.player`）还会看到 `audio_menu=true`（体检 R2-4）。
    let menus_were_open = {
        let mut state = state.borrow_mut();
        let open = state.player.menus_open();
        state.player.close_menus();
        open
    };
    if menus_were_open {
        player_refresh(ui, state);
    }
    player::clear_scrub(ui);
    go_back(ui, state)
}

fn handle_player_key(ui: &AppWindow, state: &Rc<RefCell<State>>, key: PlayerKey) -> bool {
    match key {
        PlayerKey::TogglePlay => player_play_pause(ui, state),
        PlayerKey::SeekBack15 => player_seek_relative(ui, state, -15),
        PlayerKey::SeekForward15 => player_seek_relative(ui, state, 15),
        PlayerKey::SeekBack60 => player_seek_relative(ui, state, -60),
        PlayerKey::SeekForward60 => player_seek_relative(ui, state, 60),
        PlayerKey::VolumeUp => player_volume_by(ui, state, crate::viewmodel::VOLUME_STEP),
        PlayerKey::VolumeDown => player_volume_by(ui, state, -crate::viewmodel::VOLUME_STEP),
        PlayerKey::Mute => player_toggle_mute(ui, state),
        PlayerKey::AudioMenu => player_toggle_menu(ui, state, true),
        PlayerKey::SubtitleMenu => player_toggle_menu(ui, state, false),
        PlayerKey::Fullscreen => player_toggle_fullscreen(ui, state),
        PlayerKey::Exit => return player_exit_or_close_menus(ui, state),
    }
    true
}
