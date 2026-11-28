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
    pub playable: bool,             // 条目自身能否直接播（首页 hero 用）
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

**换实例 = 替换栈顶**：已经停在媒体库时再打开另一个库（顶栏标签、调试通道 `tab`）
走 `Navigator::replace_top`（替换栈顶，不新增一层）——否则 `current_route()` 会永远
停在第一个进的库上（体检 R8-A2：页面 / 高亮 / 标题都已经切对，`view.route` 却是旧库），
而返回仍然只回一层。`Route::Search` 换查询词、播放页下方挂着另一条的详情也是同一
做法（统一用 `replace_top`，不再手写 `pop` + `push`）。

从**详情页**触发顶栏标签（调试通道 `tab`，以及以后详情页上的顶栏）是**压栈**：
新库压在详情页上，返回先回详情页。点的是当前详情所属的库也一样——栈里会短暂出现
两份相同的 `Route::Library`，这是保留“返回 = 回到来时的页面”的代价（去重会让返回
直接跳过详情页），页面 / 高亮 / 探针只看栈顶，不受影响。

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

键位（与下文「键盘 / 遥控器导航」的键位表同一条实现）：输入框里 `↓` 把焦点交给结果
网格（`focus_search_grid`；`.slint` 侧识别到方向键后先 `clear-focus()` +
`search-blur()`，再把意图回调上来）。焦点落点与首页「保持列」同口径：优先回到**上次
离开网格时那张卡**（`State::search_return` 记住 `(查询词, item id)`，按 id 在新结果里
找回位置；结果变少 / 重排不会跳到另一张卡上，找不到就回第 0 张）；查询词变了就回
第 0 张；从结果卡**进详情再返回**时也按 item id 回到进详情前那张卡（Netflix 同款，
详情页期间异步结果重排也照样找回；点卡走 `open_item`、不经过键盘焦点，进详情时按
item id 记下）。只有**真的离开搜索**（Esc 回首页 / 切换标签进媒体库）才清空记录，
下次进来从第 0 张开始；空网格首行 `↑` 取不到卡时写入 `None`，不留旧记录。
**结果为空时 `↓` 不移动焦点**——留在输入框，不额外反馈
（Netflix 同款行为，`search_page.rs` 有回归用例钉住）。网格**首行**再按 `↑` 回到页内输入框
（`move_up` 的 `Page::Search` 分支，`focus_search_input`）并记下离开时的位置；输入框里
`↑` 进顶栏（`focus_search_navbar`，标签仍键盘可达）；`Enter` 提交，`Esc` 清空查询并回首页。

**输入法组字（已知限制，推测 / 未验证）**：组字期间 `↑` / `↓` 应该属于输入法
（候选上下移），`search.slint` 在 `TextInput.key-pressed` 里看预编辑状态
`TextInput.preedit-text`：非空就 `reject`、不交接焦点。这个属性是 `i-slint-compiler`
的 `builtin_elements.rs` 里**内置声明**的（`out property <string> preedit-text`，源码注释
写明 "Internal, undocumented property, only exposed for IME"）：语言层可见，但属于内部、
未文档化接口。升级 Slint 时如果它被改名 / 删掉，会在**编译期**报错（`search.slint` 直接
引用）；如果只是语义变了（例如不再反映组字态），则会**静默失效**。风险评级：中低。

另外加了兜底：方向键的判据多一道 `ime-rely-on-preedit` 开关（`search.slint` 的
`TextInput.key-pressed`）。因为输入法异常时 `preedit-text` 可能一直不清空，非空会一直
保留，↑ / ↓ 会被永久拒掉；所以文本真的被编辑（`edited`，含提交）/ `Enter` 提交 / `Esc`
之后置假，不再拿 `preedit-text` 拦方向键。恢复信任有两条路，缺一不可：
- `search.slint` 把 `preedit-text` 绑到一个本地只读镜像（`preedit-mirror`），`changed`
  时置真。这是「提交一段后紧接着组下一段」唯一一定会经过的点：那段焦点一直在框里，
  新输入的字母被 `TextInput` 吃掉、不经过页面的 `key-pressed`，等下一段组字开始时
  preedit 已经非空，靠按键里的判断走不到（修复前的缺陷：下一段组字里按 `↓` 焦点会进
  网格，正在组的字丢掉）；
