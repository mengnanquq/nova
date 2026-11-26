# UI 架构与视觉规范（阶段 3）

阶段 3 的目标是**能看**：首页、媒体库、搜索页、详情页、播放页外壳，能一路点进去，图片有占位与失败退化，
键盘/遥控器能导航。不做播放（阶段 4）、不做设置页、不做浅色主题。

## 分层与接缝

```
nova-app（可执行 nova-desktop）
   ├─ 读 SQLite 缓存 / 调 provider 刷新 / 下载并解码图片
   └─ 把 nova-core 模型压成 nova-ui 的"视图模型"，塞进 Slint
        │
        ▼
nova-ui（Slint 标记 + 纯 Rust 视图模型）
   ├─ 不含任何 I/O、async、reqwest、rusqlite
   └─ 只认视图模型与 slint::Image
```

**为什么这样切**：视图模型是纯数据，能用普通单测覆盖分组/排序/格式化（这是界面最容易错的地方）；
Slint 标记只负责摆放和动画。这样界面测试不需要网络和数据库，也不需要显示服务器。

## 核心类型（`nova-ui`，全部纯 Rust）

```rust
pub struct CardView {
    pub item_id: String,
    pub title: String,
    pub subtitle: Option<String>,   // "第 2 季 · 第 5 集" / "2021 · 动作"
    pub progress: Option<f32>,      // 0.0..=1.0，仅"继续观看"
    pub badge: Option<String>,      // "4K" / "HDR" / "S01E05"
}

pub struct HomeView {
    pub hero: Option<CardView>,               // 最上面那张大图
    pub continue_watching: Vec<CardView>,
    pub recently_added: Vec<CardView>,
    pub library_tabs: Vec<LibraryTab>,        // 顶部导航：电影 / 剧集 / …
}

pub struct DetailView {
    pub item_id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub meta_line: String,          // "2021 · 2 小时 18 分 · 动作 / 科幻"
    pub overview: Option<String>,
    pub play_label: String,         // "播放" / "从 12:34 继续"
    pub sources: Vec<SourceOption>, // 多版本时让用户选
}

pub struct PlayerShellView { title: String, position: f32, status: String }

// 搜索页（独立结果页）。查询本身在上层，这里只装结果。
pub struct SearchView {
    pub term: String,          // 当前查询词（回填搜索框）
    pub cards: Vec<CardView>,  // 只有电影 / 剧集
    pub state: SearchState,    // Idle / Loading / Ready / Empty / Error(String)
    pub total: usize,          // 命中数（服务端总数，或本页卡片数）
}
```

导航也是纯 Rust：

```rust
pub enum Route {
    Home,
    Library { id: String, title: String },
    Detail { item_id: String },
    Player { item_id: String },
    Search { term: String },
}
pub struct Navigator { /* 栈 + back() */ }
```

`.slint` 侧的页面编号：`0` 首页 / `1` 媒体库 / `2` 详情 / `3` 播放 / `4` 搜索
（`AppWindow.page`，`Page::index()` 与之对应）。新页面**不重排已有编号**，只在末尾追加。

## 搜索页（独立路由）

搜索不再"就地过滤首页"：`Route::Search { term }` 是一个真正的页面
（`ui/search.slint` + `pages/search.rs` + `viewmodel/search.rs`）。

数据流（窗口模式）：

```text
输入（顶栏或页内大搜索框）
  └─ AppWindow.search-changed / search-input ──> nova-ui 回调
       └─ nova-app: Shell::begin_search(term, debounce=true)
            ├─ 本地缓存：Local::search_view（SQLite，同步只读）→ App::open_search（立刻出结果）
            └─ 服务端：runtime.spawn { sleep(300ms) 防抖 → provider.items(ItemQuery{search, kinds=[Movie,Series], limit:60})
                         → cache_items → views::search_view_from_items → channel.post(UiMessage::Search{generation, view}) }
```

要点：

- **代次丢弃**：`Shell.search_generation: Arc<AtomicU64>` 每次输入 +1。防抖任务在发请求前、
  请求返回后各检查一次；主线程 `drain` 里也再检查一次，所以晚到的旧结果不会覆盖新结果；
- **离线可用**：`Local::search_view` 直接读缓存（标题包含过滤 + 只留电影/剧集），
  没有 provider 时本地结果就是最终结果；
- **图片**：搜索页的图片与其他页走同一条 `ImageKey`/`request_images` 路径；`apply_image`
  重新应用模型时把 `state.search` 也重贴一遍（否则海报晚到后不显示）；
