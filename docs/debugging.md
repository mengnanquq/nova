# 调试接口

界面是"点出来的"，所以诊断问题时最缺的是**驱动能力**。这里有三件工具，都不需要截屏协议、
不抢焦点、不碰用户屏幕。

调试通道除了"切路由 + 自截"，还能**枚举界面上所有可点控件、按名字点击/悬停/输入**，
并对整页做一次"交互扫描"找出"点了没反应"的按钮（真实事故：媒体库的「排序」按钮回调
注册了但实现是空的，任何测试都没发现）。

## 1. `--debug-socket <path>`：本地命令通道

```bash
nova-desktop --debug-socket /tmp/nova.sock
```

Unix socket，**行分隔 JSON**，一问一答。用 `socat`/`nc -U` 每次连一条命令最省心：

```bash
printf '{"cmd":"view"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```

| 命令 | 作用 |
|---|---|
| `{"cmd":"view"}` | 当前路由、连接状态、首页/库/详情摘要（含标签列表与数量） |
| `{"cmd":"open","item":"<id>"}` | 打开条目详情（走界面导航，会切页并拉数据） |
| `{"cmd":"route","value":"home"\|"library"}` | 切到首页 / 第一个媒体库 |
| `{"cmd":"route","value":"detail","item":"<id>"}` | 同上，直达详情 |
| `{"cmd":"tab","library":"<id>"}` | 切媒体库标签；`"library":"@home"` = 首页，空串 / 缺省也按首页处理（R6-12，合成「全部」库不再能从这条命令直达） |
| `{"cmd":"search","term":"..."}` | 用本地缓存过滤首页（旧行为，兼容保留） |
| `{"cmd":"search","term":"...","page":true}` | 进搜索页并把查询词填好（本地缓存立即出结果，同时打一次服务端搜索） |
| `{"cmd":"screenshot","path":"/tmp/x.png"}` | 窗口自截（见下），**异步**：等文件出现；无显示器 / 锁屏时约 200ms 后自动改走离屏渲染，不必等帧回调 |
| `{"cmd":"targets"}` | 当前页面的**可交互目标清单**（按钮 / 开关 / 输入框 / 卡片 / 标签 / 滑杆 / 滚动箭头） |
| `{"cmd":"click","target":"library.sort"}` | 按名字点一个目标（发 `PointerMoved` → `PointerPressed` → `PointerReleased`） |
| `{"cmd":"click","x":123,"y":45}` | 按窗口坐标点（验证"名字清单是否覆盖真实可点区域"） |
| `{"cmd":"hover","target":"home.row.continue.card.0"}` | 只发 `PointerMoved`（验证悬停才出现的东西：音量滑杆、卡片放大、行箭头） |
| `{"cmd":"scroll","x":400,"y":300,"dy":360}` | 在该坐标滚一下（先 `PointerMoved` 再 `PointerScrolled`），见下 |
| `{"cmd":"scroll","target":"library.grid","dy":360,"dx":0}` | 同上，但坐标取目标中心（`target` 与 `x`/`y` 二选一） |
| `{"cmd":"key","key":"Enter"}` | 发一次按键：`Enter` / `Escape` / `Left` / `Right` / `Up` / `Down` / `Space` / `Tab` / `Backspace` 或单个字符（含中文等非 ASCII 字符），直接送给**当前焦点**（不预点击） |
| `{"cmd":"type","text":"沙丘"}` | 逐字符输入：每个字符作为按键事件直接送给**当前焦点**，不预点击（无输入目标的页面也不会落空） |
| `{"cmd":"type","text":"沙丘","target":"search.input"}` | 同上，但先点一下 `target`（输入框不在当前焦点上时用；坐标按 `targets` 的最新登记） |
| `{"cmd":"wait","ms":400}` | 在 UI 线程上**不阻塞**地等一段时间（动画 / 异步加载结算）|
| `{"cmd":"sweep"}` | 对当前页每个目标"悬停 → 点击"，记录状态摘要是否变化 |
| `{"cmd":"dead"}` | `sweep` 的过滤版：只返回"点击后状态没有任何变化"的目标 |
| `{"cmd":"quit"}` | 退出 |

