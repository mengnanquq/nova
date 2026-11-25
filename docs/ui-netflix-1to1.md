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
| 正文列留白 | `content-gutter`：≥1920 → 200px、1280–1919 → 152px、<1280 → `4vw`（官方 `main` 48px + 内容 padding 104/152px，见 §10） |
| 装饰性左右留白 | `Theme.gutter`：`4vw`（1280 → 51px；≥1920 固定 60px）；现只用于空态面板宽度上限 |
| 顶栏高 | 72px；未滚动时 `linear-gradient(to top, rgba(0,0,0,0) 0%, rgb(0,0,0) 100%)`；logo 字高 20px，字距 -0.5px；链接 14px/400，间距 `1.5vw` |
| 头像 | 32px 圆，`#E50914` 底 + 白色首字母 14px bold |
| 搜索图标 | 24px 描边 2px；展开时输入框 240×36，左图标 20px |
| billboard 高 | 官方 `56.25vw`，用 `88vh` 封顶（非官方折中，见 §10.5）；文案块左 `4%`（≥1500px 固定 60px，官方 `.billboard .info{left:4%}`），底部距 hero 底 `18%` |
| billboard 标题 | 48px bold / 行高 1.05 / 字距 -0.02em；logo 图高 64–96px |
| billboard 元信息 | 14px / `#B3B3B3` / 行高 1.4 / 上距 12px |
| billboard 简介 | 16px / 白 / 行高 1.5 / 宽 560px / 最多 3 行 / 上距 16px |
| 按钮 | 高 44px，圆角 4，内边距 0 24px；主按钮白底黑字，次按钮 `#FFFFFF26` 白字；图标 22px + 间距 8px |
| 行标题 | 官方 Hawkins Text 三级断点：<960 20/500、960–1279 24/500、≥1280 32/700（`Theme.row-title-*`，1920 实测框高 46px = `section-title-height`），下方 12px；右侧「查看全部 ›」14px `#B3B3B3`，悬停白 |
| 行间距 | 40px；首行距 billboard 底 24px |
| 行内卡 | 横版 327×184（≥1280）；竖版按断点 150×210（≥960）/ 184×257.6（≥1280）/ 240×336（≥1920）；间距 `Theme.card-gap` 12px |
| 卡片圆角 | 竖版卡按官方三档：<960 → 8px、960–1919 → 12px、≥1920 → 16px（放大后统一收到 6px `radius-panel`） |
| 卡片放大 | `scale 1.35`（Netflix 观感），延迟 400ms 触发，过渡 `Theme.motion-slow`（300ms）ease-out；放大卡宽 = 卡宽×1.35 |
| 放大卡信息 | 标题 14px bold、元信息 12px `#B3B3B3`、迷你进度条 3px |
| 进度条 | 高 3px，`#E50914`，距卡底 6px，左右各 6px |
| 行左右箭头 | 官方 24×80 胶囊（`width:1.5rem;height:5rem;border-radius:16rem`）、底色 `rgba(128,128,128,.4)`，垂直居中于卡区，悬停行时出现 |

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
| 正文左右 padding | 200px（1920） | `Theme.content-gutter`（≥1920 → 200px、1280–1919 → 152px；原 `detail-gutter` 已并入，避免两套数字漂移） |
| 正文列宽 | 1520px（1920 − 200×2） | 同上 |
| section 间距 | 40px | `Theme.section-gap: 40px` |
| 区块标题 | 32px / 700 / 行高 46px / 下距 12px | `Theme.text-h4-size`(32px) + `Theme.text-h4-weight`(700)；媒体库标题用同一对，行标题用 `Theme.row-title-*` 三级断点 |
| 主标题 | 48px / 900（官方 `Text-TextFontWeight` 最大档是 900；48px 本身不在官方 10 档里，见 §10.3） | `Theme.font-48` + `Theme.weight-black`(900)；标题块高度**自适应**（`min-height: 56px` + 换行，不再 160px 固定高度） |
| 元信息 | 14px / 500 / 项间距 30px（`margin-left: 30px`）/ 分隔点 `rgba(255,255,255,.4)` | 字号 14px、字重 `Theme.weight-medium` 已对齐；**项间距与点色在数据层字符串内**（`detail_meta_line` 用 `·` 拼成一串），Slint 侧不能逐项调间距 / 逐字上色，`Theme.detail-meta-dot: #FFFFFF66` 先立 token |
| 简介 | 16px / 行高 24px / 宽 696.95px（≈697）/ 不限行数 | 宽 697px、`wrap: word-wrap`、去掉 `elide` 与 90px 固定高（Slint 无 `line-height`，行距用字体默认） |
| 集数（横版）卡 | 327×322（容器）；缩略图 307×173 | `Theme.card-landscape-w` = 327px（≥1280，含 1920；447px 只属于「预告」行，我们没有那种行）；`card-landscape-h = w × 9/16` |
| 集数卡外框 | `padding 8px` / `radius 16px` / `background rgba(255,255,255,.1)` / `border 2px solid rgba(255,255,255,.04)` | `Theme.card-frame-bg` / `card-frame-border` / `card-frame-padding` / `card-frame-border-width`；`poster_card.slint` 统一生效（首页行卡同步带上内衬，属预期） |
| 缩略图圆角 | 16px（1920；1280 为 12px，见 §10.1 官方三档） | `Theme.radius-poster`（断点值；图片内缩后自己带圆角） |
| 时长角标 | `rgba(0,0,0,.9)` 底 / `radius 4px` / `padding 1px 12px` / 13px | 现有 `Theme.badge-bg`（`#000000B3`）+ `radius-badge` 4px + 12px 字号；**未逐项对齐**（我们的角标在卡右上角，Netflix 在缩略图右下角） |
| 竖版（海报）卡 | 240×336（5:7；另有 184×257.6 @1280、150×210 @960） | `Theme.card-portrait-w/h`（高度由 `poster-ratio-w/h` = 5:7 推导，不写字面高度） |
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
| 顶部大搜索框 | 高 48px、字号 1.5rem/500（= 24px，官方 Hawkins `Text` 档，`Theme.text-h5-*`）、透明底 + 底部 1px 线；左右 `content-gutter`、`navbar-height` 之下留 40px | **推导**（登录墙后看不到）+ **官方字号档**（1.5rem/500 来自 `Text-TextFontSize`) |
| 结果头 | `「{term}」的搜索结果` + `共 {total} 部`（行标题断点 20/24/32 + 12px `text-muted`） | **推导**（文案与层级按 Netflix 的 "Search results for …" 结构）+ 字号复用 `Theme.row-title-*` |
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