- **按钮/重试**：`UiMessage::Search` 只在主线程 `apply_message` 里落到
  `App::set_search` + `Page::Search`，`SearchState::Error` 由页面的"重试"回调
  （`on_search_retry`）再发一次同样的查询。

网格几何只有一份：`ui/poster_grid.slint`（媒体库与搜索页共用 6 列 / `content-gutter` /
`card-gap` / 5:7 卡 / `CardZoom` 覆盖层）。改列数或卡宽只改这一个文件。

## 可交互目标清单（`Target`）：可点元素的事实来源

界面上"用户能点的东西"有一份**显式清单**，定义在 `ui/models.slint`：

```slint
export struct Target {
    id: string;        // "library.sort" / "home.row.continue.card.2"
    kind: string;      // button | toggle | input | card | tab | slider | scroll
    label: string;     // 界面上看得见的文字（报告/无障碍用）
    x: length; y: length;   // 窗口逻辑像素（来自元素的 absolute-position，直接可点）
    width: length; height: length;
}
```

这不是"调试彩蛋"，而是**新增控件的登记义务**：

- 每个页面组件（`HomePage` / `LibraryPage` / `SearchPage` / `DetailPage` /
  `PlayerShellPage` / `Navbar` / `EmptyState` / `Banner` / `AddServerDialog`）都有一个
  `out property <[Target]> targets`，用数组字面量声明自己**直接摆放且常驻**的可点控件；
- `AppWindow.targets` 是"当前页面的那份清单"（非当前页为空）；
- **重复元素**（卡片 / 库标签 / 版本行 / 音轨字幕行）和**条件子树**（hero 按钮 / 空态 /
  横幅动作 / 对话框 / 菜单）没法写进父组件的数组（Slint 1.19 的数组属性没有推导、没有拼接），
  它们在 `TargetBus.gen` 变化时调用 `TargetBus.register(...)` 自报坐标；
- 这两路在 `nova-ui` 的 `App::targets()` 里合并、按当前页命名空间过滤、去掉被窗口裁掉的项。

**登记依赖实例化**：Slint 1.19 的 `for` 项是**惰性创建**的——只有 item tree 被遍历
（渲染 / 命中测试 / `ensure_instantiated`）才会 new 出实例，而推进 `TargetBus.gen`
只让**已存在**的元素重新求值。所以 `App::refresh_targets()` 先调
`WindowInner::ensure_tree_instantiated()` 把重复元素实例化，再推进代次；
否则“刚切过去、还没渲染过一帧的页面”里一张卡都不存在，清单会是空的
（调试通道按 id 点击报 `unknown target`，而窗口一旦出帧/收到指针事件又“自己好了”）。
旧版 `PosterCard` 还额外用 `width > 0 && height > 0` 门控登记，那层重复过滤会在
“实例化完成但布局还没算”的那一拍把合法卡片挡掉，已删除（零尺寸/窗口外的项由
`TargetInfo::is_clickable()` / `center_in_window()` 统一过滤）。

为什么用 `absolute-position` 而不是手写偏移：坐标永远跟着布局/滚动走，改布局不会让清单失准。

**硬性约定**（由测试强制）：

1. 新增一个 `TouchArea` 必须要么登记成 `Target`（在所属组件的 `targets` 数组或登记回调里），
   要么在 `crates/nova-ui/tests/interaction_targets.rs` 的 `TOUCH_AREAS` 表里写明
   **为什么它不是可枚举控件**（例如"面板的事件拦截层，没有行为"）。
2. `id` 必须带页面命名空间前缀（`home.` / `library.` / `search.` / `detail.` / `player.`），
   浮层用 `banner.` / `dialog.`；同一窗口内唯一。
3. `kind` 只能是 `button` / `toggle` / `input` / `card` / `tab` / `slider` / `scroll`。
4. 目标必须是"真的能点到"的矩形：宽高为正、中心点在窗口内。

新增控件的检查清单：

- [ ] 控件有稳定的 `id`（列表项带下标或条目 id）；
- [ ] `label` 是界面上真的看得见的文字（不是内部字段名）；
- [ ] `--debug-socket` 的 `targets` 能看到它，`click` 能点出它该有的效果；
- [ ] `cargo test -p nova-ui --test interaction_targets` 通过（会检查 `TouchArea` 覆盖率）；
- [ ] `cargo test -p nova-app --test interaction_sweep` 通过（新控件如果有行为，它必须让
  "状态摘要"发生变化；否则要把理由写进该测试的 `KNOWN_DEAD`）。