### 目标清单长什么样

```bash
printf '{"cmd":"targets"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock | head -c 600
```

```json
{"ok":true,"page":"library","count":37,"targets":[
  {"id":"library.card.0","kind":"card","label":"沙丘：第二部","x":200.0,"y":164.0,"w":232.0,"h":356.0},
  {"id":"library.nav.tab.1","kind":"tab","label":"电影","x":253.0,"y":0.0,"w":28.0,"h":68.0},
  {"id":"library.sort","kind":"button","label":"排序：最近添加  ▼","x":1016.0,"y":108.0,"w":144.0,"h":36.0}
]}
```

坐标是**窗口逻辑像素**，直接可点。`kind` 只会是
`button` / `toggle` / `input` / `card` / `tab` / `slider` / `scroll`。

按名字点击成功时回 `{"ok":true,"target":"library.sort","kind":"button","label":"…","x":…,"y":…,"page":"library"}`；
id 不存在时回 `{"ok":false,"error":"unknown target: library.sortt","available":[…前 20 个 id…]}`——
脚本可以直接从 `available` 里挑一个正确的。

### `scroll`：滚轮

```bash
printf '{"cmd":"scroll","x":400,"y":300,"dy":360}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```

- `dy` **必填**，`dx` 缺省 0；两者不能同时为 0（那样什么都没滚，只会得到假结论）；
- 单位是**逻辑像素**，符号跟 winit / Slint 原始语义（**正 = 向上滚，负 = 向下滚**，
  也就是 `content-y` 加上 `dy`）；实测：`dy:-360` 让媒体库 `scroll_y` 从 0 → -680.9，
  `dy:720` 又把它推回 0（顶）。一行滚轮 ≈ 60（winit 把 line delta 乘 60）；
- 位置参数与 `click` / `hover` 一样：`target` 或 `x`/`y`；
- 先发 `PointerMoved` 再发 `PointerScrolled`（真机的滚轮事件也带当前位置，悬停态与命中测试都要它）。

滚动到底/顶时 `content-y` 会被钳住，`scroll` 仍然回 `{"ok":true}`：判断"到底滚了没有"
要看 `view` 的数值（首页 `home_geometry.scroll_y`、媒体库 `library_geometry.scroll_y`、
搜索页 `search_geometry.scroll_y`），不要看 `ok`。真机实测（800×600）：首页 `dy:-420`
把 `home_geometry.scroll_y` 从 0 推到 -420；媒体库 `dy:-360` 推到 -680.9，`dy:720` 又钳回 0。
滚过一屏后原来在窗口外的卡片会进 `targets`，可以直接 `click` 验证。

### `view` 里的几何探针：焦点矩形是**视口相对**坐标

`view` 的 `library_geometry` / `search_geometry` / `home_geometry` 里的焦点矩形一律是
**视口相对**坐标（相对结果区 / 首页内容区的可见窗口左上角，体检 R3-2 统一口径）：

| 字段 | 含义 |
|---|---|
| `library_geometry.focus_top/focus_bottom` | 焦点卡上下沿（相对结果网格的视口） |
| `search_geometry.focus_top/focus_bottom` | 同上（搜索页结果网格） |
| `home_geometry.focus_top/focus_bottom` | 焦点卡上下沿（相对首页外层 Flickable 的视口） |
| `home_geometry.focus_left/focus_right` | 焦点卡左右沿（相对**焦点行自己的**横向 Flickable 视口） |

首页的这四个字段只覆盖**继续观看 / 最近添加**两行：库行是 `for` 重复元素，页面拿不到
它们的 `y` / `flick`（R7-2 之后库行也有键盘焦点了）。焦点不在前两行时
`home_geometry.focus_top/bottom/left/right` 与 `row_scroll_x` / `row_viewport_w`
**一律返回 `-1`**（与 `probe-focus-left` 同一口径，不能拿库行下标去点亮前两行的几何）。
焦点在库行时用 `targets` 里的 `home.row.lib.N.card.M` 矩形（窗口坐标）判断可见性。
首页布局还暴露 `hero-bottom`、`row1-title-bottom`、`cards-offset`、`row1-zoom-on` 等 Slint 几何/悬停探针（供布局与悬停测试使用）。