## 10. 官方 CSS 真值对照表（尺寸 / 字号 / 留白 / 圆角）

口径：**只落"能从 Netflix 一手样式表原文里读到的值"**，逐条写「官方原文 → 我们的 token → 实际生效值」。
样式表来源：

| 文件 | 内容 |
|---|---|
| `/tmp/genre_inline.css`（418KB） | `netflix.com/tw/browse/genre/*` 页内联的 Hawkins CSS，含 2024 新版行 / 竖版卡的**全部响应式断点** |
| `/tmp/live_akira.css`（403KB） | `akiraClient.css` 会员端 CSS（`.billboard` / `.title-card` / `.slider` / `.lolomo` / `.previewModal`） |
| `/tmp/tw_inline.css`（319KB） | Hawkins 设计令牌全集（`--hcw--local-design--Button-*` / `Text-*` / `Input-*`） |

### 10.1 竖版海报三档 + 圆角

```css
/* /tmp/genre_inline.css（竖版卡容器，四档原文） */
@media all{.default-ltr-iqcdef-cache-1xpxmo3{width:8.25rem;height:11.55rem;border-radius:0.5rem;}}
@media all and (min-width: 960px){…{width:9.375rem;height:13.125rem;border-radius:0.75rem;}}
@media all and (min-width: 1280px){…{width:11.5rem;height:16.1rem;border-radius:0.75rem;}}
@media all and (min-width: 1920px){…{width:15rem;height:21rem;border-radius:1rem;}}
```

