// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 应用绑定：把 Slint 回调接到 Rust，维护焦点索引与导航栈。
//!
//! 页面组件本身只负责摆放；键盘意图、路由、焦点移动都在这里与
//! `viewmodel` 中完成，`.slint` 里不写业务判断。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use i_slint_core::window::WindowInner;
use slint::{ComponentHandle, Model};

use crate::pages::{add_server, assets::ImageStore, banner, detail, home, library, player, search};
use crate::viewmodel::{
    AddServerError, AddServerView, BannerData, DetailView, HomeView, ImageKey, LibraryView,
    Navigator, PlayerView, Route, SearchView, TargetInfo, grid_window, move_focus, rebase_focus,
    rebase_home_focus,
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

/// 键盘焦点层（体检 R2-2b）。
///
/// 内容层的扁平焦点（`FocusState`）不变，上面叠两层“chrome”：
///
/// ```text
///   顶栏（Navbar）  ← / → 在 标签 0..N → 搜索按钮 之间移动，Enter 激活，↓ 回下一层
///      ↑↓
///   hero（Hero）    首页 billboard 的「▶ 播放」/「更多信息」，← / → 切换，Enter 激活
///      ↑↓
///   内容（Content） 卡片 / 网格（原来的扁平焦点）
/// ```
///
/// 两层之间的上下穿行只在**当前层的边界**发生：内容层只有第一行按 `↑` 才进上一
/// 层，其余情况仍然是原来的行间移动。`Esc` 在任何 chrome 层只回内容层，不返回
/// 上一页（返回要再按一次）。鼠标点击**不会**进入这两层，所以焦点环不会因为
/// 点一下就留在顶栏上。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FocusLayer {
    /// 内容层（卡片 / 网格）。
    #[default]
    Content = 0,
    /// 首页 hero 的按钮行（播放 / 更多信息）。
    Hero = 1,
    /// 顶栏（库标签 + 搜索按钮）。
    Navbar = 2,
}

impl FocusLayer {
    /// Slint 里的焦点层编号。
    pub const fn index(self) -> i32 {
        self as i32
    }