所以判断「焦点卡在视口内」**不需要**再拿 `scroll_y` 换算：

- 纵向：`0 ≤ focus_top` 且 `focus_bottom ≤ viewport_h`；
- 横向（首页行）：`0 ≤ focus_left` 且 `focus_right ≤ row_viewport_w`。

`scroll_y` / `row_scroll_x` 仍然报滚动偏移本身（向下 / 向右为负），只用来看"滚没滚、
滚了多少"。注意首页的「最近添加」是**一整条很长的横向行**（不是嵌套网格）：行内滚动
只体现在 `row_scroll_x` / `focus_left/right` 上，`scroll_y` 可以一直不变。以前这里报的是
内容坐标，行内滚出一屏后焦点矩形看起来仍在视口里，很容易误判成"不滚动"（R3-2）。

快速连按方向键时，键盘跟随（`scroll-to-index`）是**直接写** `content-y` / `content-x`，
没有平滑动画；但一次注入多个按键时渲染帧可能还没跟上（`targets` 里的坐标会短暂落后）。
`Enter` 打开的是**焦点所在的条目**（`view.focus` / `focus_ui` 指向的那张卡），不是
"屏幕上看起来"的那张——滚动只是视觉跟随，不影响按键语义。

`view` 里还有一组悬停探针：`home_geometry.row0_zoom_on`（继续观看行的放大覆盖层是否
打开），以及 `library_geometry` / `search_geometry` 里的 `zoom_on`。悬停卡片后从
`false` 变 `true`、指针移开又变回 `false`，这就是“`hover` 真的命中了吗”的直接证据。
网格与首页行用同一条放大规则：**悬停优先，其次键盘焦点**（键盘焦点只在按过方向键、
`nav-active` 为真时才放大，避免一打开页面第一张卡就是放大状态）。

悬停还会把**键盘焦点对齐到鼠标下的那张卡**：`hover` 之后的方向键从这张卡继续。
这条规则在放大覆盖层（`CardZoom`，接管命中区）上同样生效——包括“指针本来就停在
这张卡上、同一坐标再 `hover` 一次”（体检 R2-3：以前只有 `has-hover` 的边沿才同步，
重复悬停是空操作，`Right` 就从键盘旧位置 +1）。判断焦点看 `view.focus_ui`（即时的
UI 属性）；`view.focus` 是 Rust 侧镜像，只在键盘移动 / 确认时更新，悬停后可能是旧值。

- **清单是两路合并的**：页面里固定摆放的控件写在 `.slint` 的 `targets` 数组里（活绑定，
  标签永远是最新的）；重复元素（卡片 / 标签 / 菜单行）和条件子树（hero / 空态 / 对话框）
  通过 `TargetBus` 登记到 Rust。取清单前调试通道会推进两轮 `TargetBus.gen` 并各让出一拍
  事件循环，所以 `targets` / `sweep` / `dead` / `click`（带 `target`）/ `hover` / `scroll`
  会比其它命令晚 100ms 左右回包。按 `x`/`y` 给的 `click` / `hover` / `scroll` 不刷新，
  不付这 100ms——代价是：**刚滚过 / 刚换页时不要用旧坐标**。
- 坐标来自元素的 `absolute-position`，包含滚动偏移与页面偏移；**被窗口裁掉一半的卡片**
  （中心点已在窗口外）不会进清单——点它的中心只会命中窗口外，报告"没反应"是噪音。
- 「添加服务器」对话框打开时，清单只剩 `dialog.*`：下层的按钮被遮罩挡住，点它们只会得到假结论。

### `view` 里的其它状态探针