| 官方原文 | 我们 | 实际生效值 |
|---|---|---|
| `width:15rem;height:21rem;border-radius:1rem` | `card-portrait-w: 240px`；`card-portrait-h = w × poster-ratio-h/poster-ratio-w`（7/5） | ≥1920：**240×336，圆角 16px** |
| `width:11.5rem;height:16.1rem;border-radius:0.75rem` | `card-portrait-w: 184px` | 1280–1919：**184×257.6，圆角 12px** |
| `width:9.375rem;height:13.125rem;border-radius:0.75rem` | `card-portrait-w: 150px` | 960–1279：**150×210，圆角 12px** |
| `width:8.25rem;height:11.55rem;border-radius:0.5rem` | `<960` 保留原 `min(vw×0.16, 130px)`（宽度未跟官方 132px 走） | `<960`：≤130×182，圆角 **8px** |

三档官方尺寸**严格 5:7**（150/210 = 184/257.6 = 240/336 = 5/7），所以高度由
`Theme.poster-ratio-w: 5` / `poster-ratio-h: 7` 一对常量推导（`home.slint` 的行卡与
`poster_grid.slint` 的网格卡共用），**没有三组字面高度**。

**实测取证**（无头 testing 渲染，`billboard-ratio: 0.05` 让两行进首屏，用像素扫描量卡框）：

```text
1280×800 : 竖版行 [(152,335,184),(348,531,184),…]  高 258px（= 184×7/5 = 257.6）
1920×1080: 竖版行 [(200,439,240),(452,691,240),…]  高 336px（= 240×7/5）
横版卡两档都是 327×184（未改）
```

### 10.2 行标题三级断点

官方 Hawkins `Text-*` 令牌组合（字号 × 字重），实际渲染出的 h2 框高：

| 官方 | 我们 | 实际 |
|---|---|---|
| `<960`：`1.25rem` / 500 | `row-title-size-sm: 20px` / `row-title-weight-sm: 500` | 20px / 500 |
| `960–1279`：`1.5rem` / 500 | `row-title-size-md: 24px` / `row-title-weight-md: 500` | 24px / 500 |
| `≥1280`：`2rem` / 700（1920 实测框高 46、下距 12） | `row-title-size-lg: 32px` / `row-title-weight-lg: 700` | 32px / 700 |
| `Text-TextLineHeight: 125%` | `Theme.text-line-height: 1.25` → 行标题 `line-height-factor` | 行标题行高 125% |

- 解析值 `Theme.row-title-size/weight` 按断点选一档；`home.slint` 的 `CardRow` 标题与
  搜索结果头都用它（原来是写死的 `font-22` + bold）。
- `Theme.section-title-height`（行标题区高度）= `row-title-size + 14px` → 32px 档 **46px**
  （官方实测值），24px 档 38px、20px 档 34px（= 改动前的值）。标题下方 `space-3` 12px，
  即官方"h2 与 `ul` 之间 12px"。

### 10.3 排版梯度 token（官方 `Text` 10 档）

官方令牌全集（`/tmp/tw_inline.css`）：

```css
--hcw--local-design--Text-TextFontSize: 4rem / 3.5rem / 2.5rem / 2rem / 1.5rem /
                                       1.25rem / 1.125rem / 1rem / 0.875rem / 0.8125rem
--hcw--local-design--Text-TextFontWeight: 400 / 500 / 700 / 900
--hcw--local-design--Text-TextLineHeight: 125%
```

