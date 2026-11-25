# Netflix 1:1 复刻清单

按"能核对的数字"写。**凡是要 Netflix 专有资产的地方，明确写替代方案**，不假装一模一样。

## 客观限制（必须先接受）

| 项 | Netflix | 我们 | 处理 |
|---|---|---|---|
| 字体 | Netflix Sans（专有） | 不能分发 | 用 `Inter` + `Noto Sans CJK SC`，字距 `-0.02em`、字重 400/700，逼近观感 |
| 标题 | 透明底标题字 logo | 服务端不一定有 | 有 logo 图就用图；没有就用文字（48px bold，行高 1.05） |
| 行内图 | 原生 16:9 剧照 | 服务端给 16:9 backdrop + 2:3 海报 | 16:9 行用 backdrop；"最近添加"用 2:3 海报（Netflix 也有 2:3 行） |
| 预览 | 悬浮播放静音预告 | 无预览流 | 不做假预览；放大卡只显示信息与进度 |
| 图标 | 自有图标集 | 无 | 手绘 path（放大镜/播放/加号/箭头/勾），不依赖字体字形 |

## 1. 全局尺寸（1280 基准，按视口线性缩放）

| 元素 | 数值 |
|---|---|
| 页面左右留白 | `4vw`（1280 → 51px；≥1920 固定 60px） |
| 顶栏高 | 72px；未滚动时 `linear-gradient(to top, rgba(0,0,0,0) 0%, rgb(0,0,0) 100%)`；logo 字高 20px，字距 -0.5px；链接 14px/400，间距 `1.5vw` |
| 头像 | 32px 圆，`#E50914` 底 + 白色首字母 14px bold |
| 搜索图标 | 24px 描边 2px；展开时输入框 240×36，左图标 20px |
| billboard 高 | `min(88vh, 720px)`；内容底部距 billboard 底 `18%` |
| billboard 标题 | 48px bold / 行高 1.05 / 字距 -0.02em；logo 图高 64–96px |
| billboard 元信息 | 14px / `#B3B3B3` / 行高 1.4 / 上距 12px |
| billboard 简介 | 16px / 白 / 行高 1.5 / 宽 560px / 最多 3 行 / 上距 16px |
| 按钮 | 高 44px，圆角 4，内边距 0 24px；主按钮白底黑字，次按钮 `#FFFFFF26` 白字；图标 22px + 间距 8px |
| 行标题 | 22px bold（Netflix 是 1.4vw），底距 12px；右侧「查看全部 ›」14px `#B3B3B3`，悬停白 |
| 行间距 | 40px；首行距 billboard 底 24px |
| 行内卡 | 16:9 卡宽 `16.6vw`（1280 → 212px），2:3 卡宽 `14.2vw`（182px）；间距 8px |
| 卡片圆角 | 4px（放大后 6px） |
| 卡片放大 | `scale 1.35`（Netflix 观感），延迟 400ms 触发，过渡 `Theme.motion-slow`（300ms）ease-out；放大卡宽 = 卡宽×1.35 |
| 放大卡信息 | 标题 14px bold、元信息 12px `#B3B3B3`、迷你进度条 3px |
| 进度条 | 高 3px，`#E50914`，距卡底 6px，左右各 6px |
| 行左右箭头 | 48×48 半透明黑圆角块（`#000000B3`），垂直居中，悬停行时出现 |

## 2. 顶栏行为

1. 首屏：透明，字白，浮在 billboard 上。
2 滚动 > 40px：背景渐变到 `#141414`，180ms；同时出现一层底部 `1px #2A2A2A` 分隔（极淡）。
3. 悬停链接：白 + `opacity 1`；未选中 `#B3B3B3`。
4. 搜索：点图标 → 输入框从 24px 宽展开到 240px（180ms），并自动聚焦；`Esc` 收起。
5. 右到左顺序：搜索、头像（Netflix 还有通知铃，**不做**：我们没有通知系统）。

## 3. 行与卡片交互