| 字段 | 含义 |
|---|---|
| `status` | 连接状态（`nova_app::status::Status` 的 `Debug` 输出）：`Connected { … }` / `NoServer` / `MissingToken { … }` / `Expired { … }` / `RefreshFailed { … }`。横幅背后真正的状态，离线排查先看它 |
| `search_focused` | 顶栏搜索框是否真的持有焦点（Ctrl+F / 点击搜索后的交接态） |
| `focus_layer` | 键盘焦点层：`content` / `hero` / `navbar`（体检 R2-2b）。纯键盘 / 遥控器能不能到顶栏与 hero 按钮看它 |
| `nav_focus` | 顶栏焦点下标：`0..nav_tabs-1` = 库标签，`nav_tabs` = 搜索按钮，`-1` = 不在顶栏 |
| `hero_focus` | hero 按钮行的焦点：`0` = 「▶ 播放」，`1` = 「更多信息」（只在 `focus_layer == "hero"` 时有意义） |
| `nav_tabs` / `home.hero_item` / `home.hero_overview_len` | 顶栏标签个数；hero 的条目 id 与简介字符数（简介为 0 时 hero 没有两个按钮，`↑` 会跳过 hero 层直接去顶栏——不是 bug） |
| `nav_geometry.focus_tab_left` / `focus_tab_right` / `tabs_viewport_w` / `tabs_scroll_x` | 焦点标签在标签区视口里的左右沿与标签区滚动量：窄窗口下 `0 ≤ left && right ≤ viewport_w` 就是“焦点标签已经滚入可见区” |
| `detail.in_my_list` | 详情页是否已在「我的列表」（读即时 UI 属性，切换后立刻可见） |
| `dialog.open` / `address` / `user` / `password_len` / `error` | 「添加服务器」对话框的字段与错误文案；**密码只报字符长度**，不回显明文 |
| `player.playing` / `player.buffering` / `player.position_ms` / `player.error` / `player.volume` / `player.muted` | 播放页即时状态。注意 `view.player` **没有** `ended` 字段（播完判定在界面侧是 `PlayerView::ended && !playing`）：要确认“是不是真的播完了”看 `playing: false` + `position_ms` 到底 + `error: null`，或看日志 |
| `player.audio_track` / `player.subtitle_track` | 当前选中的音轨 / 字幕轨：`{"index": <mpv aid/sid>, "label": "…"}`；`null` = 字幕「关闭」或引擎还没枚举到轨道 |

密码只给长度的原因：回包会进脚本与日志，回显明文等于把凭据写出去。验证“输入框里有东西”
看 `password_len` 就够了。

从播放页用 `route` / `open` / `tab` 切走时，播放会话会像 Esc 一样先收尾（提交进度、
复位错误 / 轨道 / 标题）——所以 `view.player` 不会把上一部片子的错误带进下一个详情页。

设计取舍：

- 命令在 **UI 线程**执行（线程只负责收发），所以能安全改 Slint 与播放器状态；串行执行，
  不会两条命令同时改状态。
- 只监听本地文件、无鉴权、无加密——**只在开发机上开**。
- 交互是通过 **Slint 自己的 `Window::dispatch_event`** 注入的（`PointerMoved` /
  `PointerPressed` / `PointerReleased` / `KeyPressed` / `KeyReleased`）：不需要操作系统级
  注入工具，也不会干扰用户桌面。
- 目标清单是界面**自己登记**的（`ui/models.slint` 的 `Target` / `TargetBus`），不是从截图
  或可访问性树里猜的——新增控件必须登记，否则 `crates/nova-ui/tests/interaction_targets.rs`
  会因为"有 `TouchArea` 没登记"而红灯。

## 2. `--screenshot`：窗口自截（只含自己的窗口）

```bash
nova-desktop --screenshot /tmp/out.png --screenshot-delay 8000
```

两条路径，都只拍 Nova 自己的窗口（不截屏、不碰用户桌面）：

1. **有帧回调时**走 `glReadPixels` 回读自己的 back buffer（与播放捕获同一条路径），
   读到的是窗口那一刻真实显示的内容。
2. **等不到帧回调时**（无显示器 / 锁屏 / 窗口被完全遮挡：Wayland 合成器不下发
   `wl_surface.frame`，`request_redraw()` 是空操作）宽限 200ms（两个帧周期）后改走
   `slint::Window::take_snapshot()`：离屏渲染同一帧再读回像素。这条路**不需要任何**
   帧回调，也不需要截屏协议；视频画面（`BorrowedOpenGLTexture`）照样合成在图中。

所以：