| token（`theme.slint`） | 字号 | 字重 | 官方档 |
|---|---|---|---|
| `text-h1-size/weight` | 64px | 900 | `4rem` / 900 |
| `text-h2-size/weight` | 56px | 900 | `3.5rem` / 900 |
| `text-h3-size/weight` | 40px | 700 | `2.5rem` / 700 |
| `text-h4-size/weight` | 32px | 700 | `2rem` / 700 |
| `text-h5-size/weight` | 24px | 500 | `1.5rem` / 500 |
| `text-h6-size/weight` | 20px | 500 | `1.25rem` / 500 |
| `text-body-lg-size/weight` | 18px | 500 | `1.125rem` / 500 |
| `text-body-size/weight` | 16px | 400 | `1rem` / 400 |
| `text-label-size/weight` | 14px | 400 | `0.875rem` / 400 |
| `text-caption-size/weight` | 13px | 400 | `0.8125rem` / 400 |

旧的 `font-12 … font-48` 全部保留（避免大面积改动），新代码优先用这 10 对。

已换到官方值的**明确几处**：

| 位置 | 改动 | 依据 |
|---|---|---|
| 详情页主标题 / 首页 hero 主标题 | 48px 保留，字重 `700 → 900`（`Theme.weight-black`） | 官方 `Text-TextFontWeight` 最大档是 900。**注意**：48px 不在官方 10 档内（官方 900 档是 4rem/3.5rem），所以"48px/900"是"我们的字号 + 官方字重"的组合，不是原文里的一行 |
| 媒体库区块标题 / 搜索结果头 | `font-34`(34/700) → `text-h4`(32/700)；搜索头 22/bold → `row-title-*` 断点 | 官方 `2rem`/700 档 |
| 搜索页大输入框（含占位） | `font-22`(22px) → `text-h5`(24/500)，`TextInput` 也带上 `font-weight: 500` | 官方 `1.5rem`/500 档 |

### 10.4 正文留白：`main 48px` + 内容 padding

```css
/* /tmp/genre_inline.css（页面级 padding） */
@media all{…{padding:0;}}
@media all and (min-width: 960px){…{padding:0 3rem;}}
@media all and (min-width: 1280px){…{padding:0 6.5rem;}}
@media all and (min-width: 1920px){…{padding:0 9.5rem;}}
```

| 官方 | 换算 | 我们的 `content-gutter` |
|---|---|---|
| `@1920: 0 9.5rem` | 152px + 外层 `main` 48px = **200px** | `viewport >= 1920px ? 200px` |
| `@1280: 0 6.5rem` | 104px + 48px = **152px** | `viewport >= 1280px ? 152px` |
| `<1280` | 官方 960–1279 是 48+48 = 96px、<960 是 48px | 保留 `4vw` 连续缩放（我们最小窗口 1280，见 10.8） |

- **`detail-gutter` 已删除**，详情页改用同一个 `content-gutter`；首页行 / 详情 / 媒体库 /
  搜索页 / `poster_grid` 五处引用的是同一个 token（`grep detail-gutter` 为空）。
- 连带影响（属预期）：媒体库与搜索页仍是 6 列，但列宽从 186px 降到 **152.67px**
  （`(1280 − 152×2 − 12×5) / 6`），卡高 213.7；快照 `library.png` / `search-results.png` 整片变化。
- `Theme.gutter`（4vw / ≥1920 60px）只留给"装饰性"用途（空态面板最大宽度）。

### 10.5 首页 billboard 高度

```css
/* /tmp/live_akira.css */
.billboard{background-color:#000;height:56.25vw;position:absolute;width:100%;z-index:0}
.billboard-row{background-color:#000;margin-bottom:20px;padding-bottom:40%;position:relative;…}
.billboard-row .billboard .info{bottom:35%;…;left:4%;position:absolute;…;width:36%}
@media screen and (min-width:1500px){.billboard-row .billboard .info{left:60px}}
```