- `focus-input()` 在 `has-focus` 提前返回**之前**置真：重新聚焦是新的一段输入。
失焦后输入框不再是按键目标，箭头本来就拦不到（Slint 1.19 的 `TextInput` 在
`FocusOut` 时也会自己把 `preedit-text` 清空）。这条同样只做了无头模拟
（`UpdateComposition` 事件 + `search_page.rs` 的回归用例）验证；真实输入法
（ibus / fcitx）在 X11 / Wayland 下是否把方向键交给应用、`preedit-text` 是否在同帧
可读，**未做真机验证**。

网格几何只有一份：`ui/poster_grid.slint`（媒体库与搜索页共用 6 列 / `content-gutter` /
`card-gap` / 5:7 卡 / `CardZoom` 覆盖层）。改列数或卡宽只改这一个文件。

## 添加服务器对话框：本地校验先于联网

`AddServerDialog`（`ui/components/add_server.slint`）只负责摆放与焦点，校验在
`viewmodel/add_server.rs`；提交链路是 `.slint` 回调 → `nova-ui` 的
`on_add_server_submit`（门控 + 本地校验）→ `nova-app` 的 `Shell::submit_add_server`
（后台连接）。两层门都必须过：

- **空地址 / 地址不合规**：`AddServerView::can_submit()` 为假，`.slint` 的
  `submit-disabled` 同时置位——「连接」是禁用态（`enabled: false`），**不进
  `targets` 清单**（`sweep` 因此不会把点不动的按钮记成 dead），Enter 也会被
  `can_submit()` 拦下；
- **用户名为空**（体检 R8-C4）：「连接」仍可点，但 `on_add_server_submit` 在本地
  就返回，**不发任何网络请求**，错误区给出「请填写用户名」
  （`AddServerErrorKind::MissingUser`）。以前空用户名会真的发一次登录请求，
  服务端回 401，用户只看到「用户名或密码不对」。
  这条提示是**实时**的：用户名一变且**不是空的 / 全空格**（`add-server-user-edited`）
  就清掉自己那条 `MissingUser`，不用再点一次「连接」；把名字删空或只剩空格时提示
  必须留着（否则看起来像本地校验已经通过）；认证 / 网络错误不随输入消失（它们描述
  的是上一次连接的结果）。
- **密码可以为空**：Emby / Jellyfin 允许空密码账号，`can_submit()` 不看密码，
  空密码照常提交（回归测试 `empty_password_still_submits`）。
- 地址非法时另有一条内联红字（`address-hint`，在地址框下方），与错误区是两种展示：
  "输入还没填好"走内联提示，"连接失败 / 本地校验"走错误区。

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
  它们在 `TargetBus.gen` 变化时调用 `TargetBus.register(...)` 自报坐标。详情页的「播放」
  按钮与「播放版本」入口也都是条件子树（按条目类型 / 片源有无实例化），同样走
  `TargetBus.register`；首页 hero 的「▶ 播放」同理（主推是剧集时整个不实例化、不登记），
  它的登记只能写在那个条件子树里（父作用域看不到未实例化的元素名）。这类目标没有
  “一直登记着”的态：条件刚成立时也只有 `TargetBus.gen` 推进、`changed target-gen`
  重跑一遍才补上登记（即 `App::refresh_targets()` 的那一代）；
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
                  （主推是剧集时没有播放入口，只剩「更多信息」）
      ↑↓
  内容（content）  卡片 / 网格（原来的扁平焦点）