1. 默认卡：只有图 + 圆角，**无边框无阴影**。
2. 悬停/聚焦 400ms 后放大到 1.35，卡片上移 `-12px`，信息面板从卡片底部展开（高 56px，`#181818`）。
3. 放大卡要在**相邻卡之上**绘制（覆盖层），且不参与行布局（零跳动）。
4. 行两端各留 `4vw`，滚动时卡片可以从留白区穿出（`clip: false` 的行容器 + 行级遮罩）。
5. 滚轮横向滚动；拖拽不启用（Netflix 桌面端是滚轮/触控板）。

## 4. 页面

- **Home**：billboard → 继续观看（16:9，带进度）→ 最近添加（2:3）→ 各库前 20 条（16:9）。
- **Library**：标题 34px + 数量 14px `#B3B3B3`；网格 6 列（≥1440 为 7 列），卡 2:3，间距 8px。
- **Detail**：backdrop 78vh + 上下渐变；标题 48px（高度自适应）；元信息 `·` 分隔、14px/500；简介宽 697px、不限行数；`▶ 播放` + `+ 我的列表`；版本下拉。逐项实测值见第 7 节。
- **Player**：控制层保持现有结构，进度条与按钮换 Netflix 红/白。

## 5. 明确不做

- 预览视频、通知铃、多用户切换、Top 10 大数字行、Netflix 的骨架屏**扫光**动画（
  我们只做占位底的单次透明度呼吸，见第 9 节）、
  自动播放下一集倒计时（阶段 5 之后再说）。

## 6. 验收

| 检查项 | 方法 |
|---|---|
| 首屏像不像 | `--screenshot` 自截，与 Netflix 官网截图并排看：billboard 占比、标题位置、按钮尺寸、行标题位置 |
| 卡片放大不跳布局 | 放大前后行内其它卡坐标不变（快照逐像素对比行区域） |
| 放大卡覆盖层级 | 放大卡压住相邻卡，不被裁切 |
| 顶栏滚动变色 | 滚轮下滚 40px 后截一张，对比顶栏底色 |
| 中文不豆腐 | 快照目视 |

## 7. Netflix 实测值（1920×1080，netflix.com/tw/title/80100172）

口径：1920×1080 视口、**免登录 non-member 版**详情页，取值方式是 DevTools 读
`getComputedStyle` / `getBoundingClientRect`（已知条目 `80100172`）。

> **「播放」/「更多信息」双按钮与播放器的完整控件布局因未登录无法实测**（该区域被登录引导替代），
> 所以按钮尺寸**沿用我们现有值**（高 44px、白底/半透明白底）。
> 但 hero 内的**视频控制圆钮**与**播放器样式表（CSSOM）**可以拿到，见下面的「顶栏」「播放器圆钮」两行，
> 我们据此对齐播放器的圆钮与进度条交互；本节其余项均为实测。