| 官方 | 我们 | 实际生效值 |
|---|---|---|
| `.billboard{height:56.25vw}` | `Theme.billboard-height: viewport-width × 0.5625` | 主值 56.25vw |
| `.billboard-row{padding-bottom:40%}`（给内容流定高，图片绝对定位溢出） | **没复刻**：我们的 hero 是内容流里的一个块 | hero 高 = `min(56.25vw, 88vh)` |
| `.info{left:4%}` / `@1500: left:60px` | `Theme.billboard-left` | 1280 → 51.2px、1600/1920 → 60px |
| `.info{bottom:35%}` | 保留原 `0.82`（= 距底 18%） | **偏差**：文案块比官方低 17 个百分点（本章 10.8） |

**为什么 88vh 而不是任务书里的 92vh**：官方那 100vh 是**绝对定位的背景层**，内容流由
`billboard-row` 的 40% 高度决定，所以满屏 hero 不会挤掉行；我们的 hero 在流里，直接算：

```text
hero = 56.25vw(1280) = 720 → 行块起点 = (720−16) + 40 = 744 → 标题 744…790 → 卡顶 802 > 800
hero = 88vh  (1280) = 704 → 行块起点 = 688 + 40 = 728 → 标题 728…774 → 卡顶 786 < 800（露出 14px）
```

92vh 在 1280×800 下等于 736 > 720，**封不住**（卡顶仍在 802），所以取 88vh。
这也是改动前的旧值，因此 1280×800 / 1600×900 / 1920×1080 三档的 hero 高度与改动前**一致**
（704 / 792 / 950），只有宽高比 > 16:9 的超宽屏才会真正取到 56.25vw。

### 10.6 行内翻页箭头

```css
/* /tmp/genre_inline.css（行右缘的分页按钮） */
.handle{height:5rem;width:1.5rem;border-radius:16rem;border:none;…
        color:rgb(255,255,255);background-color:rgba(128,128,128,0.4);cursor:pointer;}
.handle:hover,.handle:focus-visible{background-color:rgba(128,128,128,0.2);color:rgb(255,255,255);}
/* /tmp/live_akira.css（会员端旧实现，两套来源都记下来） */
.slider .handle{background:hsla(0,0%,8%,.5);bottom:0;…;top:0;width:4%;}
.slider .handle{width:60px}                     /* ≥某断点 */
```

| 官方 | 我们 | 实际 |
|---|---|---|
| `width:1.5rem;height:5rem` | `Theme.arrow-width: 24px` / `arrow-height: 80px`（原 `arrow-size: 48px` 已删） | **24×80**，垂直居中于卡区 |
| `border-radius:16rem`（256px ≥ 半宽/半高） | `Theme.radius-pill` | 胶囊 |
| `background-color:rgba(128,128,128,0.4)` | `Theme.arrow-bg: #80808066` | 40% 灰 |
| `:hover{background-color:rgba(128,128,128,0.2)}` | **未启用**：沿用上一轮定稿的 `opacity → 0.85`（`motion-fast`） | hover 不变色（偏差，避免动本轮不该动的动效层） |
| （旧实现 `width:60px` + `hsla(0,0%,8%,.5)`） | 不采用 | — |
| 官方是 SVG chevron | 字体字形 `‹` / `›`；`arrow-glyph-size: 26px → 20px` | 20px 字形能完整放进 24px 宽（26px 会横向裁切） |

### 10.7 按钮 / 输入框

```css
/* /tmp/tw_inline.css */
--hcw--local-design--Button-BorderRadius:0.25rem;   /* 4px */
--hcw--local-design--Button-BorderWidth:0.0625rem;
--hcw--local-design--Input-BorderRadius:0.25rem;    /* 4px */
--hcw--local-design--Input-BorderWidth:0.0625rem;   /* 1px */
--hcw--local-design--Input-Border:rgba(128,128,128,0.7);
```