- **不依赖 screencopy 协议**（这台机器的 KDE 没有，`grim` 直接失败）
- **不会抓到别人的窗口**（`spectacle -a` 抓的是"活动窗口"，会截到用户正在播的视频）
- **不抢焦点、不碰用户屏幕**（离屏路径连 present 都不做）
- **没有显示器也能截**（当前机器的 HDMI 拔掉、只剩无输出的会话时就是这条路径）

调试通道的 `screenshot` 命令与它完全同一条实现（同样有 200ms 宽限 + 离屏兜底）。

`--screenshot-delay` 是必需的：库标签要等后台刷新完成才出现，1.5s 时只能看到「首页 + 全部」，
看完整标签给 8000。

## 3. `--print-view`：不开窗口打印视图摘要

```bash
nova-desktop --print-view [--refresh] [--search foo]
```

无头、不发网络（除 `--refresh`），适合 CI 与"缓存里到底有什么"的取证。

## 4. 日志

```bash
NOVA_LOG=debug nova-desktop      # 或 info / warn（默认）
```

播放相关诊断走 `nova_playback` / `nova_playback_mpv` 两个 target，可以只开它们：

```bash
NOVA_LOG=warn,nova_playback_mpv=debug nova-desktop
```

退出 / 换片时的进度日志（`crates/nova-app/src/player/mod.rs` 的 `end_session`）。
两个字段别读反：

- `submitted`：这次退出**有没有把进度交给上报队列**（`build_progress` + `submit(Exit)` 的结果）；
- `flushed`：补发队列这个动作发出去了几条（`ProgressReporter::flush` 的 `outcome.sent`）。
  正常（没有积压）时它是 `0`，**不代表没上报进度**。

两种结束路径的文案不同：离开播放页是 `结束播放会话：补发进度`（有界等待 `FLUSH_TIMEOUT`，
带 `submitted` / `flushed` / `failed` / `queued`）；换片是
`结束播放会话：进度已交给上报 worker（换片不等待补发）`（debug 级，只有 `submitted`）。

## 用 `targets` + `sweep` 找"死按钮"

1. 起一个真实窗口（**必须**真实窗口：无头后端不跑异步刷新，卡片坐标也可能不同）：

   ```bash
   export NOVA_DATA_DIR=$HOME/.local/share/nova
   ./target/release/nova-desktop --window-size 1920x1080 --debug-socket /tmp/t.sock &
   ```

2. 确认目标清单有东西，并按 id 看一眼：

   ```bash
   printf '{"cmd":"targets"}\n' | socat - UNIX-CONNECT:/tmp/t.sock
   ```

3. 切到要检查的页面（`route` / `tab` / `search`），再扫一遍：

   ```bash
   printf '{"cmd":"route","value":"library"}\n' | socat - UNIX-CONNECT:/tmp/t.sock
   printf '{"cmd":"sweep"}\n' | socat - UNIX-CONNECT:/tmp/t.sock > /tmp/sweep.json
   ```

   回包结构：

   ```json
   {"ok":true,"page":"library","count":37,"changed":34,"aborted":false,
    "dead":[{"id":"library.nav.tab.1","kind":"tab","label":"电影"}],
    "audio_restored":true,
    "results":[{"id":"library.sort","kind":"button","label":"排序：最近添加  ▼",
                "changed":true,"restored":true,"before":"…","after":"…"}]}
   ```

   - `changed:false` **不是失败**：有的按钮只改局部状态（悬停态），有的是"点当前项"的空操作；
   - 每条都带 `kind` / `label`，方便人工核对；
   - `restored:false` 表示点击把应用带到了回不去的状态（例如播放页），扫描会提前收尾
     （`aborted:true`）——这时先手动 `route` 回原页再继续；
   - `audio_restored`：sweep 会点到音量 / 静音，扫完会把音量与静音改回去；`true` = 已经
     恢复成扫描前的值，`false` = 没恢复成功（真机上出现就是 bug，不能忽略）。

4. 只看"死按钮"，适合 CI / 人工核对：

   ```bash
   printf '{"cmd":"dead"}\n' | socat - UNIX-CONNECT:/tmp/t.sock
   ```