| 项 | 实测值 | 落在代码哪里 |
|---|---|---|
| 顶栏 | `height: 72px` / `z-index: 20` / `background-image: linear-gradient(to top, rgba(0,0,0,0) 0%, rgb(0,0,0) 100%)` | `Theme.navbar-height: 72px`；未滚动态用 `Theme.navbar-scroll-gradient`（`180deg` + `#000000 → #00000000`，即 CSS `to top` 的反写）再向下延伸 `Theme.navbar-gradient-extend: 56px`；滚动后仍是 `Theme.surface` 实底 |
| 播放器圆钮（hero 内视频控制钮实测） | 40×40 / `border-radius: 20px`（正圆）/ `background: rgba(22,22,22,.7)` / `border: 1px solid rgba(128,128,128,.7)` / 内图标 24×24 / 两钮间距 12px | `Theme.player-control-size: 40px`、`Theme.player-icon-bg: #161616B3`、`Theme.player-icon-border: #808080B3` + `Theme.player-icon-border-width: 1px`、`Theme.player-icon-glyph: 24px`、行内 `spacing: Theme.space-3`；左上返回钮保持 44×44（不是实测项，保留现状） |
| 进度条交互（播放器样式表 CSSOM 原文，未渲染验证） | `.nfp .scrubber-container .scrubber-head:hover { transform: scale(1.2) }`；`.nfp .is-scrubbing .scrubber-container .track, .nfp .scrubber-container:hover .track { height: 1em }`；`.nfp .volume-controller .scrubber-target:hover { transform: scale(1.2) }` | 拖动头 `Theme.player-bar-knob: 12px` × `Theme.player-bar-knob-scale: 1.2`；轨道 `Theme.player-bar-height: 4px` → `Theme.player-bar-height-hover: 8px`（Netflix 的 `1em` = 16px 属大屏播放器，我们的控制层按 40px 控件缩到 8px）；悬停 / 拖动均走 `Theme.motion` 过渡 |
| 页面底色 | `background-color: rgb(15,15,15)` | `Theme.detail-bg`（**不动** `Theme.bg`，其它页仍然纯黑） |
| hero 高度 | 845px / 视口 1080px = 0.782 | `Theme.detail-backdrop-ratio: 0.78` |
| hero 底部渐变 | `linear-gradient(rgba(0,0,0,0) 40%, rgba(0,0,0,.6) 100%)`，占 hero 高 60% | `detail.slint` 底部层：高 `backdrop-height * 0.6`、`@linear-gradient(180deg, #00000000 40%, #00000099 100%)` |
| hero 顶部压暗 | 浅黑渐变压住顶栏 | 顶部层高 `backdrop-height * 0.15`，`Theme.scrim-soft` → 透明 |
| 正文左右 padding | 200px（1920） | `Theme.detail-gutter`（≥1920 为 200px，否则沿用 `Theme.gutter`） |
| 正文列宽 | 1520px（1920 − 200×2） | 同上 |
| section 间距 | 40px | `Theme.section-gap: 40px` |
| 区块标题 | 32px / 700 / 行高 46px / 下距 12px | `Theme.font-32` + `Theme.weight-bold`（详情页当前没有 section 标题，故仅立 token） |
| 主标题 | 48px / 700（有 logo 图时用图，无图回退文字 48px bold） | `Theme.font-48`；标题块高度**自适应**（`min-height: 56px` + 换行，不再 160px 固定高度） |
| 元信息 | 14px / 500 / 项间距 30px（`margin-left: 30px`）/ 分隔点 `rgba(255,255,255,.4)` | 字号 14px、字重 `Theme.weight-medium` 已对齐；**项间距与点色在数据层字符串内**（`detail_meta_line` 用 `·` 拼成一串），Slint 侧不能逐项调间距 / 逐字上色，`Theme.detail-meta-dot: #FFFFFF66` 先立 token |
| 简介 | 16px / 行高 24px / 宽 696.95px（≈697）/ 不限行数 | 宽 697px、`wrap: word-wrap`、去掉 `elide` 与 90px 固定高（Slint 无 `line-height`，行距用字体默认） |
| 集数（横版）卡 | 327×322（容器）；缩略图 307×173 | `Theme.card-landscape-w` = 327px（≥1280，含 1920；447px 只属于「预告」行，我们没有那种行）；`card-landscape-h = w × 9/16` |
| 集数卡外框 | `padding 8px` / `radius 16px` / `background rgba(255,255,255,.1)` / `border 2px solid rgba(255,255,255,.04)` | `Theme.card-frame-bg` / `card-frame-border` / `card-frame-padding` / `card-frame-border-width`；`poster_card.slint` 统一生效（首页行卡同步带上内衬，属预期） |
| 缩略图圆角 | 16px | `Theme.radius-poster`（图片内缩后自己带圆角） |
| 时长角标 | `rgba(0,0,0,.9)` 底 / `radius 4px` / `padding 1px 12px` / 13px | 现有 `Theme.badge-bg`（`#000000B3`）+ `radius-badge` 4px + 12px 字号；**未逐项对齐**（我们的角标在卡右上角，Netflix 在缩略图右下角） |
| 竖版（海报）卡 | 240×336（5:7） | `Theme.card-portrait-w/h` |
| 卡间距 | 12px（`margin-right: .75rem`，首末归零） | `Theme.card-gap` |
| 主 CTA（播放按钮） | 266×48 / `radius 100` / `#E50914` / 18px 500 | **无法实测**（登录墙）；沿用现有白底 44 高按钮，不改 |

实现备注（实测值的边界）：