| 官方 | 我们 | 实际 |
|---|---|---|
| `Button-BorderRadius: 0.25rem` | 新增 `Theme.radius-button: 4px` | 首页 hero「播放 / 更多信息」、详情「播放 / 我的列表 / 返回 / 版本下拉」、媒体库排序按钮 → **4px**（这些原本就是 `radius-badge` 的 4px，现在换成语义 token，值不变） |
| 营销页 CTA `border-radius:100px` + 240×48 | 空态主/次按钮、添加服务器对话框的动作按钮保持 `radius-pill` | 胶囊（**两种官方来源并存**：会员端动作 4px、营销页 CTA 100px；这两处是"引导开始用"的 CTA，取后者） |
| `Input-BorderRadius: 0.25rem` | 新增 `Theme.radius-input: 4px` | `add_server.slint` 三个输入框 → **4px** |
| `Input-Border: rgba(128,128,128,0.7)` / `Input-BorderWidth: 0.0625rem` | `Theme.input-border: #808080B3` / `input-border-width: 1px` | 输入框描边 1px `rgba(128,128,128,.7)`（聚焦态仍是品牌红，沿用原交互） |

> **口径纠正**：任务书里写"官方 `Input-BorderRadius: 6.25rem`（胶囊）"，我在三个样式表里
> 逐字查过：`6.25rem` 只作为 `font-size`（`font-size:6.25rem`）与一个容器 `height:6.25rem`
> 出现，`Input-BorderRadius` 的令牌原文是 `0.25rem`。所以输入框落的是 **4px 方角**，
> 不是胶囊。颜色 `rgba(128,128,128,0.7)` 与任务书一致。

### 10.8 已知偏差（不要当成"和 Netflix 一致"）

| 项 | 官方 | 我们 | 理由 |
|---|---|---|---|
| 卡片悬停放大量 | `transform: scale(1.05)`；标题在卡片下方 | `zoom-factor: 1.35` + 56px 信息面板 | **保持不动**（本轮明确不改）：我们的标题/元信息在卡内面板里，1.05 会让面板内容挤爆；`zoom-allowance` / 覆盖层几何也全按 1.35 立的值 |
| billboard 高度 | `56.25vw` + `billboard-row{padding-bottom:40%}` | `min(56.25vw, 88vh)` | 见 10.5 的算术；内容流里放不下 100vh hero |
| billboard 文案块位置 | `.info{bottom:35%}` | `18%` | 未改（任务书要求"核对位置仍正确"而非改位置）；我们的 hero 变暗渐变从 40% 起，文案压在渐变里更好读 |
| hero 字号 | 900 档是 4rem / 3.5rem | 48px / 900 | 48px 是本仓库既有值；直接上 64px 中文标题在 1280 宽会破排 |
| 1280 以下的正文留白 | 960–1279 → 96px、<960 → 48px | `4vw`（1280 → 51px） | 我们的最小窗口就是 1280；避免 1279→1280 之间从 51px 跳到 152px 之外再多一个断点 |
| `<960` 竖版卡宽 | 8.25rem = 132px | `min(vw×0.16, 130px)` | 保留原值（本轮只按任务书改三档 + 圆角）；圆角已按官方 8px |
| 箭头 hover | 换底色 `rgba(128,128,128,.2)` | `opacity → 0.85` | 动效层上一轮刚定稿，本轮不动 |
| 行标题放大遮挡 | 非 1.35 场景 | 焦点在第 1 张卡时，1.35 倍放大卡（上移 12px）会压到行标题下沿 | **既有问题**，本轮标题框从 34 抬到 46 后重叠量从 44px 降到 32px（快照与真机截图可复现）；彻底修需要把标题画在覆盖层之上或收敛放大上移，属交互/层级改动，留作后续 |

### 10.9 快照变化（本轮 21 张，其余 8 张逐字节不变）

`cargo test --workspace` 重拍后与改动前逐像素比对（bbox 为差异外接框）。
其中 18 张来自 `tests/snapshots.rs`，另外 3 张 `add-server-*.png` 来自 `tests/add_server.rs`
（输入框圆角 / 描边改动）。