## 图片契约

界面**不下载图片**。它只做两件事：

1. 在视图模型里声明"我需要这些图片"：`ImageKey { item_id, kind }`（`Poster`/`Backdrop`/`Logo`）。
2. 提供 `set_image(&ImageKey, slint::Image)` 供 app 推入。

没推入时界面显示**占位**（纯色 + 首字母/图标），推入失败时显示**退化态**（与占位一样但不重试）。
缓存键必须含服务端的版本 tag（`ImageSet::poster_tag`），否则换图后永远命中旧图。

## 键盘 / 遥控器导航

所有页面都必须能纯键盘 / 遥控器走完（不依赖鼠标悬停）。焦点分三层，内容层的扁平
焦点不变，上面叠两层「chrome」：

```text
  顶栏（navbar）   ← / → 在「标签 0..N-1 → 搜索按钮」之间移动，Enter 激活，↓ 回下一层
      ↑↓
  hero（hero）     首页 billboard 的「▶ 播放」/「更多信息」，← / → 切换，Enter 激活
      ↑↓
  内容（content）  卡片 / 网格（原来的扁平焦点）
```

| 键 | 行为 |
|---|---|
| `←` `→` | 行内移动焦点（首页是横向行，媒体库是网格）；在 hero 层是播放 / 更多信息；在顶栏是标签 ↔ 搜索按钮 |
| `↑` | 行/网格之间上移；**内容第一行**再往上：首页先进 hero 按钮行（hero 有简介时），再进顶栏；媒体库 / 搜索页首行直接进顶栏 |
| `↓` | 行/网格之间下移；顶栏 → 首页先回 hero、其余页回内容层；hero → 内容层 |
| `Enter` / `Space` | 打开详情 / 开始播放；顶栏上激活焦点标签（切库）或展开搜索框并聚焦输入框；hero 上播放 / 打开详情 |
| `Esc` / `Backspace` | 在 chrome 层（顶栏 / hero）**只回内容层**，不返回上一页；在内容层返回上一级（栈空时无动作） |
| `Ctrl+F` | 聚焦搜索框（首页/媒体库是顶栏胶囊，搜索页是页内大搜索框） |
| 搜索页 `Enter` | 提交并把焦点还给全局键盘（方向键在结果网格里移动） |
| 搜索页 `Esc` | 清空查询并回首页 |

详情页没有顶栏（页内就是 hero + 按钮行），它的按钮与版本行本来就在内容层的扁平焦点
里，`↑` 仍是行间上移。播放页有自己的快捷键表，见 `docs/debugging.md`。

`Tab` **不参与**页面导航：那是 Slint 内建的表单遍历，会和输入框 / 顶栏搜索框抢焦点。

焦点必须有**可见焦点环**（不是只靠颜色变化），并且焦点环与选中态要区分开：

- 焦点环统一用 `crates/nova-ui/ui/focus_ring.slint`（`Theme.focus-width` 的 `accent` 描边）；
- 「当前已选标签」只有字色（`Theme.text`）与字重（semibold）变化，**没有**描边；
- 焦点环只在键盘导航中显示：`AppWindow.focus-layer != 0`（顶栏 / hero 层）才画，
  鼠标点击不会进入这两层，所以不会留下一个「假焦点」；
- 焦点离开内容层时，内容网格 / 行**不再放大**焦点卡（避免两处焦点指示同时亮着）；
- 窄窗口下顶栏标签行是横向 Flickable：焦点移到被截断的标签上时，那个标签会被
  `ensure-visible` 横向滚入标签区。

调试通道（`--debug-socket` 的 `view`）里的 `focus_layer`（`content` / `hero` / `navbar`）
与 `nav_focus`（`0..标签数-1` = 库标签，`标签数` = 搜索按钮，`-1` = 不在顶栏）就是
这条链路的真机探针。

## 视觉规范

深色、克制、以内容为主。不用渐变堆装饰，不用霓虹发光。

**颜色**

| 名称 | 值 | 用途 |
|---|---|---|
| `bg` | `#0A0B0D` | 窗口底色 |
| `surface` | `#14161A` | 卡片、面板 |
| `surface-raised` | `#1C1F25` | 悬停/选中项 |
| `hairline` | `#2A2E36` | 1px 分隔线 |
| `text` | `#F2F4F7` | 主文字 |
| `text-muted` | `#9AA3AF` | 次要信息 |
| `text-faint` | `#6B7280` | 占位、禁用 |
| `accent` | `#E9A03C` | 进度条、焦点环、主按钮 |
| `accent-hover` | `#F5B85C` | 悬停 |
| `danger` | `#E5484D` | 错误提示 |