5. 单独复现某个按钮：先 `hover` 再 `click`，然后用 `view` 看状态有没有变。

   ```bash
   printf '{"cmd":"click","target":"library.sort"}\n' | socat - UNIX-CONNECT:/tmp/t.sock
   printf '{"cmd":"view"}\n' | socat - UNIX-CONNECT:/tmp/t.sock   # library.sort 变了没有
   ```

**sweep 会真的点所有东西**，所以它也会点「播放」「全屏」「连接」这类有副作用的按钮：
播放会被收掉、对话框会被关掉、全屏会被恢复，但**连接的服务器/写盘操作不会被撤销**。
在真实服务器上跑之前先想清楚（或者只 `click` 具体目标）。

## 典型用法

**"首页为什么是黑的"**（真实踩过的坑）：

```bash
nova-desktop --debug-socket /tmp/nova.sock &
printf '{"cmd":"view"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock      # 数据在不在？
printf '{"cmd":"screenshot","path":"/tmp/a.png"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```

数据在但画面黑 → 渲染层问题（当时是 mpv 无条件画到窗口上盖掉了界面）。

**验证某个页面**：

```bash
printf '{"cmd":"open","item":"196390"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
sleep 1
printf '{"cmd":"screenshot","path":"/tmp/detail.png"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```

## 用 `click` / `sweep` 排查"按钮点了没反应"

### 先唤醒，再点

播放页的控制层 **3 秒无输入就自动隐藏**，隐藏后 `targets` 是空的——这时候去
`click {"target":"player.play-pause"}` 会得到 `unknown target`，看起来像"按钮不存在"。
先发一个按键把控制层叫醒，再点：

```bash
printf '{"cmd":"key","key":"Down"}\n{"cmd":"targets"}\n' | socat - UNIX-CONNECT:$SOCK
printf '{"cmd":"click","x":101,"y":619}\n'                            | socat - UNIX-CONNECT:$SOCK
```

用 `key Down` 唤醒是安全的：控制层**隐藏时**第一下 `↑` / `↓` **只唤醒、不改音量**
（之前它同时走音量 −5%，每次唤醒都无声地降一档，体检 R2-5）。控制层**可见时**
`↑` / `↓` 才是音量 ±5%。`←` / `→` 的快进快退、`Space` 的暂停在隐藏时仍直接生效，
所以拿它们唤醒会**带**上 seek / 暂停副作用——只想唤醒就用 `Down`（或 `Up`）。

**`hover` 同样要先唤醒**（实测过一次误判）：控制层隐藏时那一次 `PointerMoved` 只会
把它叫醒，控件本身当时不在命中区里，`has-hover` 没被置上——看起来就像"hover 不生效"。
真机鼠标没问题，因为移动是一串事件。播放页验证悬停（音量滑杆展开）的完整序列：

```bash
printf '{"cmd":"key","key":"Down"}\n{"cmd":"hover","target":"player.mute"}\n{"cmd":"targets"}\n' \
  | socat - UNIX-CONNECT:$SOCK     # targets 里应出现 player.volume（收起时宽 0，不登记）
```

### 判断"有没有生效"要用即时状态，不要读 `state.player`

`view` 里的 `player` 字段现在读的是**即时视图**（`Player::live_view`）。
它以前读的是 `UiState.player`，而那个值受 `VIEW_INTERVAL` 节流、只在
`set_player` 时更新——播放中读它会得到 `playing: false`，据此下结论会得到
**完全相反**的判断（我就因此误判过"播放器所有按钮点不动"）。

判断播放是否真的在跑，两个可靠信号：

1. `view` 的 `player.position_ms` 在两秒后是否增长（暂停时不增长）；
2. `targets` 里 `player.progress` 的 `label`（形如 `"0:07 / 1:54:57"`）是否在走。

### 已验证的一次完整点击链

```
playing=True  pos=6   → 点 play-pause
playing=False pos=6   → 两秒后 pos 仍是 6（确实暂停了）
playing=False pos=6   → 再点一次
playing=True  pos=8   → 恢复播放
                      → 点 back：route 从 Player 变回 Home
```