1. 底部渐变层取 hero 的 60% 高、内部 stops 40%→100%，因此实际变暗起点落在 hero 的
   76% 处；若按「整段 hero 上 40% 处开始变暗」解释，应改为占满 hero 高、stops 仍 40%→100%。
   当前按本节表格实现（层高 60% + stops 40/100）。
2. `Text` 直接作为 `VerticalLayout` 子项时，无头渲染器（`i-slint-backend-testing` 的
   parley 测量）会把换行文本的 preferred-height 算成按极窄宽度折行的结果（实测 80 字简介
   被算成 ≈711px），导致后续按钮被挤出视口。因此标题与简介都套一层定宽 `Rectangle` 容器，
   用 `Text.preferred-height` 撑开高度。
3. 元信息行的「项间距 30px」与「点 `.4` 白」都在 `viewmodel::detail_meta_line` 拼好的
   单串里，不碰 Rust 逻辑就无法逐项生效——需要后续把元信息拆成分段字段。
4. 集数卡外框对 `PosterCard` 是全局改动，放大覆盖层（`CardZoom`）保持原样式（它是一张
   独立的放大卡，本次未纳入实测范围）。

**仍然无法实测的部分**（不要当成事实）：

- 播放器的**完整控件布局**（传输行顺序、间距、左右分组）需要登录才能拿到，保持不变。
- 播放器**中央大钮尺寸**（现用 `Theme.player-play-size: 56px`，中央覆盖图标圈 `Theme.player-icon-size: 88px`）无法实测，**保留现状**。
- 进度条轨道静息高度（4px）也是我们的选择：Netflix 只给出悬停态的 `1em`，未给出静息值。

## 8. 搜索页

**Netflix 搜索页无法实测**：`netflix.com/search`、`netflix.com/browse` 未登录访问一律
`302` 回登录页，archive.org 上也只留到 `301/302` 的快照（没有渲染后的页面）。因此本节
把每一项都标成「实测 / 推导 / 无法验证」，推导项只按 Netflix 的**公开行为**（产品文档、
公开截图、行/卡组件复用）推导，不假装是量出来的。

| 项 | 取值 | 出处 |
|---|---|---|
| 搜索是**独立路由**，不是首页就地过滤 | 输入即切到独立结果页 | **推导**（Netflix 搜索是 `/search?q=` 独立页；我们对应 `Route::Search { term }`） |
| 顶部大搜索框 | 高 48px、字号 22px、透明底 + 底部 1px 线；左右 `content-gutter`、`navbar-height` 之下留 40px | **推导**（登录墙后看不到；尺寸取本仓库已有的 22px 标题档与 48px 触控高度，不引新数字） |
| 结果头 | `「{term}」的搜索结果` + `共 {total} 部`（22px bold + 12px `text-muted`） | **推导**（文案与层级按 Netflix 的 "Search results for …" 结构，字号复用本仓库标题行） |
| 结果网格 | 与媒体库**同一个** 5:7 竖版网格：6 列、`content-gutter`、12px `card-gap`、`radius-poster`、放大覆盖层 | **实测（网格本身）+ 推导（用在搜索页）**：240×336 = 5:7、12px 间距、1520px 正文列来自第 7 节详情页实测；搜索页复用同一组件属推导 |
| 结果只收电影 / 剧集 | 集（Episode）/ 季（Season）不进结果 | **推导**（搜索结果应是"片"，集/季属于详情页的选集层级） |
| 空态 | "没有找到相关内容" + "换个关键词试试" | **推导**（文案按 Netflix 的空态句式，不是截屏原文） |
| 加载态 | 一行"搜索中…"，不加 spinner | **推导 + 工程约束**（零依赖，不做骨架屏动画） |
| 失败态 | 错误文案 + "重试" | **推导**（错误卡样式复用本仓库 `EmptyState`） |
| `Esc` | 清空查询并回首页 | **推导**（Netflix 搜索页的取消/返回惯例） |
| `Enter` | 立即提交，焦点还给全局键盘（方向键才能在网格里移动） | **推导**（键盘可达性的最低要求） |
| 网络行为 | 输入时先用本地缓存立即出结果，停手 300ms 后打服务端；每次输入递增"代次"，过期结果丢弃 | **我们自己的实现选择**（Netflix 是 SSR + 即时接口，无法实测其防抖窗口） |
| 结果数量上限 | 一次取 60 条 | **我们自己的实现选择**（Netflix 是无限滚动，无法验证其分页尺寸） |
| 页内是否画顶栏 | **不画**：搜索框就是页头（顶部 72px + 40px 留白是纯黑） | **推导**（避免出现两个搜索框；Netflix 的搜索页 header 里是 logo，我们不重复画第二个顶栏） |