```

| 键 | 行为 |
|---|---|
| `←` `→` | 行内移动焦点（首页是横向行，媒体库是网格）；在 hero 层是播放 / 更多信息（不能播时只有更多信息，键无效）；在顶栏是标签 ↔ 搜索按钮 |
| `↑` | 行/网格之间上移；**内容第一行**再往上：首页先进 hero 按钮行（hero 有简介时），再进顶栏；媒体库首行直接进顶栏；搜索页的结果网格**首行**先进页内搜索框，输入框里再按一次才进顶栏 |
| `↓` | 行/网格之间下移；顶栏 → 首页先回 hero、其余页回内容层；hero → 内容层 |
| `Enter` / `Space` | 打开详情 / 开始播放；顶栏上激活焦点标签（切库）或展开搜索框并聚焦输入框；hero 上播放 / 打开详情（主推不能播时永远是打开详情）；播放页上是播放 / 暂停，**播完后是从头重播**、**错误卡上是「重试」**（见「起播与页面导航」） |
| `Esc` / `Backspace` | 在 chrome 层（顶栏 / hero）**只回内容层**，不返回上一页；在内容层返回上一级（栈空时无动作） |
| `Ctrl+F` | 聚焦搜索框（首页/媒体库是顶栏胶囊，搜索页是页内大搜索框） |
| 搜索页 `Enter` | 提交并把焦点还给全局键盘（方向键在结果网格里移动） |
| 搜索页 `↓`（输入框里） | 有结果时把焦点交给**结果网格**（`R7-D1`：输入框不能把方向键吞掉）；优先回到上次离开网格时那张卡，从详情返回也回到进详情前那张卡（按 item id），查询词变了回第 0 张；**结果为空时留在输入框**（无操作，不额外反馈） |
| 搜索页 `↑`（输入框里） | 进顶栏（标签仍键盘可达；没结果时也一样） |
| 搜索页 `↑` / `↓`（输入法组字中） | 交给输入法（候选上下移），不交接焦点：`.slint` 看 `TextInput.preedit-text` + `ime-rely-on-preedit`（编辑 / 提交 / `Esc` 后置假；`preedit-mirror` 变化或重新聚焦输入框后恢复信任，覆盖“提交一段后接着组下一段”）；**推测 / 未做真机输入法验证** |
| 搜索页 `Esc` | 清空查询并回首页 |

详情页没有顶栏（页内就是 hero + 按钮行），它的按钮与版本行本来就在内容层的扁平焦点
里，`↑` 仍是行间上移。**剧集详情没有播放按钮**（见「详情页」一节）：`focus_rows` 只剩
「我的列表」一行，焦点 `0` 就是它，`←` / `→` / `↑` / `↓` 不动焦点，`Enter` 激活
「我的列表」。首页 hero 的槽位数同样按条目类型缩：主推是剧集时没有「▶ 播放」，
`hero_slots` 返回 `1`，「更多信息」就是槽位 `0`（`←` / `→` 不动，`Enter` 打开详情，
调试通道直接派发 `hero-play` 也会被 Rust 侧门控掉）。播放页有自己的快捷键表，见 `docs/debugging.md`。

hero 的槽位在**数据刷新**时（`App::set_home`，与悬停规则同一入口）也跟着条目类型走：

- 可播 → 不可播：焦点从「▶ 播放」退回「更多信息」（槽位 `0`）；
- 不可播 → 可播：「更多信息」从槽位 `0` 挪到 `1`，停在它上面的焦点也跟到 `1`；
  留在 `0` 会让环一声不响地跳到「▶ 播放」上，那一刻按 `Enter` 就是意外起播；
- **同一份视图重贴**（每批海报到达时 `apply_images` 走的就是这条路）不动 `hero_focus`，
  只重算内容层的悬停（见「首页焦点模型」）；
- hero 消失（没有 hero 或简介为空）而焦点层还停在 hero 时，**退回内容层**：
  `home.slint` 整个不实例化按钮行，留着就是一个画不出焦点环、`Enter` 也没反馈的死层
  （`move_up` / 顶栏 `↓` 里的 `hero_available` 只挡「进入」，救不了已经在里面的）。

`Tab` **不参与**页面导航：那是 Slint 内建的表单遍历，会和输入框 / 顶栏搜索框抢焦点。

焦点必须有**可见焦点环**（不是只靠颜色变化），并且焦点环与选中态要区分开：

- 焦点环统一用 `crates/nova-ui/ui/focus_ring.slint`（`Theme.focus-width` 的 `accent` 描边）；
- 「当前已选标签」只有字色（`Theme.text`）与字重（semibold）变化，**没有**描边；
- 焦点环只在键盘导航中显示：`AppWindow.focus-layer != 0`（顶栏 / hero 层）才画，
  鼠标点击不会进入这两层，所以不会留下一个「假焦点」；
- 焦点离开内容层时，内容网格 / 行**不再放大**焦点卡（避免两处焦点指示同时亮着）；
- 窄窗口下顶栏标签行是横向 Flickable：焦点移到被截断的标签上时，焦点**环**会被
  `ensure-visible` 横向滚入标签区。标签行（`tabs-row`）左右各留 `Theme.focus-offset`
  的内边距，这段余量算进标签区的 `content-width`——否则滚到两端时第一个 / 最后一个
  标签的环描边会被标签区的 `clip` 裁掉。所以“焦点环完整可见”的判定是
  `focus_tab_left - focus-offset >= 0 && focus_tab_right + focus-offset <=
  tabs_viewport_w`（`focus-offset` / `focus-width` 都从 `Theme` 取）；800px 下滚到底时
  标签本体右沿是 `tabs_viewport_w - focus-offset`，环的外沿才贴住视口右沿；
- 搜索页的顶栏**不画**胶囊搜索框（页内已有大搜索框），那个控件也就不能占标签区的
  宽度（`R7-C1`：以前 `visible: false` 照样占位，800px 下标签区从 587 掉到 383）。
  标签区的宽度与首页同一套算法，两页的标签行 / 最大滚动量也完全一致（`R8-A1`
  曾以为是搜索页少算了一段余量，实际是 `nav_geometry` 探针把滚动偏移算了两遍）；

调试通道（`--debug-socket` 的 `view`）里的 `focus_layer`（`content` / `hero` / `navbar`）
与 `nav_focus`（`0..标签数-1` = 库标签，`标签数` = 搜索按钮，`-1` = 不在顶栏）就是
这条链路的真机探针。

### 首页焦点模型

首页的焦点按**视觉行**组织：继续观看 / 最近添加 / 每个库行各是一行，`↓` / `↑` 在行间移动、
保留最接近的列；滚动只由键盘导航触发，悬停本身不滚动。

**空行不占位**：`items` 为空的库行在 `views::home_view`（`crates/nova-app/src/views.rs`）里就被
整行过滤掉了，界面根本不会拿到它；「继续观看」「最近添加」为空时 `CardRow.height` 是
`0px`（`ui/home.slint`），连自己的行间距一起消失。

卡片实例不重建，悬停由 `App::set_home` 的三条规则维持（`convert::home_data` 复用已有的
model，库行按 `library_id` 匹配，所以行内 `PosterCard` 实例不会被重建、`has-hover` 不会被清零）：

- **同一份视图重贴**（每批海报到达时 `home::apply` 走的就是这条路）：不清悬停，指针底下的
  放大保持不动；
- **内容变了、行几何不变**：推进 `home-hover-reset`，各行按每张卡**当前**的 `has-hover`
  按指针位置重算——条数不变、内容整体替换时，同一个下标已经不是指针下面那张卡了；
- **行几何变了**（`home_row_geometry_changed`：hero 有无、继续观看 / 最近添加的条数、库行的
  数量或 `library_id` 顺序）：**硬清**悬停锁存（`home-hover-clear`），不重算。行会整体上下
  挪位，而 `has-hover` 只在指针事件里更新，指针停着不动时它还是旧位置的悬停；

焦点重定位按 `item_id`（`rebase_home_focus`）：条目序列不变（同一份视图重贴）就保留原下标；
序列变了则在所有同 `item_id` 的候选里取离旧下标最近的那个；找不到则退回旧下标并夹进新范围。
焦点**越界**（数据变少）时钳回第 1 张，并把首页滚回顶部（否则 Enter 打开的是屏幕外那张）。

**行左右箭头**（`CardRow.arrow-left-on` / `arrow-right-on`）只在"本行还能往那个方向滚"
且"行被点亮"时可见，与 Netflix 一致：行在横向起点不显示左箭头，到行尾不显示右箭头，
内容装得下时两个都不显示。可见性同时是登记门控——不可见的箭头**不进 `targets`**，`sweep`
也就不会点到"点了没反应"的空箭头。媒体库 / 搜索 / 详情是网格（`poster_grid.slint`），
没有这一对箭头。

"被点亮"（`CardRow.zoom-index >= 0`）有三种来源，**不只键盘**：

- **鼠标悬停卡片**（`hot`）：首屏没按过方向键（`nav-active` 仍为 false）也照样点亮，
  悬停不会抢走键盘焦点，也不会滚动行；
- **悬停在箭头上**（`arrow-left-area` / `arrow-right-area` 的 `has-hover`）：指针从卡片移
  到箭头上时放大层不灭，箭头也跟着保持点亮；
- **键盘焦点落在这一行**（`nav-active && content-focused` 时取 `focused-index`）：方向键
  进来就点亮，并以 `scroll-to-card` 把焦点卡滚进视口。

`zoom-index` 的优先级顺序是 `hot` → （指针在放大层 / 箭头上时）锁存的 `latch` → 键盘焦点，
所以鼠标与键盘同时存在时悬停优先。

## 详情页：没有播放入口的剧集，按片源实例化的版本区

**剧集详情目前没有播放入口（选集还没做）。** 片源挂在单集上，剧集本体没有任何可播
版本；真机实测（R9-A2：真实数据目录、条目 `18778`「X局密档」、`kind = series`、
`sources = []`）点「播放」只会压入一个 `Player` 路由，页面显示错误卡
「这个条目没有可播放的版本：确认服务端上文件还在」。所以：

- `DetailView.playable` 由生产方（`nova-app` 的 `detail_from_item`）按
  **条目类型**（`MediaKind::is_playable()`）填写：只有电影与单集是 `true`，剧集和
  季都是 `false`。**不能按「片源为空」判断**：
  电影离线时 `sources` 也可能为空，联网后能播，播放按钮要保留。
- 详情页的**加载占位**（`detail_placeholder`，“正在加载…”）显式 `playable = false`：
  占位不知道条目类型，用 `DetailView::default()` 的默认值会先闪一下可点的「播放」。
- `playable = false` 时 `ui/detail.slint` 里播放按钮**整个不实例化**（`if (root.data.playable)`），
  也不登记 `detail.play`。只写 `visible: false` 不够：不可见的 Slint 元素仍有宽高，仍会被
  命中测试判为可点，清单里也照样留着它。
- 焦点模型跟着缩成一行：`pages::detail::focus_rows` 返回 `vec![1]`（焦点 `0` = 「我的
  列表」，只有能播时才是 `1 + sources.len()`，`0` = 播放）。键盘 `Enter` 由
  `play_current` 门控到「我的列表」，不会复活一个界面上不存在的按钮。
- 电影与单集（`MediaKind::Movie` / `MediaKind::Episode`）不受影响：`sources` 为空时
  播放按钮照常在。

**首页 hero 的播放入口用同一个判据。** hero 取的是「继续观看 / 最近添加」的第一条，
可能是剧集：`CardView.playable` 由 `card_view` 按 `item.kind.is_playable()` 填写，
`HomeView::hero_playable()` 汇总给 `HomeData.hero-playable`。false 时 `home.slint` 里
「▶ 播放」整个不实例化、不登记 `home.hero.play`，hero 按钮行只剩「更多信息」；
`pages::app` 的 `hero_slots` 随之变成 `1`，焦点只落在「更多信息」上，`on_hero_play`
也做门控（调试通道的 `click` 绕不过去）。

**版本区只在有片源时显示。** 整段版本入口（`NETFLIX` 风格的下拉 + 展开面板）包在
`if (root.data.sources.length > 0)` 里：片源为空时既没有版本行，也没有那个只剩
「播放版本：  ▾」的空下拉（R9-A2 的原始症状）。`detail.drop` 与展开后的
`detail.source.N` 一样在条件子树里经 `TargetBus.register` 登记，入口消失时一起从
`targets` 里消失（调试通道 `targets` / `sweep` 都看不到它）。

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

**圆角**：海报 6，面板 10，胶囊 999。**海报比例** 2:3；结果网格固定 6 列（`poster_grid.slint` 的 `columns` 属性，媒体库与搜索页共用同一份几何，不随窗口宽度减少列数；窗口变窄只让列宽变窄）。

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

## 起播与页面导航

- 起播条目由**路由**决定：`Route::Player { item_id }` 指向哪个条目就播哪个。
- 在播放页**换片**会把当前路由栈顶替换为新片的 `Player`，返回时回到**新片**的详情页。
- 播放页的 `Enter` / `Space` / 控制层播放键都是**播放 / 暂停**。播完（`SessionState::Ended`，
  界面侧判定 `player_finished(&view)` = `PlayerView::ended && !view.playing`）之后，同一个键
  就是**从头重播**：走起播路径（`play_current` → `play` → `nova-app` 的 `Player::begin`），
  会话代号递增（同一部片重播也是**新会话**，见 `PlaybackProgress::session_seq`）、旧会话
  交出 `Exit`（服务端收到 `stop` 与「已看完」），起播位置钉在 **0**——不沿用服务端 / 缓存里
  那份常常还贴在片尾的续播点（体检 R8-B1 / R8-B2 真机上就是「按了播放键，画面还是最后一帧」）。
  重播期间照旧受 `Session::reportable_position` 保护：首帧之前一条进度都不发，所以起播 / 缓冲
  不会产生 `Progress@0` 把续播点清掉。
- 正常播放时按 `Enter` **不会**重开；同一部片重开只发生在两种“看起来在播放页、其实没在放”
  的状态上，且两个键的语义不同：**播完（`Ended`）时 `Enter` = 重播**（新会话、位置 0，见上），
  **出错（`Failed`）时 `Enter` = 重试**（重跑一遍起播，不强制把位置重置成 0）。`ended` 由
  `SessionState::Ended` 直传，不靠「位置 ≥ 时长」猜：时长未知时位置永远推不到头，续播点
  贴片尾时位置又会一开始就 ≥ 时长。
- 错误卡（`PlayerView::error` 非空）上的 `Enter` / `Space` / 控制层播放键 = 卡上的「重试」，
  与点击按钮同一条路径（`player_retry` → `Player::retry`）。起播（含取直链）失败时
  `request` / `pending` / `target` 已被清空，`retry` 靠最后一次起播目标的快照
  （`Player::last_start`：条目 + 版本）重跑一遍 `begin`（偏好传 `None`，与重播一样读
  数据库里的当前值）——没有快照时只能静默返回，表现就是“点了没反应”。
- 续播判定在 `nova_playback::resume::resume_position`（纯函数）：续播点为空、或
  `resume_position < resume_min_position`（太靠前）、或剩余时长不大于 `finished_threshold`
  （已看完）时从头播，否则从续播点起播。**`played` 不参与判定**：
  服务端会把看完的片重新标成未看完，也会给 `Played = true` 的条目保留中段的续播点。
  按钮文案同样读它的结果（`resume_label`：`"播放"` / `"从 12:34 继续"`）。
  命令行 `nova info` 的「观看」行也是同一个来源：有有效续播点就显示「看到 h:mm:ss」，
  否则才按 `played` 显示「已看完」/「未看」（偏好同样读本地库）。
- 音量与静音在**本次运行内**跨会话保留（退出播放页只复位标题/进度/时长/轨道/倍速）。

**播完之后（`Ended`）**：位置固定为时长、界面显示「已看完」，三个入口的行为与 Netflix 一致：

> 前提：引擎给 mpv 设了 `keep-open=yes` / `idle=yes`（`crates/nova-playback-mpv/src/options.rs`
> 的默认选项），播完停最后一帧而不是销毁媒体。用户用额外 mpv options（`NOVA_MPV_OPTIONS`）
> 把 `keep-open` 改成 `no` 时，播完后的拖动行为**不保证**：媒体已卸载，seek 可能没反应，
> 也不会再产生第二次 `Ended`。

1. 控制层播放键 / `Enter` / `Space` = 从头重播（新会话、位置 0，见上）。生产路径由
   `Player::replay`（走 `begin`：递增会话代号、重取直链）处理；`Session::play` /
   `toggle_pause` 的 `Ended` 分支（位置回 0、状态转回 `Playing`）只是状态机的**一致性**
   保障，app 路径到不了那里，不要把它当成起播路径改；
2. 拖动进度条、`←` / `→`（含 `Shift` 的 ±60s）= 跳到目标位置并**继续播放**：
   `Session::seek` 在 `Ended` 下也生效（位置立刻生效、状态转回 `Playing`），并按
   `ProgressTrigger::Seek` 上报一次进度。引擎这时还停在 EOF（mpv `keep-open`），
   所以宿主在 seek 之后要补一次 `play`，否则进度条动了、画面仍是最后一帧；
3. 同一个 session 继续播到片尾会**再报一次** `Ended`，不会卡在「播放中」：worker 的
   `Command::Seek` 分支**无条件**清掉「已报过 Ended」的标记，同时**置位**旧观察标记
   `stale_eof`（`worker::EndedGuard::after_seek`：`stale_eof |= reported`，**累积**而不是覆盖），
   不依赖 mpv 的具体时序（实测 mpv 0.42 + `keep-open=yes`：片尾**只有** `eof-reached=true`
   属性、没有 `end-file` 事件；seek 之后才送 `eof-reached=false` + `playback-restart`）。
   旧实现只靠 `eof-reached` 回到 `false` 来清，第二次播完就可能不再产生 `Ended`。
   同时 seek 还要把“seek 之前产生、seek 之后才被处理到”的 `eof-reached=true` 标成旧观察
   （`EndedGuard::stale_eof`）：主循环**先处理命令再处理事件**，不吞掉它就会在清标记之后
   立刻误报第二次 `Ended`——界面把状态钉成 `Ended`、位置钉到时长（显示「已看完」），
   引擎其实还在播，这时按 `Enter` 会从 0 重播，位置就丢了。旧观察在**确认 seek 已生效**、
   或者**这是一次新片尾**时才失效：`playback-restart` 时读到的 `eof-reached` 已经翻成
   `false`（`EndedGuard::on_playback_restart(Some(false))`，读不到属性按“还不知道”处理）、
   `eof-reached=false`、新的 `end-file`。`playback-restart` **本身**不再是无条件证据：
   `keep-open` 下它与 `eof-reached=false` 的先后由 mpv 决定，restart 之后仍可能排着一条
   seek 之前产生的 `eof-reached=true`，以前在这里无条件清 `stale_eof` 会把它当成新片尾
   误报第二次 `Ended`（理论风险，事件乱序）。
   用**累积**是因为片尾后可能连着拖两次：第二次拖时 `reported` 已经是 `false`，覆盖赋值会把
   第一次设下的“吞掉旧观察”清掉，队列里更早的 `eof-reached=true` 又被当成新片尾误报。
   已经停在 `Ended` 时重复收到的 `Ended` 也不再补报一条 `Pause` 进度（`Player::pump_events`）。
   这几条路径都有单测（`worker::tests::seek_after_end_*` / `stale_eof_*` /
   `two_seeks_after_end_*` / `restart_while_eof_is_still_reached_*` /
   `restart_after_eof_is_cleared_*` / `player::tests::duplicate_ended_does_not_report_again`）。

**已知限制（推测，未验证）**：播完后**直接拖到 100%（`duration`）** 时，`Session::seek`
把位置 clamp 到时长、状态转回 `Playing`，界面靠引擎再报一次 `Ended` 才会回到「已看完」。
但 mpv 这时可能只送 `playback-restart`，`eof-reached` 本来就是 `true`、seek 到时长也不会
变成 `false`，于是没有新的 `eof-reached` 变化，也就没有第二次 `Ended`——界面停在
「播放中」，位置在片尾。按上面的状态机推演是这个结果，但**没有证据**：没取过真机上
mpv 的 `eof-reached` 属性日志，也没在真机复现过。要确认（或否证）需要一份
`NOVA_MPV_LOG`/属性观察日志。

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

音量与静音属于用户偏好，**在同一次运行内跨播放会话保留**（退出播放页只复位标题 / 进度 /
时长 / 轨道 / 倍速），**重启后恢复默认**（未做持久化；要改的话得先决定写进
`UserPreferences` 还是本地状态）。

## 快捷键（客厅遥控与键盘都要能操作）

| 键 | 行为 |
|---|---|
| `Space` / `K` | 播放 / 暂停（播完后是**从头重播**，出错时是错误卡上的**重试**，`Enter` 同义） |
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
- 控制层可见时字幕上移让开按钮行（mpv `sub-pos`）。**已知限制（不处理）**：ASS 里用 `\pos` 等
  写死位置的排版/特效事件不受 `sub-pos` 影响，可能与控制层短暂重叠，控制层自动隐藏后即消失；
  强制 `sub-ass-override` 会抹掉这类字幕的样式与定位，代价不可接受。

## 进度与"继续观看"

- 会话身份是 `(会话代号 + 条目 + 资源)`：`PlaybackProgress::session_seq` 由 player 每次起播
  （`begin`）递增，所以同一部片**重播也是新会话**；`0` 表示"不知道"（旧版本落盘的待发文件、
  测试构造的进度），sink 不会为它补发 `Started`。
- 服务端的 `PlaySessionId` 绑在**会话**上，不是绑在条目上：`PlaybackInfo` 每取一次都会给一个
  新标识。player 在起播前那次刷新成功的那一刻就把它记进**本会话**
  （`Player::play_session_id`），`build_progress_at` 写进每条 `PlaybackProgress::play_session_id`，
  上报时原样带回。标识与资源出自**同一个响应**（`MediaProvider::sources_with_session` 返回
  `PlaybackSources`）：以前是「先 `sources()`、再查 `latest_play_session_id()`」，两次调用之间
  能插进一次详情页刷新，本会话就会拿到那次刷新的标识。既不能等到发送时按条目查“最近一次”
  （同一部片重播时，新会话的 `PlaybackInfo` 会在旧会话异步发出的 `Exit`/`stop` 之前就把条目上的
  记录覆盖掉），也不能在入队时去查：同一部片**播放期间**详情页刷新（`Shell::spawn_detail` →
  `provider.sources()`）同样会覆盖条目上的记录，入队时查到的是第三个标识，`Exit` 照样关错会话。
  `sink::Progress::stamp` 只给**没带**标识的进度兜底（旧版本落盘的待发项等）：这时退回
  "按条目查最近一次"（`MediaProvider::latest_play_session_id`）的老行为。
- 起播前那次刷新有 400ms 上限（`refresh_before_play`）：超时 / 失败时拿不到本会话的标识，
  这条会话的进度 `play_session_id` 就是 `None`，兜底查到的可能是**上一次会话**的旧标识——
  预期服务端会把这条上报当作无效会话丢弃（这次续播点不准；**推测，未在服务端验证**）。取直链（`stream_target`）只按
  已有资源拼地址、**不再取** `PlaybackInfo`，所以后续拿到直链也补不回标识；要补只能再取一次
  `PlaybackInfo`，而服务端会给一个**新**标识、多出一个会话，所以不补。
  这条降级路径有单测（`player::tests::startup_refresh_timeout_*`）。
- 上报节奏：播放中每 10 秒、暂停/切轨/退出时立即、seek 后立即。
- 上报失败不能阻塞播放：进队列，退避重试，退出时若仍失败要本地落盘并在下次启动补发。
- 重试队列是跨条目共用的，但压制只针对**同一个会话**（会话代号 + 条目 + 资源）：条目 A 的
  待发项在退避时不会吞掉条目 B 的上报，也不会吞掉同一部片**重播**时新会话的第一条上报
  （player 每次起播递增代号）；旧待发项保留自己原来的位置，不会被改写成新会话刚开始的位置。
  同一个（条目, 资源）的待发项只保留**最新会话**的一条：服务端只关心最终续播点，离线时
  反复重播同一部片不会把队列越攒越长。
- 换片时旧会话的最后进度（`Exit` + `stop`）交给后台 worker **异步**发送，界面线程不等；
  进程在这之前被强杀的话会丢掉这一次上报（正常退出不受影响）。
- 每个新会话的第一条上报都先发 `Started`（`Sessions/Playing`）：换片或重播后要重新发，
  否则服务端不会把新会话的进度算到条目上。`Started` 只对**从没见过的会话代号**发：已经
  `stop` 过的会话被队列补发时（比如重试旧的 `Exit`）绝不再发——即使用这个代号的老 `Started`
  还有重试次数没用完（sink 记一个"已结束代号"的水位，代号相等的也一并拦住）；否则服务端
  会把那个会话一直记成在播，而之后再也没人 `stop`。`Started` 遇到 transient 失败在本会话
  后续的上报里重试，最多 3 次（不阻塞播放）。
- 待发文件里的会话代号字段带默认值：旧版本写下的（没有代号）仍能读回并补发。**读回时一律
  清零代号**（上个进程的会话都已结束），所以补发只发 `Progress`、不发 `Started`——否则服务端
  会把已结束的会话记成在播，而之后再也没人 `stop`。服务端会话标识（`play_session_id`）相反是
  **保留**的：它标识"这条进度属于哪个会话"，带着已死会话的标识补发最多被服务端忽略；清掉就会
  退回"按条目查最近一次"，把旧位置记到一个还活着的新会话上。
- "看完"用剩余时长阈值（`UserPreferences::finished_threshold`），不用百分比。
- 从详情页进来时，按钮文案与起播位置都走 `resume::resume_position`：续播点
  `>= resume_min_position` 且剩余时长多于 `finished_threshold` 时，文案为"从 12:34 继续"
  并从该位置起播；否则从头播。`played` 不参与这个判定。

## 缓冲与错误

- 缓冲中：画面保留、角落显示"缓冲中…"、控件不自动隐藏。
- **起播的转圈一直显示到引擎真正开始推进为止**。`Session` 只在 `Opening` 里因首帧转
  `Playing`；如果首帧到时引擎仍是 `Buffering`（`core-idle` 为真），状态保持 `Buffering`，
  等引擎发 `Buffering(false)` 才回 `Playing` 并收转圈，不会露出一帧静止的预滚画面而
  位置不动。引擎假如一直不发 `Buffering(false)`，由下面的 15 秒卡死判定兜底。
- **深续播点在 WAN 上可能需要几秒预滚，这是正常现象**（mpv 在做 seek 与填缓存），
  不是 Nova 的 bug；这期间画面与位置都不动是预期的，只有转圈必须一直在。
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