| 快照 | 差异区域 bbox | 差异像素 | 原因 |
|---|---|---|---|
| `add-server-empty.png` / `add-server-error.png` / `add-server-plaintext-warning.png` | (460,270)-(820,482) | 1 688 | 三个输入框：圆角 6 → 4px、描边 `#262626` → `rgba(128,128,128,.7)` |
| `home-filled.png` / `netflix-home.png` / `nf-home-1280.png` | (51,730)-(1280,800) | 53 759 | 行标题 22/700 → 32/700、title-off 46 → 58、行标题 x 51 → 152 |
| `home-scrolled.png` | (0,250)-(1280,800) | 447 931 | 同上 + 全部行/卡按 152px 列重排 |
| `netflix-card-focused.png` / `nf-card-hover.png` / `nf-row-hover.png` | (0,186)-(1280,800) | ≈53 万 | 同 `home` + 箭头 48×48 方角 → 24×80 胶囊 |
| `nf-home-1920.png` | 全图 | 1 912 505 | content-gutter 60 → 200、竖版卡 240×336、行标题 32/700、圆角 16 |
| `library.png` / `netflix-library.png` | (51,96)-(1229,724) | 644 368 | 列宽 186 → 152.67（gutter 51 → 152 的连带）、标题 34/700 → 32/700 |
| `search-results.png` | (19,125)-(1229,630) | 337 498 | 同上 + 结果头改行标题断点 + 输入框 22 → 24/500 |
| `search-active.png` | (51,18)-(1280,800) | 53 931 | 顶部输入框字号 22 → 24/500、列宽变化 |
| `search-empty.png` | (51,125)-(1229,160) | 2 837 | 搜索框字号（空态面板本身不变） |
| `detail.png` / `netflix-detail.png` | (47,16)-(845,800) | 54 054 | 正文列 51 → 152、标题字重 700 → 900 |
| `banner-warning.png` | (52,778)-(1227,800) | 12 742 | 底部露出的行标题放大 |
| `player-audio-menu.png` / `player-subtitle-menu.png` | (917,303)-(1221,548) | 172 | **连带**：播放器控制层里两处借用 `radius-poster` 的元素（时间按钮 / 菜单行）在 1280 下圆角 16 → 12，只有四角像素变 |

`home-empty.png` / `player*.png`（除上面两张菜单图）等 8 张逐字节不变。

### 10.10 真机验证（三种视口）

```bash
for s in 1280x800 1600x900 1920x1080; do w=${s%x*};
  WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 NOVA_DATA_DIR=$HOME/.local/share/nova \
    ./target/release/nova-desktop --window-size $s --screenshot /tmp/bp-$w.png --screenshot-delay 3000
done
magick identify /tmp/bp-1280.png /tmp/bp-1600.png /tmp/bp-1920.png
```

| 视口 | 截图 | `content-gutter` | 观察到的断点生效 |
|---|---|---|---|
| 1280×800 | `/tmp/bp-1280.png`（1280×800） | 152px | hero 文案 `left:4%` = 51px、行标题/卡片从 152 起、32/700 标题、箭头 24×80 |
| 1600×900 | `/tmp/bp-1600.png`（1600×900） | 152px | hero 文案 `left:60px`（≥1500 档）、行从 152 起 |
| 1920×1080 | `/tmp/bp-1920.png`（1920×1080） | 200px | 行/卡从 200 起、竖版卡 240×336、圆角 16px |

海报卡尺寸是从**无头快照**上量出来的（真机首屏只有横版行，竖版行在折叠下方）：
1920 竖版行像素扫描 `[(200,439,240),(452,691,240),…]`、高 336；1280 竖版行
`[(152,335,184),(348,531,184),…]`、高 258（= 257.6 光栅化）。两组都是严格 5:7。