### 实现与证据

- 网格几何只有一份：`ui/poster_grid.slint`。媒体库页与搜索页都实例化它，抽组件前后
  `library.png` / `netflix-library.png` 逐像素一致（`cmp` 比对）。
- 尺寸断言：`crates/nova-ui/tests/search_layout.rs` 读 `.slint` 暴露的探针
  （`search-columns` / `search-card-w` / `search-card-h` / `search-first-card-x` / …），
  断言 6 列、`卡高 = 卡宽 × 7/5`（±1px）、首列左起点 = `content-gutter`（±1px）、
  6 列 + 5 个间距正好铺满正文列。
- 快照：`target/snapshots/search-results.png`（固定中文查询词"沙丘"、6 张结果、最后
  一格故意不推海报以显示占位）与 `search-empty.png`。
- 放大覆盖层的上边界在搜索页是**结果头下沿**（`grid-top`），不是媒体库的顶栏下沿——
  否则第一行卡片的放大态会盖住结果头（拍快照时实际踩到过）。

## 9. 动效

### 9.1 取值来源（三档，别混）

| 档位 | 含义 | 本次哪些项 |
|---|---|---|
| **官方 CSS 原文** | 从 `assets.nflxext.com` 的 `akiraClient.css`（403KB）与 `/tw/browse/genre/34399` 页内联 Hawkins CSS（418KB）里直接读到的规则/令牌 | 卡片悬停过渡（`transform 0.2s ease-in-out`）、骨架周期（`4000ms linear`）与骨架底色 `rgb(35,35,35)`、按钮三态色 / 250ms / 按下 0ms / 两条贝塞尔、行 `.lolomoRow` 的 `540ms cubic-bezier(.5,0,.1,1)` |
| 公开资料 | 第三方 UI 拆解 / 公开截图观感，不是我们量的 | 卡片悬停后约 400ms 才展开的"延迟" |
| 我们自定 | 官方没给、也测不到，纯工程取值 | 内容/页面入场、顶栏变色与标签字色、箭头悬停、控件显隐、进度条轨道过渡 |

**没有实测到的**：动画的实际观感/时长我们**没有逐条实测**（免登录页只能量静态布局与读样式表原文）。
所以除上表前两档外，值都是工程取值，且**集中在 `crates/nova-ui/ui/theme.slint`**，
改一处即可全局调整。`grep -n "duration: [0-9]" crates/nova-ui/ui/*.slint` 为空（组件里只引用 token）。

### 9.2 token 表（`theme.slint`）

| token | 值 | 出处 | 用途 |
|---|---|---|---|
| `motion-fast` | 120ms | 我们自定 | 微交互（箭头悬停、顶栏标签字色） |
| `motion` | 180ms | 我们自定 | 默认过渡（透明度、顶栏变色、播放器控件显隐、进度条轨道） |
| `motion-slow` | 300ms | 我们自定 | 内容 / 页面入场 |
| `motion-page` | 220ms | 我们自定 | 页面主要区域切换（当前无消费者：对话框/空态入场没做成动画，见 9.5） |
| `motion-pulse` | 4000ms | **官方 CSS**：`animation: loading 4000ms infinite linear` | 骨架周期（我们**没有**复刻循环扫光，见 9.5） |
| `motion-delay` | 400ms | 公开资料 | 卡片悬停后展开的延迟（`zoom-delay` 复用） |
| `motion-zoom` | 200ms | **官方 CSS**：`transition: transform 0.2s ease-in-out` | 卡片放大 / 还原 |
| `motion-button` | 250ms | **官方 Hawkins 令牌**：`Button-Duration` | 按钮悬停过渡 |
| `motion-press` | 0ms | **官方 Hawkins 令牌**：`Button-DurationPressed` | 按钮按下（立即响应） |
| `motion-row` | 540ms | **官方 CSS**：`.lolomoRow{transition:transform .54s ...}` | 行入场（官方用于"行展开"，我们借用） |
| `motion-rise` / `motion-rise-hero` | 6px / 8px | 我们自定 | 入场位移量 |