    /// 从 Slint 的编号还原；未知值按内容层处理。
    pub const fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Hero,
            2 => Self::Navbar,
            _ => Self::Content,
        }
    }

    /// 调试通道（`view` 的 `focus_layer`）用的名字。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Hero => "hero",
            Self::Navbar => "navbar",
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
/// 网格可见窗口的图片键。
type ImagesHandler = Rc<dyn Fn(&[ImageKey])>;
/// 首页某一行的可见图片窗口：`(row, first, count)`，`row = -1` 表示 hero。
type RowImagesHandler = Rc<dyn Fn(i32, i32, i32)>;
/// 页面切换（`set_page` 真正换页后调用）。
type PageHandler = Rc<dyn Fn(Page)>;

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
    // 图片按可见窗口请求：网格上报自己的窗口键；首页逐行上报区间。
    library_images: Option<ImagesHandler>,
    search_images: Option<ImagesHandler>,
    home_images: Option<RowImagesHandler>,
    /// 页面切换（上层据此释放离开页面的图片可见窗口）。
    page_changed: Option<PageHandler>,
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
    // 键盘焦点层与 chrome 层内的焦点下标（体检 R2-2b）。
    //
    // `layer` 是**权限来源**：只有它为 `Hero` / `Navbar` 时，那两层的焦点环才画得出来
    // （`.slint` 侧还会再 AND 一次 `nav-active`）。`nav_focus` 与 `hero_focus` 在
    // 离开层时保留，下次回来还能落在上次的位置（顶栏则固定从当前标签开始）。
    layer: FocusLayer,
    nav_focus: usize,
    hero_focus: usize,
    home: HomeView,
    library: LibraryView,
    detail: DetailView,
    player: PlayerView,
    search: SearchView,

    player_fullscreen: bool,
    // 当前顶部横幅；`None` 表示隐藏。
    banner: Option<BannerData>,
    // 上一次推给引擎的字幕安全区（像素 + 窗口物理尺寸）：不变就不重复发。
    // 窗口尺寸也要进缓存键：引擎按**画面**（letterbox 后）高度换算百分比，
    // 窗口一变百分比就变了，像素没变也得重推一次。
    subtitle_margin: Option<(i64, (u32, u32))>,
    // 上一次上报给图片层的可见窗口键：窗口没变就不重复上报（滚动事件很密）。
    last_library_images: Vec<ImageKey>,
    last_search_images: Vec<ImageKey>,
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
            layer: FocusLayer::Content,
            nav_focus: 0,
            hero_focus: 0,
            home: HomeView::new(),
            library: LibraryView::default(),
            detail: DetailView::default(),
            player: PlayerView::default(),
            search: SearchView::default(),
            player_fullscreen: false,
            banner: None,
            subtitle_margin: None,
            last_library_images: Vec::new(),
            last_search_images: Vec::new(),
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
    /// 本代登记的收集区：`refresh_targets` 推进代次期间由 `TargetBus.register` 写入，
    /// 推进完成后整张换进 `targets`。
    ///
    /// 历史：这里曾是"双缓冲"——读取时把上一代与本代合并，以避开登记要等下一个
    /// 事件回合才发生的空窗期。副作用是条件子树（错误卡「重试」、对话框、暂停时的
    /// 中央图标）消失后，旧坐标会多留一代，只能逐个按视图状态打补丁过滤。
    /// 现在 `refresh_targets` 用 `ensure_tree_instantiated` 当场跑完登记回调，
    /// 不再有空窗期，也就不需要合并旧代。
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

    /// 让所有登记型目标（卡片 / 标签 / 菜单行 / 条件子树）重新上报一次，并**当场**
    /// 用本代结果整张替换清单。
    ///
    /// 1. 先实例化重复元素（见 [`App::instantiate_items`]）：`for` 项是惰性创建的，
    ///    推进 `gen` 只让已存在的元素重新求值；
    /// 2. 推进 `TargetBus.gen`，再调一次 `instantiate_items`——它内部会同步跑掉
    ///    `changed target-gen` 回调，登记在返回前就已完成；
    /// 3. 用本代结果替换 `targets`。消失的条件子树不会留下旧坐标。
    pub fn refresh_targets(&self) {
        self.instantiate_items();
        self.staging.borrow_mut().clear();
        let bus = self.ui.global::<TargetBus>();
        bus.set_gen(bus.get_gen().wrapping_add(1));
        // `ensure_tree_instantiated` 会同步跑掉 `changed target-gen` 回调：
        // 本代登记就在这里当场完成，然后整张替换。
        self.instantiate_items();
        *self.targets.borrow_mut() = std::mem::take(&mut *self.staging.borrow_mut());
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
    ///   点它们只会得到"没反应"的假结论）；关闭后 `dialog.` 目标一律不列
    ///   （声明式清单里的对话框目标是活绑定，关框后仍有坐标，所以要按状态挡掉）。
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
        let declared = self.ui.get_targets();
        for index in 0..declared.row_count() {
            if let Some(target) = declared.row_data(index) {
                let info = TargetInfo::from(target);
                merged.insert(info.id.clone(), info);
            }
        }
        let dialog_open = self.ui.get_add_server_open();
        let prefix = target_prefix(self.page());
        let mut out: Vec<TargetInfo> = merged
            .into_values()
            .filter(|target| target.is_clickable())
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

    /// 当前键盘焦点层（内容 / hero / 顶栏）；调试通道与测试读它。
    pub fn focus_layer(&self) -> FocusLayer {
        self.state.borrow().layer
    }

    /// 顶栏焦点下标（`0..标签数-1` = 库标签，`标签数` = 搜索按钮）。
    ///
    /// 只在 [`FocusLayer::Navbar`] 时有意义：其他层里顶栏没有键盘焦点。
    pub fn nav_focus(&self) -> usize {
        self.state.borrow().nav_focus
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

    /// 按字节预算淘汰最旧的图（保护 `protect` 里在屏上的键），返回被淘汰的键。
    ///
    /// 参数顺序与 [`ImageStore::trim`] 一致（`protect` 在前、`budget` 在后），
    /// 避免两个调用点把预算当成保护集合。
    ///
    /// 上层拿到返回值后要解除这些键的"已请求"标记，它们重新进入视口时才会
    /// 再装载（磁盘缓存命中，很快）。
    pub fn trim_images(&self, protect: &HashSet<ImageKey>, budget: u64) -> Vec<ImageKey> {
        self.images.borrow_mut().trim(protect, budget)
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
        // 焦点**以 UI 属性为准**：悬停（`CardRow.focus-hover`）与键盘都直接写
        // `.slint` 的 `focus-index`，Rust 的 `state.focus.home` 只是缓存（下一次
        // 方向键由 `current_focus` 回写）。每批海报到达时 `apply_images` 会重贴
        // **同一份**视图，若在这里无条件回写 `state.focus.home`，用户眼前的悬停
        // 高亮（以及页面滚动）会被弹回上一次键盘位置。
        //
        // 但数据**真的变了**时要按条目 id 重新定位（[`rebase_home_focus`]）：
        //「继续观看」会按最近播放重排，沿用旧下标会让焦点落到另一部作品上（A3）。
        // 同一份视图重贴时 id 位置不变，结果就是原来的下标，悬停不受影响。
        let current = self.ui.get_home_focus().max(0) as usize;
        let mut state = self.state.borrow_mut();
        // 首页按行上报可见图片窗口是事件驱动的（滚动 / 布局变化）；数据本身变化
        // 不一定改几何，所以数据真变了时推一行代次，让每行重新上报自己的区间。
        // （`apply_image` 每次到图都会重贴**同一份**视图，那不该触发一整轮上报。）
        let changed = state.home != *view;
        // 行几何（hero 有无 / 两行的条数 / 库行的数量与顺序）是否变了：变了下方的
        // 行会整体上下挪位，而 `has-hover` 只在指针事件里更新，指针停着不动时它
        // 还是**旧位置**的悬停（见下面的硬清）。
        let geometry_changed = home_row_geometry_changed(&state.home, view);
        // 越界（数据变少）：钳到第 1 张，并把页面滚回顶部（A2）。
        // 否则焦点虽然写在 0，屏幕还停在底部——Enter 打开的是屏幕外那张。
        let out_of_range = current >= view.card_count();
        let focus = if out_of_range {
            0
        } else {
            rebase_home_focus(&state.home, current, view)
        };
        state.home = view.clone();
        state.focus.home = focus;
        drop(state);
        if self.ui.get_home_focus() != focus as i32 {
            self.ui.set_home_focus(focus as i32);
        }
        // 同一份**空**视图重贴：焦点钳位恒成立（`card_count == 0`），不该每拍都
        // 滚一下页面再推一档硬清——那是无谓的事件。
        let quiet = !changed && view.is_empty();
        if !quiet {
            // 焦点越界被钳回 0：页面滚回顶部（A2，见上）。
            if out_of_range {
                self.ui.invoke_scroll_home_to(0.0);
            }
            // 焦点越界（数据变少）或**行几何变了**：把悬停锁存**硬清**（不重算）。
            //
            // 两种情况都会让行整体上下挪位，而 `has-hover` 只在指针事件里更新——
            // 指针停着不动时它还是**旧位置**的悬停，拿它重算会把放大画到指针已经
            // 不在的卡上（「继续观看」被清空、hero 消失时，焦点仍停在「最近添加」
            // 里是合法的，以前就漏走了下面的 `hover-reset`）。
            if out_of_range || geometry_changed {
                let next = self.ui.get_home_hover_clear().wrapping_add(1);
                self.ui.set_home_hover_clear(next);
            } else if changed {
                // 只有内容替换、几何不变时，才要求各行按每张卡**当前**的
                // `has-hover` 重算悬停（`hover-reset`）。
                //
                // 只按长度判断太窄：条数不变、内容整体替换时，同一个下标已经不是
                // 指针下面那张卡了。同一份视图重贴（每批海报到达时 `apply_images`
                // 走的就是这条路）不推，否则会把指针底下好好的悬停白白清掉再点亮。
                let next = self.ui.get_home_hover_reset().wrapping_add(1);
                self.ui.set_home_hover_reset(next);
            }
        }
        if changed {
            let next = self.ui.get_home_image_gen().wrapping_add(1);
            self.ui.set_home_image_gen(next);
        }
    }

    /// 设置媒体库视图并刷新界面。
    ///
    /// 同一个库**重建 / 后台刷新**时，把焦点**按条目 id** 重新定位
    /// （[`rebase_focus`]）：补齐条目、重排顺序后，沿用旧下标会让焦点落到另一条
    /// 内容上；滚动位置保留（用户正在原地浏览，刷新不该把人弹回顶部）。
    ///
    /// **换了库**（`view.id` 变了）则是另一回事：滚动位置属于上一个库，必须回到
    /// 顶部、焦点回到第 0 张（真机 R6-2：从动画电影滚到 -1080 再切到海外电影，
    /// 新库直接落在半空中）。只在换库时复位，详情页返回（同库、不重建）仍然保留。
    pub fn set_library(&self, view: &LibraryView) {
        {
            let images = self.images.borrow();
            library::apply(&self.ui, view, &images);
        }
        let mut state = self.state.borrow_mut();
        let library_changed = state.library.id != view.id;
        let focus = if library_changed {
            0
        } else {
            rebase_focus(&state.library, state.focus.library, view)
        };
        state.library = view.clone();
        // `rebase_focus` 已经保证下标落在新卡片范围内（新列表为空时是 0）。
        state.focus.library = focus;
        drop(state);
        self.ui.set_library_focus(focus as i32);
        if library_changed {
            // 网格的 `content-y` 是 `.slint` 局部滚动状态，只有换库时才归零。
            self.ui.invoke_reset_library_scroll();
        }
        // 数据一落地就按当前视口窗口化（否则首帧还是"全部实例化"）。
        // 顺便清掉"上次上报的图片窗口"：换了视图要重新上报一次（即使可见的
        // 卡片 id 恰好与旧视图相同，例如刷新前缓存里还没有这些条目）。
        self.state.borrow_mut().last_library_images.clear();
        sync_grid_window(&self.ui, &self.state, Page::Library);
    }

    /// 设置搜索页视图并刷新界面。
    ///
    /// 查询词与搜索框文本保持一致（回填）：只有值真的不同才写，
    /// 否则每次输入都会重置输入框光标。
    pub fn set_search(&self, view: &SearchView) {
        // 先只读地取出顶栏数据：`state` 的 borrow 在进入 `apply` 之前结束，
        // 保证这里不会同时持有 `state` 与 `images` 两个 borrow。
        // 搜索页不属于任何库标签：`-1` = 顶栏不高亮任何一项（真机 R6-4：从
        // 「动画电影」进来后顶栏仍亮着它，看着像搜索结果属于那个库）。
        let tabs = self.state.borrow().home.library_tabs.clone();
        {
            let images = self.images.borrow();
            search::apply(&self.ui, view, &tabs, -1, &images);
        }
        let mut state = self.state.borrow_mut();
        state.search = view.clone();
        if state.focus.search >= view.cards.len() {
            state.focus.search = 0;
        }
        let focus = state.focus.search;
        drop(state);
        self.ui.set_search_focus(focus as i32);
        // 与媒体库同口径：数据一落地就按视口窗口化，并重新上报一次图片窗口。
        self.state.borrow_mut().last_search_images.clear();
        sync_grid_window(&self.ui, &self.state, Page::Search);
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
        leave_player_before_navigating(&self.ui, &self.state);
        open_detail(&self.ui, &self.state, item_id)
    }

    /// 开始播放：压栈、切到播放外壳、通知上层。
    pub fn play(&self, item_id: &str) -> bool {
        play(&self.ui, &self.state, item_id)
    }

    /// 直接切到某个页面（不改变导航栈；演示与测试用）。
    pub fn show_page(&self, page: Page) {
        set_page(&self.ui, &self.state, page);
    }

    /// 回到首页并把导航栈重置为根。
    pub fn show_home(&self) {
        leave_player_before_navigating(&self.ui, &self.state);
        self.state.borrow_mut().nav.reset(Route::Home);
        set_page(&self.ui, &self.state, Page::Home);
    }

    /// 打开媒体库：刷新数据、压栈、切页。已经停在媒体库时不重复压栈。
    pub fn open_library(&self, view: &LibraryView) -> bool {
        leave_player_before_navigating(&self.ui, &self.state);
        self.set_library(view);
        let mut state = self.state.borrow_mut();
        match state.nav.current() {
            // 同一个库重建（后台刷新 / 详情返回）或重复点同一个标签：栈不动。
            Some(Route::Library { id, title }) if *id == view.id && *title == view.title => {}
            // 换库：**替换**栈顶，不是留着旧的那个（体检 R8-A2）。以前只在
            // “栈顶不是 Library”时压栈，于是切库后 `current_route()` 永远是第一个
            // 进的库：调试通道 `view.route` 报旧 id，`restore_route` 之类的
            // “当前路由 == 某库”判断也会对着旧 id 做。返回仍然只回一层
            // （与 `open_search` 换查询词同一做法）。
            Some(Route::Library { .. }) => {
                state.nav.replace_top(Route::Library {
                    id: view.id.clone(),
                    title: view.title.clone(),
                });
            }
            _ => state.nav.push(Route::Library {
                id: view.id.clone(),
                title: view.title.clone(),
            }),
        }
        drop(state);
        set_page(&self.ui, &self.state, Page::Library);
        true
    }

    /// 打开搜索页：刷新数据、压栈（已在搜索页时只更新栈顶查询词）、切页。
    pub fn open_search(&self, view: &SearchView) -> bool {
        leave_player_before_navigating(&self.ui, &self.state);
        self.set_search(view);
        let mut state = self.state.borrow_mut();
        match state.nav.current() {
            Some(Route::Search { term }) if *term == view.term => {}
            Some(Route::Search { .. }) => {
                // 查询词变了：替换栈顶，不重复压栈（返回仍然只回一层）。
                state.nav.replace_top(Route::Search {
                    term: view.term.clone(),
                });
            }
            _ => state.nav.push(Route::Search {
                term: view.term.clone(),
            }),
        }
        drop(state);
        set_page(&self.ui, &self.state, Page::Search);
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
    /// `y` 来自 `AppWindow::player-subtitle-inset`（控制层真实布局），引擎再按
    /// 画面高度换算成 `sub-pos` 百分比。只在（像素值，窗口尺寸）确实变化时发。
    pub fn on_player_subtitle_margin(&self, handler: impl Fn(i64) + 'static) {
        self.state.borrow_mut().callbacks.player_subtitle_margin = Some(Rc::new(handler));
    }

    /// 媒体库网格当前可见窗口的图片键（滚动时补发新进入视口的卡）。
    ///
    /// 只在上报的窗口确实变化时调用：网格窗口化后图片也只该按可见范围请求，
    /// 否则一个 4546 张的库永远只有前 120 张有海报（体检 R6-5）。
    pub fn on_library_images(&self, handler: impl Fn(&[ImageKey]) + 'static) {
        self.state.borrow_mut().callbacks.library_images = Some(Rc::new(handler));
    }

    /// 搜索页网格当前可见窗口的图片键（与媒体库同一套窗口化口径）。
    pub fn on_search_images(&self, handler: impl Fn(&[ImageKey]) + 'static) {
        self.state.borrow_mut().callbacks.search_images = Some(Rc::new(handler));
    }

    /// 首页某一行的可见图片窗口：`(row, first, count)`。
    ///
    /// `row = -1` 是 hero（`count = 1` 表示它在视口内），`0` = 继续观看，
    /// `1` = 最近添加，`2 + n` = 第 n 个库行；`first/count` 是该行的横向可见区间。
    /// 只在该行可见区间变化、重新进入视口或首页数据更新时发。
    pub fn on_home_images(&self, handler: impl Fn(i32, i32, i32) + 'static) {
        self.state.borrow_mut().callbacks.home_images = Some(Rc::new(handler));
    }

    /// 页面切换回调：`set_page` 真正换页后调用，参数是**新页面**。
    ///
    /// 上层用它释放离开页面的图片可见窗口（否则 `ImageScope::HomeRow(n)` 的
    /// `visible` 集合停在离开时的值，那些键永远被当成在屏上的图保护着）。
    pub fn on_page_changed(&self, handler: impl Fn(Page) + 'static) {
        self.state.borrow_mut().callbacks.page_changed = Some(Rc::new(handler));
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
    // 网格窗口化：页面报"视口 / 几何变了"（滚动、resize、列数、总数）时重算
    // 实例化区间。回调里只做整数/浮点运算与两个属性写入，不碰数据层。
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_library_grid_viewport_changed(move || {
            if let Some(ui) = weak.upgrade() {
                sync_grid_window(&ui, &state, Page::Library);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_search_grid_viewport_changed(move || {
            if let Some(ui) = weak.upgrade() {
                sync_grid_window(&ui, &state, Page::Search);
            }
        });
    }
    {
        let state = state.clone();
        // 首页行自己按可见区间上报（行内 content-x / 卡宽 + 页面纵向可见的行）。
        ui.on_home_image_window(move |row, first, count| {
            // 不拿着 state 的 borrow 回调：上层收到后可能重建视图。
            let handler = state.borrow().callbacks.home_images.clone();
            if let Some(handler) = handler {
                handler(row, first, count);
            }
        });
    }
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
        // 搜索页输入框 ↓：焦点交给结果网格的第一张卡（R7-D1）。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_search_grid_focus(move || {
            if let Some(ui) = weak.upgrade() {
                focus_search_grid(&ui, &state);
            }
        });
    }
    {
        // 搜索页输入框 ↑：进顶栏（标签仍键盘可达）。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_search_navbar_focus(move || {
            if let Some(ui) = weak.upgrade() {
                focus_search_navbar(&ui, &state);
            }
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
    {
        // 控制层布局 / 窗口高度变化：只重算字幕安全区，别的什么都不动。
        let weak = ui.as_weak();
        let state = state.clone();
        ui.on_player_subtitle_reflow(move || {
            if let Some(ui) = weak.upgrade() {
                let view = state.borrow().player.clone();
                player_push_subtitle_margin(&ui, &state, &view);
            }
        });
    }
}

fn focus_rows(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) -> Vec<usize> {
    let columns = ui.get_grid_columns().max(1) as usize;
    let state = state.borrow();
    match page {
        Page::Home => home::focus_rows(&state.home),
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

/// 网格窗口化：按**当前视口**重算需要实例化的卡片区间。
///
/// 大库（海外电影 4546 张）以前会一次性实例化全部卡片。现在只实例化"可见行 ±
/// 缓冲行"，区间由纯函数 [`grid_window`] 算：布局几何（行高 / 网格起点 / 视口高）
/// 全部从 `.slint` 侧的 `probe-*` 属性读，Rust 不重算一遍布局公式；卡片总数也来自
/// 同一份数据（`State` 里的视图），列数用 `grid-columns`。
///
/// 触发点：
/// 1. 视图落地（`App::set_library` / `App::set_search`）；
/// 2. 页面报"视口 / 几何变了"（`PosterGrid.viewport-changed`）：滚动、窗口 resize、
///    列数变化、卡片总数变化；
/// 3. 键盘焦点移动后 `scroll-to-index` 改了滚动偏移（同样走第 2 条）。
///
/// 同一个窗口还会作为**图片请求的来源**（[`Callbacks::library_images`] /
/// [`Callbacks::search_images`]）：只请求可见行 ± 缓冲行里的海报，窗口没变就不
/// 重复上报。这是 R6-5 的修法——以前按整页 `take(120)` 截断，视口外的键永远轮不到。
fn sync_grid_window(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) {
    let columns = ui.get_grid_columns().max(1) as usize;
    let (total, scroll_y, viewport_h, grid_top, row_height, set_start, set_count) = match page {
        Page::Library => (
            state.borrow().library.cards.len(),
            // `content-y` 向下滚为负；窗口化用内容坐标里的视口上沿。
            -f64::from(ui.get_library_scroll_y()),
            f64::from(ui.get_library_viewport_h()),
            f64::from(ui.get_library_grid_top()),
            f64::from(ui.get_library_grid_row_h()),
            AppWindow::set_library_window_start as fn(&AppWindow, i32),
            AppWindow::set_library_window_count as fn(&AppWindow, i32),
        ),
        Page::Search => (
            state.borrow().search.cards.len(),
            -f64::from(ui.get_search_scroll_y()),
            f64::from(ui.get_search_viewport_h()),
            f64::from(ui.get_search_grid_top()),
            f64::from(ui.get_search_grid_row_h()),
            AppWindow::set_search_window_start as fn(&AppWindow, i32),
            AppWindow::set_search_window_count as fn(&AppWindow, i32),
        ),
        Page::Home | Page::Detail | Page::Player => return,
    };
    let window = grid_window(scroll_y, viewport_h, grid_top, row_height, columns, total);
    set_start(ui, window.start as i32);
    set_count(ui, window.count as i32);

    // 图片：只要可见窗口里的键（与实例化区间同一个几何口径）。
    let (keys, changed, handler) = {
        let mut state = state.borrow_mut();
        match page {
            Page::Library => {
                let keys = state.library.image_window_keys(window.start, window.count);
                let changed = state.last_library_images != keys;
                if changed {
                    state.last_library_images = keys.clone();
                }
                (keys, changed, state.callbacks.library_images.clone())
            }
            Page::Search => {
                let keys = state.search.image_window_keys(window.start, window.count);
                let changed = state.last_search_images != keys;
                if changed {
                    state.last_search_images = keys.clone();
                }
                (keys, changed, state.callbacks.search_images.clone())
            }
            Page::Home | Page::Detail | Page::Player => return,
        }
    };
    if !changed {
        return;
    }
    if let Some(handler) = handler {
        handler(&keys);
    }
}

fn handle_key(ui: &AppWindow, state: &Rc<RefCell<State>>, command: KeyCommand) -> bool {
    // 对话框打开时：键盘不透给被遮罩的首页（Esc 已由 .slint 直接关框）。
    if ui.get_add_server_open() {
        return true;
    }
    let page = Page::from_index(ui.get_page());
    // chrome 层（hero / 顶栏）的按键单独走：方向键在层内移动、Enter 激活层内控件、
    // Esc 退回内容层。内容层完全保持原来的行为（体检 R2-2b）。
    let layer = state.borrow().layer;
    if layer != FocusLayer::Content && page != Page::Player {
        return handle_chrome_key(ui, state, page, layer, command);
    }
    match command {
        // 播放页没有焦点环：Enter / Space 交给"播放/暂停"，不重新起播。
        KeyCommand::Accept if page == Page::Player => {
            player_play_pause(ui, state);
            true
        }
        KeyCommand::Left => move_focus_by(ui, state, page, -1, 0),
        KeyCommand::Right => move_focus_by(ui, state, page, 1, 0),
        KeyCommand::Up => move_up(ui, state, page),
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
        KeyCommand::ConnectServer => open_connect_dialog(state),
    }
}

/// Ctrl+N / 空态按钮 / 横幅动作共用的“连接服务器”回调。
fn open_connect_dialog(state: &Rc<RefCell<State>>) -> bool {
    let handler = state.borrow().callbacks.connect_server.clone();
    if let Some(handler) = handler {
        handler();
    }
    true
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
    // 首页是 N 条横向行（继续观看 / 最近添加 / 各库行）：纵向滚动按**可视行号**
    // 在 `HomePage.scroll-to-index` 里算行上沿（库行是 `for` 重复元素，页面拿不到
    // 它们的 `y`）；库行的横向滚动由页面下发「键盘滚到本行第 N 张」触发，悬停只
    // 同步焦点、不滚动（R2-2 / R7-2）。可视行号必须在这里算：`focus_rows` 会跳过
    // 空行，它的下标不能当界面行号用。
    scroll_focus_into_view(ui, state, page);
    true
}

/// 把当前焦点卡重新滚进视口（焦点没变也要重新下发请求）。
///
/// 两个调用点都需要它：
/// 1. 方向键移动后（行内 / 行间）；
/// 2. 从 chrome 层（顶栏 / hero）回到内容层时（A1）：用户可能在离开期间用滚轮
///    把那一行滚走了，进门时的 row/local 与上次相同，必须重新请求才能回来。
fn scroll_focus_into_view(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) {
    let index = current_focus(ui, state, page);
    match page {
        Page::Home => {
            let row = state.borrow().home.focus_row_at(index).unwrap_or(0);
            ui.invoke_scroll_home_to_index(index as i32, row as i32);
        }
        Page::Library => ui.invoke_scroll_library_to_index(index as i32),
        Page::Search => ui.invoke_scroll_search_to_index(index as i32),
        Page::Detail | Page::Player => {}
    }
}

// ---- 键盘焦点层：hero / 顶栏（体检 R2-2b）----

/// 当前页顶栏的标签。
///
/// 数据源必须与页面真正画的保持一致：首页 / 搜索页用首页的标签（`search::apply`
/// 就借的这份），媒体库页用自己的。
fn nav_tabs(state: &State, page: Page) -> &[crate::viewmodel::LibraryTab] {
    match page {
        Page::Library => &state.library.library_tabs,
        _ => &state.home.library_tabs,
    }
}

/// 顶栏可聚焦槽位数：标签 `0..N-1` + 搜索按钮（下标 `N`）。
///
/// 搜索页的顶栏把胶囊搜索框藏起来了（`show-search: false`，页内已经有更大的输入框），
/// 那个页面只有标签可聚焦——否则会多出一个“按 Enter 没反应”的空槽。
fn nav_slots(state: &State, page: Page) -> usize {
    let tabs = nav_tabs(state, page).len();
    if page == Page::Search { tabs } else { tabs + 1 }
}

/// 首页的**行几何**是否变了：hero 有无、「继续观看」/「最近添加」的卡片数、
/// 库行的数量与顺序（按 `library_id` 比）。
///
/// 变化意味着下方各行的纵向位置整体挪动（例如「继续观看」被清空、hero 消失）。
/// `has-hover` 是边沿事件，指针不动就不会更新，所以这种时候不能按旧的 `has-hover`
/// 重算悬停——见 [`App::set_home`] 里的硬清。
fn home_row_geometry_changed(previous: &HomeView, next: &HomeView) -> bool {
    previous.hero.is_some() != next.hero.is_some()
        || previous.continue_watching.len() != next.continue_watching.len()
        || previous.recently_added.len() != next.recently_added.len()
        || previous.library_rows.len() != next.library_rows.len()
        || previous
            .library_rows
            .iter()
            .zip(&next.library_rows)
            .any(|(previous, next)| previous.library_id != next.library_id)
}

/// 首页 hero 的按钮行是否真的可聚焦。
///
/// `home.slint` 只在 `hero.overview != ""` 时实例化那两个按钮（条件子树的布局收紧是
/// 有意的），没有简介时进 hero 层就是一个看得见焦点、却点不到东西的死层。
fn hero_available(state: &State) -> bool {
    state.home.hero.as_ref().is_some_and(|hero| {
        hero.overview
            .as_deref()
            .is_some_and(|text| !text.is_empty())
    })
}

fn set_focus_layer(ui: &AppWindow, state: &Rc<RefCell<State>>, layer: FocusLayer) {
    state.borrow_mut().layer = layer;
    ui.set_focus_layer(layer.index());
}

fn set_nav_focus_index(ui: &AppWindow, state: &Rc<RefCell<State>>, index: usize) {
    state.borrow_mut().nav_focus = index;
    ui.set_nav_focus(index as i32);
}

fn set_hero_focus_index(ui: &AppWindow, state: &Rc<RefCell<State>>, index: usize) {
    state.borrow_mut().hero_focus = index;
    ui.set_hero_focus(index as i32);
}

/// 回到内容层：焦点环娥灭，内容网格重新接管键盘放大指示。
fn leave_chrome_layer(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    if state.borrow().layer == FocusLayer::Content {
        return false;
    }
    set_focus_layer(ui, state, FocusLayer::Content);
    true
}

fn enter_hero(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    set_hero_focus_index(ui, state, 0);
    set_focus_layer(ui, state, FocusLayer::Hero);
    true
}

/// 顶栏**当前高亮**的标签下标（必须与页面真正画出来的一致）。
///
/// 首页写死 0（`convert::home_data` 的 `current-tab` 就是 0）；搜索页不属于任何
/// 库，没有高亮项（真机 R6-4 的 `current-tab` 传 `-1`），但键盘进入顶栏时仍要
/// 有一个合理起点，所以这里返回第一个标签（0）。
fn nav_current_tab(state: &State, page: Page) -> usize {
    match page {
        Page::Home => 0,
        Page::Library => state.library.current_tab,
        Page::Search | Page::Detail | Page::Player => 0,
    }
}

/// 进入顶栏：焦点落在**当前标签**上，让标签横向滚入可见区由 `Navbar` 负责
/// （它的 `focus-index` 一变就会 `ensure-visible`）。
fn enter_navbar(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) -> bool {
    let slots = nav_slots(&state.borrow(), page);
    if slots == 0 {
        return false;
    }
    let index = nav_current_tab(&state.borrow(), page).min(slots - 1);
    set_nav_focus_index(ui, state, index);
    set_focus_layer(ui, state, FocusLayer::Navbar);
    true
}

/// 内容层按 `↑`：只在**第一行**再往上时进 chrome 层（首页先 hero，其余直接顶栏）。
fn move_up(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) -> bool {
    if matches!(page, Page::Player | Page::Detail) {
        // 详情页没有顶栏（页内是 hero + 按钮，焦点已能到播放 / 版本行），播放页
        // 有自己的快捷键表：这两页没有 chrome 层，保持原来的行间移动。
        return move_focus_by(ui, state, page, 0, -1);
    }
    let rows = focus_rows(ui, state, page);
    let index = current_focus(ui, state, page);
    let first_row = rows.first().copied().unwrap_or(0);
    let at_first_row = rows.is_empty() || index < first_row;
    if !at_first_row {
        return move_focus_by(ui, state, page, 0, -1);
    }
    if page == Page::Search {
        // 搜索页首行 ↑：回到页内大搜索框（R7-D1）。搜索框里再按 ↑ 才进顶栏
        // （见 `focus_search_navbar`），所以标签仍然键盘可达。
        return focus_search_input(ui);
    }
    if page == Page::Home && hero_available(&state.borrow()) {
        return enter_hero(ui, state);
    }
    enter_navbar(ui, state, page)
}

/// 把键盘焦点交给搜索页的页内大搜索框。
///
/// `search-focused` 必须与"输入框真的是按键目标"一致：只 `focus()` 不置属性会让
/// 晚到的 `set_search` 按“用户没在输入”回填旧查询词。
fn focus_search_input(ui: &AppWindow) -> bool {
    ui.set_search_focused(true);
    ui.invoke_focus_search_input();
    true
}

/// 搜索页输入框 ↓：把键盘焦点交给结果网格的第一张卡（R7-D1）。
///
/// 页内输入框是 `TextInput`：它会把 ↑ / ↓ 当编辑键吃掉，事件也冒泡不到兄弟节点
/// `keyboard`（方向键因此完全无法进网格）。`.slint` 侧识别到这两个键后先
/// `clear-focus()` + `search-blur()`（与 Enter 提交同一条路径），再回调到这里；
/// 焦点数学仍然只在 Rust 里做。
fn focus_search_grid(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    if Page::from_index(ui.get_page()) != Page::Search {
        return false;
    }
    set_focus_layer(ui, state, FocusLayer::Content);
    set_focus(ui, state, Page::Search, 0);
    // 键盘已开始导航：网格的放大指示生效（与全局按键处理器同一口径）。
    ui.set_nav_active(true);
    scroll_focus_into_view(ui, state, Page::Search);
    true
}

/// 搜索页输入框 ↑：进顶栏（与首页 / 媒体库的 chrome 层同一套）。
fn focus_search_navbar(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    if Page::from_index(ui.get_page()) != Page::Search {
        return false;
    }
    enter_navbar(ui, state, Page::Search)
}

/// chrome 层（hero / 顶栏）里的方向键：层内移动，不碰内容层的扁平焦点。
fn move_chrome_focus(
    ui: &AppWindow,
    state: &Rc<RefCell<State>>,
    page: Page,
    layer: FocusLayer,
    dx: i32,
) -> bool {
    match layer {
        FocusLayer::Hero => {
            // hero 只有两个按钮（播放 / 更多信息）：在一端夹住。
            let current = state.borrow().hero_focus.min(1);
            let next = if dx > 0 {
                (current + 1).min(1)
            } else {
                current.saturating_sub(1)
            };
            if next != current {
                set_hero_focus_index(ui, state, next);
            }
            true
        }
        FocusLayer::Navbar => {
            let slots = nav_slots(&state.borrow(), page);
            if slots == 0 {
                return false;
            }
            let current = state.borrow().nav_focus.min(slots - 1);
            let next = if dx > 0 {
                (current + 1).min(slots - 1)
            } else {
                current.saturating_sub(1)
            };
            if next != current {
                set_nav_focus_index(ui, state, next);
            }
            true
        }
        FocusLayer::Content => false,
    }
}

/// hero 层 Enter：0 = 播放（与点击「▶ 播放」同一条回调链），1 = 打开详情。
fn activate_hero(ui: &AppWindow, state: &Rc<RefCell<State>>) -> bool {
    if state.borrow().hero_focus == 0 {
        // `.slint` 里点「▶ 播放」走的就是这个回调（`wire` 接的是 `play`）。
        ui.invoke_hero_play();
        return true;
    }
    let item_id = state
        .borrow()
        .home
        .hero
        .as_ref()
        .map(|hero| hero.item_id.clone());
    match item_id {
        Some(item_id) => open_detail(ui, state, &item_id),
        None => false,
    }
}

/// 顶栏 Enter：标签 → 切库（与点击标签同一条回调），搜索槽 → 展开并聚焦输入框。
fn activate_navbar(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) -> bool {
    let slots = nav_slots(&state.borrow(), page);
    if slots == 0 {
        return false;
    }
    let index = state.borrow().nav_focus.min(slots - 1);
    let tab = {
        let state = state.borrow();
        nav_tabs(&state, page).get(index).cloned()
    };
    // 选完就回内容层：顶栏是一层“选完就走”的 chrome（鼠标点标签时焦点环从来不亮）。
    leave_chrome_layer(ui, state);
    match tab {
        Some(tab) => {
            ui.invoke_tab_activated(tab.id.as_str().into());
            true
        }
        None => {
            focus_search(ui, state);
            true
        }
    }
}

/// chrome 层的按键：方向键层内移动、Enter 激活层内控件、Esc 退回内容层。
///
/// Esc / Backspace 在 chrome 层**只退层**，不返回上一页（返回要再按一次）：
/// 遥控器上“退一层”与“退一页”是两个不同意图，混在一起会让用户莫名其妙地
/// 从媒体库页直接被丢回首页。
fn handle_chrome_key(
    ui: &AppWindow,
    state: &Rc<RefCell<State>>,
    page: Page,
    layer: FocusLayer,
    command: KeyCommand,
) -> bool {
    match command {
        KeyCommand::Left => move_chrome_focus(ui, state, page, layer, -1),
        KeyCommand::Right => move_chrome_focus(ui, state, page, layer, 1),
        KeyCommand::Up => match layer {
            // 顶栏是最上一层；hero 再往上就是顶栏。
            FocusLayer::Navbar => false,
            FocusLayer::Hero => enter_navbar(ui, state, page),
            FocusLayer::Content => false,
        },
        KeyCommand::Down => {
            // 首页从顶栏往下先回 hero（若 hero 真的可聚焦），其余直接回内容层。
            if layer == FocusLayer::Navbar && page == Page::Home && hero_available(&state.borrow())
            {
                return enter_hero(ui, state);
            }
            let left = leave_chrome_layer(ui, state);
            if left {
                // 回到内容层时重新下发一次滚动请求（A1）：离开期间可能被滚轮滚走。
                scroll_focus_into_view(ui, state, page);
            }
            left
        }
        KeyCommand::Accept => match layer {
            FocusLayer::Hero => activate_hero(ui, state),
            FocusLayer::Navbar => activate_navbar(ui, state, page),
            FocusLayer::Content => false,
        },
        KeyCommand::Back => {
            let left = leave_chrome_layer(ui, state);
            if left {
                // Esc / Backspace 只退层：退回来时也要把焦点卡重新滚进视口（A1）。
                scroll_focus_into_view(ui, state, page);
            }
            left
        }
        KeyCommand::Search => {
            leave_chrome_layer(ui, state);
            focus_search(ui, state);
            true
        }
        KeyCommand::ConnectServer => open_connect_dialog(state),
    }
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
    set_page(ui, state, Page::Detail);
    if let Some(handler) = handler {
        handler(item_id);
    }
    true
}

/// 播放页上的这一条是否已经播完（引擎报过 `Ended`）。
///
/// 只看 `PlayerView::ended`——它由 nova-app 的 `Player::fill_view` 从
/// `SessionState::Ended` 直传，不靠位置推。`position >= duration` 不可靠：时长
/// 未知时位置推不到时长（永远判为 false，播完 Enter 没反应），续播点贴片尾时位置
/// 在 Opening / 缓冲首帧里就已 >= 时长（误判成播完，Enter 把片子重开）。
///
/// `!playing` 是防御：正在缓冲 / 起播的会话即使上一拍留下了 `ended`，也不能当
/// 播完。
fn player_finished(view: &PlayerView) -> bool {
    view.ended && !view.playing
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
    let (target, handler) = {
        let mut state = state.borrow_mut();
        // 起播条目以**路由**为准：详情页按钮传上来的是渲染出来的 `detail.item-id`
        // （`app.slint` 的 `root.play-item(root.detail.item-id)`）。路由与渲染数据
        // 分叉的那一帧（迟到的详情刚被丢弃、界面还没重贴）按渲染数据起播会播错片
        // （体检 R7-D3）。已经在播放页时以调用方给的 id 为准（换片）。
        let target = match state.nav.current() {
            Some(Route::Detail { item_id }) => item_id.clone(),
            _ => item_id.to_owned(),
        };
        // 已经在播的就是这一条：什么都不做。
        //
        // 播放页按 Enter 走 `play_current` → `play`，以前会把同一个 `Player{A}`
        // 弹了再压一次：播放页闪一下、重新取直链、重开 mpv（连进度位置都重算）。
        //
        // **例外**：片子自然播完（Ended）或起播失败停在错误卡上时，Enter 就是
        // “重播”。这两个状态都不是“还在放”，提前返回会让用户只能退出播放页
        // 再进来才能重新看。
        if matches!(
            state.nav.current(),
            Some(Route::Player { item_id: playing }) if *playing == target
        ) && state.player.error.is_none()
            && !player_finished(&state.player)
        {
            return true;
        }
        // 已经在播放页再播另一条：**替换**栈顶而不是叠加，返回仍然只回一层。
        //
        // 返回语义：换片后按返回要回到**新片 B 的详情页**，而不是上一部 A 的——
        // 栈是 `[.., Detail{A}, Player{B}]` 时把 `Detail{A}` 换成 `Detail{B}`，
        // 与“从 B 的详情页起播”同义（否则返回看到的是 A 的标题 / 简介 / 播放按钮）。
        // 其他情形（下层不是详情页，或下层就是 `Detail{B}`）保持原样。
        if matches!(state.nav.current(), Some(Route::Player { .. })) {
            state.nav.pop();
            let stale_detail = matches!(
                state.nav.current(),
                Some(Route::Detail { item_id: below }) if *below != target
            );
            if stale_detail {
                state.nav.replace_top(Route::Detail {
                    item_id: target.clone(),
                });
            }
        }
        state.nav.push(Route::Player {
            item_id: target.clone(),
        });
        (target, state.callbacks.play.clone())
    };
    set_page(ui, state, Page::Player);
    if let Some(handler) = handler {
        handler(&target);
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
    set_page(ui, state, page);
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
///
/// 换页同时**退出 chrome 层**（体检 R2-2b）：键盘焦点层属于上一页，不重置的话
/// 新页面的顶栏会凭空亮着焦点环（且按 Enter 会激活未选中的标签）。重置写在
/// 这里而不是 `.slint` 的 `changed page`，理由同上：`changed` 是延迟的，而
/// `handle_key` 在下一次按键时就读 `state.layer`。
fn set_page(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) {
    let changed = Page::from_index(ui.get_page()) != page;
    ui.set_page(page.index());
    if changed {
        ui.invoke_focus_global();
        set_focus_layer(ui, state, FocusLayer::Content);
        set_nav_focus_index(ui, state, 0);
        set_hero_focus_index(ui, state, 0);
        // 图片晚到时只重贴**当前路由**的视图（一批图一次，见 `nova-app` 的
        // `drain`）：离屏期间到达的图因此不会进入别的页面的模型。切回这一页时
        // 在这里统一补一次，两边的语义才对得上。
        reapply_page(ui, state, page);
        // 媒体库 / 搜索页的网格可见窗口在离屏时被上层释放了：换回来时清掉
        // "上次上报"并重新上报（`begin_window` 会去重，不会重复下载）。
        match page {
            Page::Library => {
                state.borrow_mut().last_library_images.clear();
                sync_grid_window(ui, state, Page::Library);
            }
            Page::Search => {
                state.borrow_mut().last_search_images.clear();
                sync_grid_window(ui, state, Page::Search);
            }
            Page::Home | Page::Detail | Page::Player => {}
        }
        let handler = state.borrow().callbacks.page_changed.clone();
        if let Some(handler) = handler {
            handler(page);
        }
    }
}

/// 把某一页当前持有的视图重新写进 Slint（用于切页时补上离屏期间晚到的图）。
///
/// 只碰 Slint 属性，不重建视图模型、不改焦点 / 滚动状态。
fn reapply_page(ui: &AppWindow, state: &Rc<RefCell<State>>, page: Page) {
    let state = state.borrow();
    let images = state.images.borrow();
    match page {
        Page::Home => home::apply(ui, &state.home, &images),
        Page::Library => library::apply(ui, &state.library, &images),
        Page::Detail => detail::apply(ui, &state.detail, &images),
        // 搜索页不属于任何库标签（`-1` = 顶栏无高亮），与 `App::set_search` 同口径。
        Page::Search => search::apply(ui, &state.search, &state.home.library_tabs, -1, &images),
        Page::Player => {}
    }
}

fn focus_search(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // 顶栏搜索框只存在于首页 / 媒体库页；搜索页有自己的大输入框。详情页与播放页
    // 没有任何输入框可聚焦——只把 `search-focused` 置真会让焦点落空，随后字符被
    // 全局按键处理器复位（体检 R5-4）。这两页直接打开搜索页并聚焦页内大输入框。
    match Page::from_index(ui.get_page()) {
        Page::Detail => open_empty_search(ui, state),
        // 播放页先走正常出口：提交进度、收尾会话，再进搜索（体检 R5-3）。
        Page::Player => {
            leave_player_before_navigating(ui, state);
            open_empty_search(ui, state);
        }
        Page::Home | Page::Library | Page::Search => {}
    }
    ui.set_search_focused(true);
    ui.invoke_focus_search_input();
    let handler = state.borrow().callbacks.search.clone();
    if let Some(handler) = handler {
        handler();
    }
}

/// 打开一个空查询的搜索页（详情页 / 播放页的 Ctrl+F）。
///
/// 详情页没有顶栏搜索框、播放页也没有输入框，所以不能像首页 / 媒体库那样
/// "只聚焦等输入"：直接切页 + 聚焦页内大输入框，用户敲的第一个字符就走
/// `search-input` 回调（体检 R5-4）。视图用 Idle（与搜索页「×」清空后一致），
/// 顶栏标签数据沿用当前页。
fn open_empty_search(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let (tabs, images) = {
        let state = state.borrow();
        (state.home.library_tabs.clone(), state.images.clone())
    };
    let view = SearchView::default();
    // 搜索页顶栏无高亮（`-1`），与 `set_search` 同一口径（真机 R6-4）。
    search::apply(ui, &view, &tabs, -1, &images.borrow());
    {
        let mut state = state.borrow_mut();
        state.search = view;
        state.focus.search = 0;
        state.nav.push(Route::Search {
            term: String::new(),
        });
    }
    ui.set_search_focus(0);
    ui.set_search_text("".into());
    set_page(ui, state, Page::Search);
}

/// 搜索页「×」：清空关键词但**留在搜索页**，焦点交回输入框。
///
/// 与 Esc 分开：Esc（`search-clear`）是“清空 + 回首页”，而点「×」只是
/// 撤掉这次查询——把用户从自己刚打开的结果页踢走不是那个按钮的意思。
/// 路由不变，只换一份空视图（Idle）：否则清空后结果网格还留着上一轮的卡片。
fn clear_search_input(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    // 先只读地取出顶栏数据，`state` 的 borrow 在 `apply` 前结束（同 `set_search`）。
    let (tabs, images) = {
        let state = state.borrow();
        (state.home.library_tabs.clone(), state.images.clone())
    };
    let view = SearchView::default();
    // 搜索页顶栏无高亮（`-1`），与 `set_search` 同一口径（真机 R6-4）。
    search::apply(ui, &view, &tabs, -1, &images.borrow());
    {
        let mut state = state.borrow_mut();
        state.search = view;
        state.focus.search = 0;
        // 路由顶端的查询词跟着清空：否则从结果点进详情再返回，栈顶还挂着旧词。
        if matches!(state.nav.current(), Some(Route::Search { .. })) {
            state.nav.replace_top(Route::Search {
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

/// 把字幕安全区推给引擎（最终换算成 `sub-pos`）：控制层可见时抬起，隐藏时归零。
///
/// 入参是 Slint 按控制层**真实布局**算出的“控制块顶边到窗口底边”+ theme 安全间距
/// （`AppWindow::player-subtitle-inset`，逻辑像素），不是窗口高度乘系数：
/// 固定系数在矮窗口（800×600 控制块约占 42%）下盖不住控制层。
/// 百分比换算在引擎侧做（那里才知道 letterbox 后的画面高度）。
/// 只在（像素值，窗口尺寸）真正变化时发，所以 250ms 的 tick 不会刷屏；
/// 但窗口 resize 时即使像素值不变也会重推，让引擎按新画面高度重算。
fn player_push_subtitle_margin(ui: &AppWindow, state: &Rc<RefCell<State>>, view: &PlayerView) {
    let inset = ui.get_player_subtitle_inset();
    let margin = view.subtitle_margin(inset);
    let size = ui.window().size();
    let window = (size.width, size.height);
    let handler = {
        let mut state = state.borrow_mut();
        if state.subtitle_margin == Some((margin, window)) {
            return;
        }
        state.subtitle_margin = Some((margin, window));
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
    // 错误态没有可跳转的媒体：引擎会拒绝 seek，而这里的乐观更新会让进度数字
    // 跳一下又回不去——界面上的进度条 / ±15s 已在错误态禁用，键盘与拖动同样不做。
    if state.borrow().player.error.is_some() {
        state.borrow_mut().player.note_input();
        player_refresh(ui, state);
        return;
    }
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

/// 路由要离开播放页时，先把播放会话收尾。
///
/// 播放页的正常出口是 Esc / 左上返回（[`player_exit`]）；但导航 API 也能直接换页——
/// 调试通道的 `route` / `open` / `tab` / `search` 就走 `open_detail` / `open_library` /
/// `open_search` / `show_home`。会话不属于导航栈：不先收尾的话 mpv 会在后台继续播，
/// 上一会话的错误也会留在 `view.player` 里（体检 R4-2）。
fn leave_player_before_navigating(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    if matches!(state.borrow().nav.current(), Some(Route::Player { .. })) {
        player_exit(ui, state);
    }
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