**间距**：4 / 8 / 12 / 16 / 24 / 32 / 48 的 8 点栅格。页面左右留白 48，区块间距 32。

**字号/字重**：12 常规（元信息）/ 14 常规（正文）/ 16 中粗（卡片标题）/ 22 中粗（区块标题）/
34 粗（页面标题）/ 48 粗（hero 标题）。字体族：`"Inter", "Noto Sans CJK SC", "PingFang SC", "DejaVu Sans"`。

**圆角**：海报 6，面板 10，胶囊 999。**海报比例** 2:3，网格 6 列（窗口宽 < 1280 时 4 列）。

**动效**：只做 120–160ms 的 opacity/位移过渡，用于焦点移动与页面切换；不做弹簧、不做视差。

## 测试策略

1. **纯视图模型单测**：分组、排序、进度格式化、badge 判定、导航栈。
2. **无头快照测试**：用 Slint 的 testing backend（`slint::testing::init()`）渲染到内存，
   `window.take_snapshot()` 存 `target/snapshots/*.png`。这些 PNG 要能被人眼直接看，
   用来判断布局是否崩坏（不是像素级回归基线）。
3. **键盘导航测试**：testing backend 发键事件，断言焦点索引与路由变化。
4. **交互目标 / 交互扫描测试**：`interaction_targets.rs` 断言每个页面的目标清单完整
   （含 `TouchArea` 覆盖率），`interaction_sweep.rs` 点每个目标并检查"死按钮"清单
   （见 `docs/testing.md`）。

如果 testing backend 在本机跑不起来（缺字体、缺后端），必须**如实报告**并退化为"启动真实窗口 + 截图"，
不允许把"没验证"写成"通过"。

## 阶段 3 的非目标

播放、设置、浅色主题、图片预取策略、多语言、动画系统、列表虚拟化。

---

# 播放器交互规范（阶段 4）

对标 Infuse：起播要快、控制层要退到后面、切换音轨/字幕要即时且不打断播放。

## 状态

```
Idle → Opening（黑底 + 标题 + 转圈，超过 400ms 才显示转圈，避免闪一下）
     → Playing ⇄ Paused
     → Buffering（保留画面最后一帧 + 角落提示，不要黑屏）
     → Ended（回到详情页，并显示"已看完"/"继续观看"已更新）
     → Failed（居中错误卡：可读原因 + 重试按钮）
```

**不许出现**：黑闪（opening 时就铺底色）、控制层盖住起播（前 1.2 秒不显示控件）、
加载中转圈闪烁（<400ms 不显示）。

## 控制层

自动隐藏：3 秒无输入隐藏（鼠标不动也算），任何键/移动立即显示。隐藏时只留光标隐藏，画面不动。

布局（自下而上）：

| 区域 | 内容 |
|---|---|
| 底部（全宽，官方两层渐变：`rgba(0,0,0,.7)`×`.75` + `rgba(0,0,0,.3)`） | 第一行：标题 + 副标题（年份 / 类型）；第二行：进度条；第三行（官方底栏分组）：**左组** 播放/暂停、音量、-15s、+15s —— 弹性间距 —— **右组** 字幕、音轨、倍速、全屏 —— 最右：剩余时间 `-2:05:26` |
| 顶部（全宽，`rgba(0,0,0,.651)` → 透明，高 11.11em） | 左上返回箭头（Esc 同义） |
| 中央 | 暂停时显示大播放图标（半透明，不挡字幕） |

进度条（官方 `.nfp .scrubber-container`）：已播 `#E50914` / 已缓冲 `hsla(0,0%,100%,.2)` /
轨道 `#5b5b5b`；轨道 8px，hover / 拖动变 16px；红色圆把手 36.8px 常驻、hover / 拖动放大 1.2×，
过渡 200ms `ease`；可点区高 3.3em（52.8px）。拖动时时间气泡跟随，松手才真正 seek，拖动期间画面不动。
尺寸体系见 `docs/ui-netflix-1to1.md` §11（`Theme.player-unit × 官方 em`，基数 16px 由我们选定）。