缓动没有做成 token：`in property <easing>` 在 Slint 1.19 会发
"Properties of type easing are not supported yet for public API" 的构建警告，所以曲线直接写在
各个 `animate` 里，且只用官方原文的四个值 + `ease-out`：

- 卡片悬停：`ease-in-out`（官方 CSS）
- 按钮悬停：`cubic-bezier(0.32, 0.94, 0.6, 1)`（官方 `Button-EasingHovered`）
- 按钮离开 / 按下：`cubic-bezier(0.4, 0, 0.68, 0.06)`（官方 `Button-Easing`）
- 行入场：`cubic-bezier(0.5, 0, 0.1, 1)`（官方 `.lolomoRow`）
- 其余：`ease-out`（我们自定）

### 9.3 逐项对照

| 元素 | 触发 | 时长 | 缓动 | 出处 |
|---|---|---|---|---|
| 卡片放大 / 还原 | 悬停或聚焦 | `motion-zoom`（200ms）+ 延迟 `motion-delay`（400ms） | `ease-in-out` | **官方 CSS**（`transform 0.2s ease-in-out`）+ 公开资料（400ms 延迟） |
| 放大卡信息面板 | 由 `zoom-factor` 驱动（放大过 1.05 才开始现形） | `motion` | `ease-out` | 我们自定（面板 56px / `#181818` 是实测值，出现时机不是） |
| 按钮悬停（主 CTA / 次按钮 / 清空搜索 / 播放器圆钮 / 重试 / 添加服务器） | 悬停 | `motion-button`（250ms） | `cubic-bezier(0.32, 0.94, 0.6, 1)` | **官方 Hawkins 令牌** |
| 按钮按下 | `TouchArea.pressed` | `motion-press`（0ms） | `cubic-bezier(0.4, 0, 0.68, 0.06)` | **官方 Hawkins 令牌**（底色用 `Button-SurfacePressed`，见 9.4） |
| 按钮三态底色 | 常态 / 悬停 / 按下 / 禁用 | — | — | **官方 Hawkins 令牌**：`#E50914 / #C11119 / #99161D / rgba(229,9,20,.4)` 落在 `Theme.accent*` |
| 行入场（淡入 + 6px 上浮） | 数据空 → 非空 | `motion-row`（540ms） | `cubic-bezier(0.5, 0, 0.1, 1)` | 时长/曲线来自**官方 CSS**（那行是 `.lolomoRow` 的"行展开"），入场是我们借用 |
| 首页 billboard 文案块入场 | 同上 | `motion-row` | 同上 | 同上 |
| 网格入场（媒体库 / 搜索结果） | 数据空 → 非空 | `motion-slow` | `ease-out` | 我们自定（触发时机见 9.5） |
| 详情页 backdrop 淡入 + 标题块 +8px 归位 | 数据到达 | `motion-slow` | `ease-out` | 我们自定 |
| 顶栏滚动变实底 | 滚 > 40px | `motion` | `ease-out` | 我们自定（终态色 `#141414` 是实测的） |
| 顶栏标签悬停 / 激活字色 | 悬停 / 切标签 | `motion-fast` | `ease-out` | 我们自定（净止态颜色实测，过渡时长不是） |
| 顶栏搜索框展开 | 聚焦 | `motion` | `ease-out` | 我们自定（展开宽 240px 实测） |
| 行左右箭头悬停 | 悬停 | `motion-fast` | `ease-out` | 我们自定（`opacity 1 → 0.85`，图标不位移） |
| 行左右箭头翻页 | 点箭头 | Slint `Flickable.scroll-to(..., ScrollBehavior.smooth)` 的库内 spring | 库内 spring | 我们自定（见 9.6 的"拖拽必须跟手"） |
| 播放器控制层显隐 | `controls-visible` | `motion` | `ease-out` | 我们自定 |
| 播放器进度条轨道 / 拖动头 | 悬停 / 拖动 | `motion` | `ease-out` | 尺寸来自第 7 节 CSSOM 实测，时长自定 |
| 骨架占位 | 静态（无过渡） | — | — | 底色 `#232323` 是**官方 CSS**；呼吸/扫光没做，见 9.5 |
| 空态 / 对话框出现 | 元素出现 | — | — | **没做**，见 9.5 |
| 焦点环 | 焦点进出 | `motion` | 默认 | 我们自定 |

