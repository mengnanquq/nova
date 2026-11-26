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
| `{"cmd":"tab","library":"<id>"}` | 切媒体库标签（空串=首页） |
| `{"cmd":"search","term":"..."}` | 用本地缓存过滤首页（旧行为，兼容保留） |
| `{"cmd":"search","term":"...","page":true}` | 进搜索页并把查询词填好（本地缓存立即出结果，同时打一次服务端搜索） |
| `{"cmd":"screenshot","path":"/tmp/x.png"}` | 窗口自截（见下），**异步**：等 `view` 里看不到 pending 或直接等文件出现 |
| `{"cmd":"targets"}` | 当前页面的**可交互目标清单**（按钮 / 开关 / 输入框 / 卡片 / 标签 / 滑杆 / 滚动箭头） |
| `{"cmd":"click","target":"library.sort"}` | 按名字点一个目标（发 `PointerMoved` → `PointerPressed` → `PointerReleased`） |
| `{"cmd":"click","x":123,"y":45}` | 按窗口坐标点（验证"名字清单是否覆盖真实可点区域"） |
| `{"cmd":"hover","target":"home.row.continue.card.0"}` | 只发 `PointerMoved`（验证悬停才出现的东西：音量滑杆、卡片放大、行箭头） |
| `{"cmd":"scroll","x":400,"y":300,"dy":360}` | 在该坐标滚一下（先 `PointerMoved` 再 `PointerScrolled`），见下 |
| `{"cmd":"scroll","target":"library.grid","dy":360,"dx":0}` | 同上，但坐标取目标中心（`target` 与 `x`/`y` 二选一） |
| `{"cmd":"key","key":"Enter"}` | 发一次按键：`Enter` / `Escape` / `Left` / `Right` / `Up` / `Down` / `Space` / `Tab` / `Backspace` 或单个字符 |
| `{"cmd":"type","text":"沙丘"}` | 逐字符输入（先 `click` 到输入框上，焦点在它那里才收得到） |
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

`view` 里还有一组悬停探针：`home_geometry.row0_zoom_on`（继续观看行的放大覆盖层是否
打开），以及 `library_geometry` / `search_geometry` 里的 `zoom_on`。悬停卡片后从
`false` 变 `true`、指针移开又变回 `false`，这就是“`hover` 真的命中了吗”的直接证据。
网格与首页行用同一条放大规则：**悬停优先，其次键盘焦点**（键盘焦点只在按过方向键、
`nav-active` 为真时才放大，避免一打开页面第一张卡就是放大状态）。

- **清单是两路合并的**：页面里固定摆放的控件写在 `.slint` 的 `targets` 数组里（活绑定，
  标签永远是最新的）；重复元素（卡片 / 标签 / 菜单行）和条件子树（hero / 空态 / 对话框）
  通过 `TargetBus` 登记到 Rust。取清单前调试通道会推进两轮 `TargetBus.gen` 并各让出一拍
  事件循环，所以 `targets` / `sweep` / `dead` / `click`（带 `target`）/ `hover` / `scroll`
  会比其它命令晚 100ms 左右回包。按 `x`/`y` 给的 `click` / `hover` / `scroll` 不刷新，
  不付这 100ms——代价是：**刚滚过 / 刚换页时不要用旧坐标**。
- 坐标来自元素的 `absolute-position`，包含滚动偏移与页面偏移；**被窗口裁掉一半的卡片**
  （中心点已在窗口外）不会进清单——点它的中心只会命中窗口外，报告"没反应"是噪音。
- 「添加服务器」对话框打开时，清单只剩 `dialog.*`：下层的按钮被遮罩挡住，点它们只会得到假结论。

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

走 `glReadPixels` 回读自己的 back buffer（与播放捕获同一条路径），所以：

- **不依赖 screencopy 协议**（这台机器的 KDE 没有，`grim` 直接失败）
- **不会抓到别人的窗口**（`spectacle -a` 抓的是"活动窗口"，会截到用户正在播的视频）
- **不抢焦点、不碰用户屏幕**

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
    "results":[{"id":"library.sort","kind":"button","label":"排序：最近添加  ▼",
                "changed":true,"restored":true,"before":"…","after":"…"}]}
   ```

   - `changed:false` **不是失败**：有的按钮只改局部状态（悬停态），有的是"点当前项"的空操作；
   - 每条都带 `kind` / `label`，方便人工核对；
   - `restored:false` 表示点击把应用带到了回不去的状态（例如播放页），扫描会提前收尾
     （`aborted:true`）——这时先手动 `route` 回原页再继续。

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