画面上**只有**播放需要的东西：返回、标题、进度条、剩余时间、播放/暂停、±15s、音量、字幕、音轨、
倍速、全屏、缓冲角标、错误卡、菜单弹层。引擎诊断（直连/转码、hwdec、1080p60）**不上屏**——
数据仍在 `PlayerData`（`transport-mode` / `engine-line` / `hwdec-notice`），用 `--debug-socket`
的 `view` 命令读（§11.7 有逐元素对照）。

**控件语言统一**（官方底栏）：控制行里所有按钮都是**透明底、无描边、白色图标/文字**，
hover 只有 `rgba(255,255,255,.1)` 的极淡高亮（`Theme.player-control-hover`），
只有"菜单已打开"的按钮才出现 3px 白边框（官方 `:focus` 环）。
hero 上那种深色圆钮（`rgba(22,22,22,.7)` + 描边）**不用于播放器控制条**。

**音量**：喇叭图标（点击 = 静音 / 取消），鼠标悬停时向右展开一段水平滑杆
（轨道 `#5b5b5b`、已填充 `#e50914`、红色圆手柄 `1em`、hover ×1.2，与进度条同一套 token），
按下 / 拖动直接设到百分比（`volume-set` 回调）。滑杆总宽 6em 是我们自定的（官方没给总宽，
且官方 CSS 里那是个垂直 popup）。短时音量数字已从界面移除（官方没有数字），数据仍留在 `PlayerData`。

## 快捷键（客厅遥控与键盘都要能操作）

| 键 | 行为 |
|---|---|
| `Space` / `K` | 播放 / 暂停 |
| `←` / `→` | -15s / +15s（与 Infuse 一致） |
| `Shift+←` / `Shift+→` | -60s / +60s |
| `↑` / `↓` | 音量 ±5%（控制层**隐藏时**第一下只唤醒、不改音量：这两个键同时是唤醒键） |
| `M` | 静音 |
| `A` | 音轨菜单 |
| `S` | 字幕菜单（含"关闭"与延迟 ±0.1s） |
| `F` | 全屏切换 |
| `Esc` | 菜单展开时先收菜单（再按一次才退出）；否则退出播放（回到详情页），**退出前必须提交一次进度** |

播放页没有 `Ctrl+D`：引擎诊断行已从画面移除（不留"按了没反应"的键），要看诊断走
`--debug-socket` 的 `view`。

## 音轨 / 字幕菜单

- 列表来自引擎真实枚举，不是猜的；当前项打勾。
- 选择后**不重启播放**（引擎侧切轨），并立刻写回用户偏好（下次自动选同一条）。
- 字幕菜单额外提供：关闭、延迟 -0.5s/+0.5s、外挂文件（本地路径）；这三类动作行字号更小（12px）、
  行高更矮、与轨道列表之间有一条分隔线，层级低于轨道列表。
- 图形字幕（PGS）标明"引擎渲染，样式不可改"，避免用户以为能调字体。

## 进度与"继续观看"

- 上报节奏：播放中每 10 秒、暂停/切轨/退出时立即、seek 后立即。
- 上报失败不能阻塞播放：进队列，退避重试，退出时若仍失败要本地落盘并在下次启动补发。
- "看完"用剩余时长阈值（`UserPreferences::finished_threshold`），不用百分比。
- 从详情页进来时，若 `resume_position > resume_min_position` 且未看完，按钮文案为"从 12:34 继续"，
  并默认从该位置起播。

## 缓冲与错误

- 缓冲中：画面保留、角落显示"缓冲中…"、控件不自动隐藏。
- 连续 15 秒无进度且引擎无新帧 → 判定卡死，显示错误卡并提供"重试"（重新 open，不清进度）。
- 网络错误文案要可行动（"服务器不可达，检查网络或服务器状态"），不要只打印 `reqwest` 原始错误。

## HDR（现实约束，写清楚免得返工）

Slint 的 `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture` 只接受 8bit `GL_RGBA` 纹理，
所以 **HDR 片源必须在 mpv 侧色调映射到 SDR 8bit**。界面文案只能写"支持 HDR 片源的色调映射"，
不能写"HDR 直通"。详见 `docs/research/player.md`。

## 性能预算（会被实测）

| 指标 | 目标 |
|---|---|
| 起播到首帧 | < 1.5s（局域网直连、已认证） |
| seek 到画面更新 | < 400ms（关键帧对齐） |
| 1080p60 播放主线程占用 | < 15%（零拷贝路径） |
| 控制层显示/隐藏 | 不引起掉帧 |