### 9.4 官方 CSS 原文摘录（便于以后核对）

```css
/* 卡片悬停（akiraClient / 内联 CSS） */
transform: scale(1.05); transition: transform 0.2s ease-in-out; z-index: 2;
/* 骨架 */
background-color: rgb(35, 35, 35); animation: loading 4000ms infinite linear;
/* 行展开 */
.lolomoRow, .rowContainer { transition: transform .54s cubic-bezier(.5, 0, .1, 1) 0s; }
/* Hawkins 按钮令牌 */
Button-Surface: rgb(229,9,20);            Button-SurfaceHovered: rgb(193,17,25);
Button-SurfacePressed: rgb(153,22,29);    Button-SurfaceDisabled: rgba(229,9,20,0.4);
Button-Duration: 250ms;                   Button-DurationPressed: 0ms;
Button-Easing: cubic-bezier(0.4,0,0.68,0.06);
Button-EasingHovered: cubic-bezier(0.32,0.94,0.6,1);
Button-BorderRadius: 0.25rem;             Button-TextFontWeight: 500;
```

口径提示：官方卡片放大是 `scale(1.05)`；本仓库保留 `Theme.zoom-factor: 1.35`
（之前的设计取舍 + `zoom-allowance` / 覆盖层几何都按 1.35 立的值），**属视觉取舍，不在本次动效范围**。
另外官方确认的布局事实（本次未改布局，仅记录口径）：行标题 ≥1280 是 32px/700（960–1279 是 24px/500，
<960 是 20px/500）；海报 ≥1920 是 240×336（radius 16px），≥1280 是 184×257.6（radius 12px）。

### 9.5 做成了近似 / 没做到（不要当成 Netflix 一致）

- **骨架屏**：官方是 `4000ms infinite linear` 的斜向扫光。我们**不做循环、也不做呼吸**——
  任务硬约束禁止定时器 / 每帧重绘，而"首帧之后再触发一次"在 Slint 里没有可靠的声明式时机
  （实测：属性第一次求值、`init` 里置位、`visible` 由 false 变 true 的第一帧，都不播动画）。
  所以骨架是**静态占位**：官方底色 `#232323` + 0.75 不透明度。`motion-pulse: 4000ms`
  这个周期 token 保留着，将来实现扫光时直接用。
- **空态 / 对话框入场**：同上原因**没做**（即时显示）。`scale 0.98 → 1` 也做不到：
  Slint 1.19 的 `Transform`（`transform-scale-x/y`）是 internal 元素，用户代码实例化会编译报错
  （实测报错原文：`Unknown element 'Transform'. (The type exists as an internal type ...)`）。
- **按钮按下不是 `scale 0.98`**：同 `Transform` 限制。官方按钮按下的视觉是**底色变
  `Button-SurfacePressed`**，我们照这个做（accent 按钮换底色；白底/中性底的按钮叠一层
  `Theme.press-wash` 压暗），**没有任何位移**。
- **网格 / 行入场的触发时机**：Slint 对属性的"第一次求值"不播动画，所以"元素此前从没被绘制过"
  的那一刻不会淡入。为此首页 / 媒体库 / 搜索页的内容区都改成**常驻 + opacity 门控**
  （空数据时内容区本来也画不出东西；`interactive` 同时关掉，不让不可见滚动区抢输入），
  空态面板改为**在内容之后绘制**（盖在上面）。这样"空态 → 有内容"的常用路径能看到淡入，
  实测：媒体库网格 0 → 15.9 → 27.3（亮度）、搜索网格 0 → 9.9 → 16.9。
  仍不淡入的只剩"元素第一次绘制时数据就已经在"的路径（缓存秒开），此时本来也没有可过渡的画面。
- **顶栏激活标签的下划线指示条**：本顶栏是纯文字态（第 2 节），本来没有指示条，
  因此没有可加的 `x` / `width` 过渡。

### 9.6 工程约束（怎么保证不出 bug）

1. **无定时器 / 无轮询**：所有动效都是声明式 `animate`（或 Slint 库内的 flick 平滑滚动），
   没有 `slint::Timer`、没有 `animation-tick()` 每帧重绘。
2. **拖拽必须跟手**：行左右箭头没有给 `content-x` 加 `animate`——那会把用户拖拽/滚轮也套上
   缓动。改为 `Flickable.scroll-to(..., ScrollBehavior.smooth)`：只有这一次程序化定位走
   库内 spring 动画，用户手势仍逐帧跟手。
3. **透明层不吃点击**：首页 / 媒体库 / 搜索页的内容区常驻但 `interactive: !empty`
   （空数据时不接鼠标）；播放器控制层淡出用 `visible: controls-fade > 0.01`（淡完才收起）、
   放大卡信息面板 `visible: panel-reveal > 0.01`，归零后都不再参与命中测试；
   对话框关闭是 `visible` 立即收起（不做淡出，避免"半透明层还在拦截鼠标"）。
4. **入场动画的写法**：入口类动画用自定义 `float` 进度（默认 0 = 隐藏态）驱动
   opacity / 位移，而不是直接 `animate opacity`——`opacity` 默认值是 1，
   在"创建时不可见 / 还没求值过"的元素上会退化成 `1 → 1` 的空动画。
5. **快照如何避免拍到中间帧**：无头 testing backend 只认 `mock_elapsed_time` 推进时间，
   所以 `tests/snapshots.rs` 在 `take_snapshot()` 前推进时间（非播放页 1000ms、播放页 260ms，
   后者要小于播放器 3s 自动隐藏阈值）。这是"让动画结算"，不是为快照删动效。

### 9.7 快照变化（本次改动导致的 8 张，其余逐字节不变）

| 快照 | 差异区域 | 原因 |
|---|---|---|
| `library.png` / `netflix-library.png` | 第 6 列第 2 行的"无海报占位卡"（x 1053–1218, y 473–713） | 骨架底色改为官方 `#232323` + 0.75 不透明度（原来是 `surface-raised` 100%） |
| `search-results.png` | 同上区域（y 232–472） | 同上 |
| `search-active.png` | 顶栏搜索框（x 933–1165, y 18–53） | 之前搜索框宽度动画冻在起始帧（窄），现在结算到展开的 240px |
| `home-scrolled.png` | 顶部 0–127px 全宽 | 之前顶栏实底过渡冻在起始帧（透明），现在结算到 `Theme.surface` 实底 |
| `add-server-empty.png` / `add-server-error.png` | 禁用态的"连接"按钮（x 646–753, y 549–592）+ 光标列（x 472） | 禁用按钮底色改为官方 `Button-SurfaceDisabled`（不再用 opacity 0.5）；光标闪烁相位随推时变化 |
| `add-server-plaintext-warning.png` | 光标列（x 472，16 个像素） | 同上，纯光标相位 |

### 9.8 真机验证（`--screenshot`）

```bash
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 NOVA_DATA_DIR=$HOME/.local/share/nova \
  ./target/release/nova-desktop --window-size 1920x1080 --screenshot /tmp/a1.png --screenshot-delay 900
# 同命令再来一次 --screenshot-delay 4000 → 两张的 md5 不同、像素差非零
```

无头侧另有取证：用 testing backend 的 `mock_elapsed_time` 逐帧采样（临时测试，验证后删除），
实测到 卡片放大 `zoom` 宽度 327 →（400ms 延迟内不动）→ 362.3（460ms）→ 441.45（860ms）、
箭头悬停 40ms 内 2299 px 变化后收敛、详情页 backdrop 亮度 13.1 → 71.6 → 117.4（淡入）、
播放器遮罩亮度 4.17 → 1.60 → 0.00（淡出）。`--debug-socket` 的注释明确"不注入鼠标/键盘事件"，
所以**悬停类动效无法在真机脚本里触发**，只能靠上述无头逐帧采样 + 代码审查取证。
