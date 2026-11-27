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
| 行标题 | 官方 Hawkins Text 三级断点：<960 20/500、960–1279 24/500、≥1280 32/700（`Theme.row-title-*`，1920 实测框高 46px = `section-title-height`），下方 12px；右侧「查看全部 ›」14px `#B3B3B3`，悬停白。**「继续观看」「最近添加」是跨库聚合行，没有对应的库页，因此不显示「查看全部 ›」**（只显示行标题）；有库 id 的库行才显示，点击进对应媒体库标签页，且只有这些行会登记 `*.more` 调试目标 |
| 行间距 | 40px；首行距 billboard 底 24px |
| 行内卡 | 横版图区 327×184（≥1280）；竖版图区按断点 150×210（≥960）/ 184×257.6（≥1280）/ 240×336（≥1920）；间距 `Theme.card-gap` 12px。**卡片总高 = 图区高 + 卡下标题块**（见下行） |
| 卡下标题 | 官方卡片结构是「海报在上、一行标题在**卡片下方**」：`p{color:#fff;font-weight:400;text-align:center}`，`margin-top` 4px（1280–1919）/ 8px（≥1920）、`line-height` 21px / 24px、字号 14px / 16px → 标题块高 25px / **32px**。竖向合计（≥1920）= 10.4 + 336 + 32 + 10.4 = **388.8** = 官方 `li` 实测高（`Theme.card-caption-*`） |
| 卡片圆角 | 竖版卡按官方三档：<960 → 8px、960–1919 → 12px、≥1920 → 16px；放大卡沿用同一圆角（不再改成 6px：官方放大只是 `scale`，不改圆角） |
| 卡片放大 | **官方 `scale(1.05)`**（原为 1.35），延迟 400ms 触发，过渡 `Theme.motion-zoom`（200ms）`ease-in-out`，**无位移**、**无悬停信息面板**；放大镜像 = 图区宽 ×1.05、卡片总高 ×1.05 |
| 放大余量 | 1.05 在官方 240×336 上多 12×16.8px；官方兜住它的就是 `li` 的 padding（左右 6px、上下 10.4px）。我们落成 `zoom-allowance-x: 12px` / `zoom-allowance-y: 20px`（原 1.35 面板时代是 `zoom-allowance: 96px`），并当成行距 / 卡间距的下限：放大卡每边只溢出 6px / 10px，12px 的间距够用 |
| 进度条 | 高 3px，`#E50914`，距卡底 6px，左右各 6px |
| 行左右箭头 | 官方 24×80 胶囊（`width:1.5rem;height:5rem;border-radius:16rem`）、底色 `rgba(128,128,128,.4)`，垂直居中于卡区；行被点亮（悬停放大 / 悬停在箭头上 / 键盘焦点行）**且该方向还能滚**时出现（行起点无左箭头、行尾无右箭头，R8-C3） |

## 2. 顶栏行为

1. 首屏：透明，字白，浮在 billboard 上。
2 滚动 > 40px：背景渐变到 `#141414`，180ms；同时出现一层底部 `1px #2A2A2A` 分隔（极淡）。
3. 悬停链接：白 + `opacity 1`；未选中 `#B3B3B3`。
4. 搜索：点图标 → 输入框从 24px 宽展开到 240px（180ms），并自动聚焦；`Esc` 收起。
5. 右到左顺序：搜索、头像（Netflix 还有通知铃，**不做**：我们没有通知系统）。

## 3. 行与卡片交互

1. 默认卡：海报 + 卡下标题，**无边框无阴影**。官方两种卡要分开：
   - 竖版海报卡（媒体库 / 搜索结果）：`240×336` 的图本体就是 `240×336`，**没有内衬**，
     圆角加在图上 → `PosterCard { framed: false }`（默认）；
   - 横版集数卡（首页行）：`li 327×322` 带 `padding:8px` + `rgba(255,255,255,.1)` 底 +
     `2px rgba(255,255,255,.04)` 描边 → 缩略图 307×173 → `framed: true`。
   一开始把集数卡的外框套到所有卡上，导致 240 宽的竖版卡里图只剩 220×316，已改掉。
2. 悬停/聚焦 400ms 后放大到 **1.05**（官方 `transform:scale(1.05)`），**不上移、不展开信息面板**——官方 `li` 里只有海报 + 一行标题。
3. 放大卡要在**相邻卡之上**绘制（覆盖层），且不参与行布局（零跳动）；覆盖卡每边只溢出 6px / 10px，而卡间距与标题下距都是 12px，所以**永远不会盖住行标题**（旧 1.35 + 56px 面板会，已修）。
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

> **「播放」/「更多信息」双按钮因未登录无法实测**（该区域被登录引导替代），
> 所以按钮尺寸**沿用我们现有值**（高 44px、白底/半透明白底）。
> 播放器的**控件布局与像素也无法实测**，但播放器**样式表（CSSOM）原文**可以拿到 ——
> 播放器控件本轮已按官方 em 比例重做（基数由我们选定），见 **§11 播放器**；
> 本节只保留 hero 内圆钮与旧口径的历史记录。

| 项 | 实测值 | 落在代码哪里 |
|---|---|---|
| 顶栏 | `height: 72px` / `z-index: 20` / `background-image: linear-gradient(to top, rgba(0,0,0,0) 0%, rgb(0,0,0) 100%)` | `Theme.navbar-height: 72px`；未滚动态用 `Theme.navbar-scroll-gradient`（`180deg` + `#000000 → #00000000`，即 CSS `to top` 的反写）再向下延伸 `Theme.navbar-gradient-extend: 56px`；滚动后仍是 `Theme.surface` 实底 |
| 播放器圆钮（hero 内视频控制钮实测，**旧口径**） | 40×40 / `border-radius: 20px`（正圆）/ `background: rgba(22,22,22,.7)` / `border: 1px solid rgba(128,128,128,.7)` / 内图标 24×24 / 两钮间距 12px | **已按 §11 改掉**：同一组 token 现在装的是官方全屏播放器的值（`Theme.player-control-size` = 3.2222em、`Theme.player-icon-bg: #414141D9`、`Theme.player-icon-border` = 3px transparent、图标改 em 比例，`player-icon-glyph` 已删除）。hero 里若还要 40×40 圆钮请另开 token |
| 进度条交互（播放器样式表 CSSOM 原文，未渲染验证） | `.nfp .scrubber-container{height:3.3em}`；`.track{background:#5b5b5b;height:.5em;transition:height .2s ease}`；`.scrubber-head{2.3em×2.3em;background:#e50914;border-radius:50%;opacity:1}`；`:hover .track / .is-scrubbing .track{height:1em}`；`.scrubber-head:hover,.is-scrubbing .scrubber-head{transform:scale(1.2)}` | **已按官方重做**：`Theme.player-hit-height`＝3.3em（52.8px）、`player-bar-height`＝8px → `player-bar-height-hover`＝16px、`player-bar-knob`＝36.8px × `player-bar-knob-scale` 1.2、轨道 `#5B5B5B` / 缓冲 `#FFFFFF33` / 已播 `#E50914`；过渡 `Theme.motion-zoom`(200ms)。详见 §11.3 / §11.5 |
| 页面底色 | `background-color: rgb(15,15,15)` | `Theme.detail-bg`（**不动** `Theme.bg`，其它页仍然纯黑） |
| hero 高度 | 845px / 视口 1080px = 0.782 | `Theme.detail-backdrop-ratio: 0.78` |
| hero 底部渐变 | `linear-gradient(rgba(0,0,0,0) 40%, rgba(0,0,0,.6) 100%)`，占 hero 高 60% | `detail.slint` 底部层：高 `backdrop-height * 0.6`、`@linear-gradient(180deg, #00000000 40%, #00000099 100%)` |
| hero 顶部压暗 | 浅黑渐变压住顶栏 | 顶部层高 `backdrop-height * 0.15`，`Theme.scrim-soft` → 透明 |
| 正文左右 padding | 200px（1920） | `Theme.content-gutter`（≥1920 → 200px、1280–1919 → 152px；原 `detail-gutter` 已并入，避免两套数字漂移） |
| 内容块位置 | 内容块 `rect(136, 424.1, 1648×440)`，hero `y 131..976`（高 844.88）→ 顶端在 hero 的 **34.7%**，底端距 hero 下缘 **111.9 ≈ 112px** | `Theme.detail-body-bottom-gap: 112px`；`detail.slint` 里单一 `body := VerticalLayout` 用 `y: max(0px, backdrop-height − detail-body-bottom-gap − self.height)` 反推顶端（**已完成**，见备注 5） |
| 正文列宽 | 1520px（1920 − 200×2） | 同上 |
| section 间距 | 40px | `Theme.section-gap: 40px` |
| 区块标题 | 32px / 700 / 行高 46px / 下距 12px | `Theme.text-h4-size`(32px) + `Theme.text-h4-weight`(700)；媒体库标题用同一对，行标题用 `Theme.row-title-*` 三级断点 |
| 主标题 | 48px / 900（官方 `Text-TextFontWeight` 最大档是 900；48px 本身不在官方 10 档里，见 §10.3） | `Theme.font-48` + `Theme.weight-black`(900)；标题块高度**自适应**（`min-height: 56px` + 换行，不再 160px 固定高度） |
| 元信息 | 14px / 500 / 项间距 30px（`margin-left: 30px`）/ 分隔点 `rgba(255,255,255,.4)` | 字号 14px、字重 `Theme.weight-medium` 已对齐；**项间距与点色在数据层字符串内**（`detail_meta_line` 用 `·` 拼成一串），Slint 侧不能逐项调间距 / 逐字上色，`Theme.detail-meta-dot: #FFFFFF66` 先立 token |
| 简介 | 16px / 行高 24px / 宽 696.95px（≈697）/ 不限行数 | 宽 697px、`wrap: word-wrap`、去掉 `elide` 与 90px 固定高（Slint 无 `line-height`，行距用字体默认） |
| 集数（横版）卡 | 327×322（容器）；缩略图 307×173 | `Theme.card-landscape-w` = 327px（≥1280，含 1920；447px 只属于「预告」行，我们没有那种行）；`card-landscape-h = w × 9/16` |
| 集数卡外框 | `padding 8px` / `radius 16px` / `background rgba(255,255,255,.1)` / `border 2px solid rgba(255,255,255,.04)` | `Theme.card-frame-*`；**只对横版集数卡生效**（`PosterCard.framed: true`，首页行用）。竖版海报卡 `framed: false`，图占满 240×336 |
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
5. **内容块已叠在 hero 下半部**（2026 本轮修完，之前是「落在 hero 下方」的未完成项）：
   标题 / 原文名 / 元信息 / 简介 / 按钮行 / 版本行合并成**一个** `body := VerticalLayout`，
   子项**一律不写 `y`**，位置只由顺序 + `spacing`（`Theme.space-4`）决定；容器顶端由
   「底端锚定 112px + 自身高度反推」得到。

   **踩过的坑（不要再犯）**：旧实现是标题块与简介块**各自绝对定位**——标题块
   `alignment: end` 贴在 `backdrop-height` 处、简介块 `y: backdrop-height + space-5`。
   两块互不知情，所以「只把简介块上移」的结果是**标题压住简介**：看起来像布局算错，
   实际是两个独立 y 绑定的必然结果。合并成一个纵向布局后，任何一块的高度变化都会把
   下面的块推开，物理上不可能重叠（回归断言：`crates/nova-ui/tests/detail_layout.rs`）。

   另一个坑：Slint 里**布局元素的默认高度是父元素的 100%**（不是内容高），直接写
   `y: max(0px, … − self.height)` 会撞上 `content.height → body.height → body.y →
   content.height` 的编译期 binding loop，而且多出来的高度会被可拉伸的 `Text` 子项
   吃掉（实测原文名 / 元信息各被撑到 269px）。所以必须显式 `height: self.preferred-height`
   + `alignment: start`。

**仍然无法实测的部分**（不要当成事实）：

- 播放器的**完整控件布局**（传输行顺序、间距、左右分组）需要登录才能拿到；CSS 里只有各行的
  padding/margin，没有按钮顺序。见 §11.8。
- 播放器**中央大钮尺寸**（现用 `Theme.player-play-size` = 3.6em = 57.6px、中央覆盖图标圈
  `Theme.player-icon-size` = 5.5em = 88px）：官方没有“中央大钮”这条规则，这两个数是我们自定的（§11.6）。
- 进度条**轨道静息高度**其实官方给了：`.track{height:.5em}` = 8px（旧版写的“官方只给悬停态 1em”是错的，
  已在 §11.2 更正）。

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
| 卡下标题 | 常驻（**没有**出现/消失动画） | — | — | **官方 CSS**：标题是 `li` 里的静态 `p`，不在悬停态里 |
| 按钮悬停（主 CTA / 次按钮 / 清空搜索 / 播放器圆钮 / 重试 / 添加服务器） | 悬停 | `motion-button`（250ms） | `cubic-bezier(0.32, 0.94, 0.6, 1)` | **官方 Hawkins 令牌** |
| 按钮按下 | `TouchArea.pressed` | `motion-press`（0ms） | `cubic-bezier(0.4, 0, 0.68, 0.06)` | **官方 Hawkins 令牌**（底色用 `Button-SurfacePressed`，见 9.4） |
| 按钮三态底色 | 常态 / 悬停 / 按下 / 禁用 | — | — | **官方 Hawkins 令牌**：`#E50914 / #C11119 / #99161D / rgba(229,9,20,.4)` 落在 `Theme.accent*` |
| 行入场（淡入 + 6px 上浮） | 数据空 → 非空 | `motion-row`（540ms） | `cubic-bezier(0.5, 0, 0.1, 1)` | 时长/曲线来自**官方 CSS**（那行是 `.lolomoRow` 的"行展开"），入场是我们借用 |
| 首页 billboard 文案块入场 | 同上 | `motion-row` | 同上 | 同上 |
| 网格入场（媒体库 / 搜索结果） | 数据空 → 非空 | `motion-slow` | `ease-out` | 我们自定（触发时机见 9.5） |
| 详情页 backdrop 淡入 + 内容块 +8px 归位 | 数据到达 | `motion-slow` | `ease-out` | 我们自定（内容块是单一纵向布局，见 §7 备注 5） |
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

口径提示：卡片放大就是官方 `scale(1.05)`（`Theme.zoom-factor: 1.05`），**没有**悬停信息面板：
官方 `li` 的结构是「海报 + 一行卡下标题」，我们的 `PosterCard` 已改成同一结构（`zoom-allowance-*`
与行高/网格行距也按 1.05 重算，原来的 96px 面板预留已删）。
另外官方确认的布局事实：行标题 ≥1280 是 32px/700（960–1279 是 24px/500，
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
   放大卡覆盖层 `interactive` 在放大走完 80% 后才接管，之前不参与命中测试；
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
| 卡片结构 | `li` = 海报 + 一行卡下标题（`p`，`text-align:center`），标题常驻 | `PosterCard` 同构：海报 + 卡下标题（`Theme.card-caption-*`） | 已改成官方结构：原来的「1.35 + 悬停信息面板」为保证标题/元信息可见而存在，现在标题常驻卡下，**面板整块删掉**，放大收敛到官方 1.05 |
| 卡下标题的换行 | 官方 `-webkit-line-clamp:3` + `overflow:hidden`（但 ≥1920 块高 32 = 8 + 24，实际只放得下一行） | 单行 + `overflow: elide` | Slint 没有 line-clamp；标题块高严格按官方（25 / 32px），所以单行就是官方在各断点下的可见结果 |
| 放大时哪些东西缩放 | 官方 `transform:scale(1.05)` 加在海报的 anchor 上（卡下标题是它之外的元素，字不缩、位置不动） | 覆盖层把整张卡（海报 + 标题）等比缩 1.05 | Slint 1.19 的 `Transform` 是 internal，只能靠"重算尺寸的覆盖层"模拟；好处是遮罩干净（不会出现两份标题）、代价是标题也跟着缩 5% 并下移 ≤9px |
| billboard 高度 | `56.25vw` + `billboard-row{padding-bottom:40%}` | `min(56.25vw, 88vh)` | 见 10.5 的算术；内容流里放不下 100vh hero |
| billboard 文案块位置 | `.info{bottom:35%}` | `18%` | 未改（任务书要求"核对位置仍正确"而非改位置）；我们的 hero 变暗渐变从 40% 起，文案压在渐变里更好读 |
| hero 字号 | 900 档是 4rem / 3.5rem | 48px / 900 | 48px 是本仓库既有值；直接上 64px 中文标题在 1280 宽会破排 |
| 1280 以下的正文留白 | 960–1279 → 96px、<960 → 48px | `4vw`（1280 → 51px） | 我们的最小窗口就是 1280；避免 1279→1280 之间从 51px 跳到 152px 之外再多一个断点 |
| `<960` 竖版卡宽 | 8.25rem = 132px | `min(vw×0.16, 130px)` | 保留原值（本轮只按任务书改三档 + 圆角）；圆角已按官方 8px |
| 箭头 hover | 换底色 `rgba(128,128,128,.2)` | `opacity → 0.85` | 动效层上一轮刚定稿，不动 |
| 详情页内容块顶端位置（占 hero 百分比） | 顶端在 hero 的 34.7%（内容块高 440） | 底端锚定 112px，顶端占 hero 33.8%（1280）/ 51%（1920，有简介） | 我们内容块只有 282–301px 高（没有徽章 / 匹配度等区），底端对齐后顶端就偏低；要改就补内容，不是调 y（见 10.12） |
| 详情页标题 / 原文名 / 元信息之间的间距 | 官方各块 gap 未逐项实测 | 都是 `Theme.space-4` 16px（旧实现标题块内是 4px） | 单一纵向布局只有一个 `spacing`；分别为标题 / 元信息单独调间距必须写 `y` 或嵌子布局，会重新引入“分块定位”的坑（见 §7 备注 5） |

> **已修**：旧版「行标题被放大卡遮住」的偏差（1.35 倍 + 上移 12px 会压进行标题 32px）
> 在改成官方 1.05 + 无位移后自然消失：放大卡上沿距行标题下沿实测仍有 6.8px（1280）/ 6.6px（1920），
> 断言在 `crates/nova-ui/tests/card_zoom_layout.rs`（`zy >= title_bottom` + 逐像素零跳动）。

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

### 10.11 卡片结构改成官方「海报 + 卡下标题」、放大收敛到 1.05

本轮把卡片从"1.35 放大 + 悬停信息面板"改成 **Netflix 官方结构**：标题**常驻在卡片下方**，
悬停只放大 `scale(1.05)`，官方没有悬停信息面板（`/tmp/genre_inline.css` 的 `li` 里只有海报 + 一行 `p`）。

| 项 | 官方原文（`/tmp/genre_inline.css`） | 我们 | 说明 |
|---|---|---|---|
| 放大 | `.…-dx6epf{transition:transform 0.2s ease-in-out}` / `:hover{transform:scale(1.05);z-index:2}` | `Theme.zoom-factor: 1.05`、`motion-zoom` 200ms `ease-in-out`、延迟 400ms | 上移 12px 与 56px 面板**整块删除**（`zoom-rise` / `zoom-info-height` / `zoom-panel-bg` 三个 token 一并删） |
| 卡下标题 | `.…-xyhsll{color:#fff;text-align:center;font-weight:400}`；`≥960 → 14px/21px`；`≥1280 → margin-top:4px`；`≥1920 → margin-top:8px / 16px / 24px` | `Theme.card-caption-size`(14/16)、`card-caption-line-height`(21/24)、`card-caption-gap`(4/8)、`card-caption-height`(25/32) | Slint 没有 line-clamp，用单行 `overflow: elide`（官方块高只放得下一行） |
| 卡片总高 | `li` = 10.4 + 336 + 32 + 10.4 = 388.8 | 图高 336 + 标题块 32 = **368**（不含 `li` 的上下 padding，由行距承担） | `li` 的 padding 在 Slint 侧表现为"行距"，见下一行 |
| 放大余量 | `li` padding `0.65rem 0.375rem`（左右 6px、上下 10.4px） | `zoom-allowance-x: 12px` / `zoom-allowance-y: 20px`（原 `zoom-allowance: 96px` 删除） | 1.05 在 240×336 上多出 12×16.8px，正是官方 padding 兜住的量；我们把它当**卡间距 / 行距 / 标题下距的下限**（`max()` 里都是 12px，实际不改变这些间距） |
| 行高 | 内容撑开（`li` 高 388.78） | `行标题 46 + 标题下距 12 + 卡片总高`（`CardRow`/`HomePage` 两处同式） | 放大卡向上只溢 ½×5%×卡片总高 ≈ 10px < 12px，所以**不可能再盖住行标题** |
| 网格行高 | 类型页网格没有单独行距（样式里没有 `rowContainer`/`lolomoRow` 的 gap） | 卡片总高 + 12px（`Theme.card-gap`） | 12px ≥ 放大卡竖向溢出 9.1px，行与行之间不会互相压到 |

实测（无头 testing，`/tmp/snap-after/` + 探针属性）：

| 视口 | 图高 | 标题块 | 卡片总高 | 放大后覆盖层 | 放大卡上沿 − 行标题下沿 |
|---|---|---|---|---|---|
| 1280×800（横版继续观看行） | 183.94 | 25 | 208.94 | 343.35 × 219.38（= 1.05×） | **+6.78px**（不相交） |
| 1920×1080（横版继续观看行） | 183.94 | 32 | 215.94 | 343.35 × 226.73（= 1.05×） | **+6.60px**（不相交） |
| 1920×1080（竖版最近添加行） | 336 | 32 | **368**（+ li padding 20.8 = 388.8 = 官方） | — | — |

快照变化（13 张，16 张逐字节不变）：`home-filled` / `netflix-home` / `nf-home-1280` /
`search-active` / `banner-warning`（首屏底部行）、`home-scrolled`（整行）、
`netflix-card-focused` / `nf-card-hover` / `nf-row-hover`（放大 + 标题）、
`library` / `netflix-library` / `search-results`（网格出现卡下标题、行高变了）、
`nf-home-1920`（同上，1920 档）。

### 10.12 详情页内容块叠在 hero 下半部（单一纵向布局 + 底端锚定 112px）

本轮把详情页的内容块从「落在 hero 下方」改成 **叠在 hero 下半部**，与 Netflix 官方结构一致。
（§7 备注 5 里那条"未完成"的待办已改为已完成，做法与踩过的坑都记在那里。）

| 项 | 官方实测（1920×1080） | 我们 | 说明 |
|---|---|---|---|
| hero | `y 131..976`，高 844.88 | `Theme.detail-backdrop-ratio: 0.78`（1080 → 842.4） | 已有 |
| 内容块 | `rect(136, 424.1, 1648×440)`：顶端在 hero 的 **34.7%** | 单一 `body := VerticalLayout`，**底端**锚定 | 高度不同，顶端百分比随之变化（见下面的实测表） |
| 内容块底端 | 距 hero 下缘 111.9px | `Theme.detail-body-bottom-gap: 112px` | 新增 token |
| 内容列左边距 | 200（`main 48` + `padding 152`） | `Theme.content-gutter` | 已有 |
| 子项顺序 | 标题 → 原文名 → 元信息 → 简介 → 按钮 → 版本 | 同（**子项一律不写 `y`**） | 位置只由顺序 + `spacing` 决定 |
| 子项间距 | 各块之间未逐项实测 | `Theme.space-4`（16px，沿用旧 `lower` 的 spacing） | 标题 / 原文名 / 元信息原来是 4px，现在同为 16px |

做法（`crates/nova-ui/ui/detail.slint`）：

- `body := VerticalLayout { x: content-gutter; width: 父宽 − 2×gutter; spacing: space-4; alignment: start; }`，
  六个子项按顺序排，**一个 `y` 都不写**；
- `height: self.preferred-height`（不写会被默认成父高的 100%，见 §7 备注 5 的 binding loop）；
- `y: max(0px, root.backdrop-height - Theme.detail-body-bottom-gap - self.height)
  + Theme.motion-rise-hero * (1 - root.reveal)`——末项是入场 8px 上浮，`reveal` 结算后为 0；
- 页面总高 `content.height = max(parent.height, body.y + body.height + Theme.space-8)`
  （底部留白 48px；官方未测，沿用现有 token）；
- **返回按钮绘制在 body 之后**（浮在内容上，`y: space-4` 固定），内容块再高也不会被盖住；
  官方也是返回钮浮在 hero 之上，所以选"改绘制顺序"而不是"给 body 加 clip"。

实测（探针 `AppWindow.detail-body-y` / `detail-body-height` / `detail-backdrop-height`）：

| 视口 | backdrop 高 | `body.y` | `body.height` | `y + h + 112` | 差 | 顶端占 hero |
|---|---|---|---|---|---|---|
| 1280×800（有简介，无头 fixture） | 624 | 211 | 301 | 624 | **0.0** | **33.8%** |
| 1920×1080（有简介，无头 fixture） | 842.4 | 429.4 | 301 | 842.4 | **0.0** | **51.0%** |
| 1280×800（真实条目「暗流」，简介为空） | 624 | 230 | 282 | 624 | **0.0** | 36.9% |
| 1920×1080（同上） | 842.4 | 448.4 | 282 | 842.4 | **0.0** | 53.2% |

为什么顶端百分比不是官方那个 34.7%：官方内容块高 440px（含我们没做的徽章 / 匹配度 /
更多元信息行），我们只有 282–301px，底端锚定就只能把顶端推下去。1280 + 有简介时
33.8% ≈ 官方；1920 视口更高而内容不再变高，百分比自然变大。要贴近官方得先把**内容做高**
（补区块），不是再回去调 y。

真机验证（`--debug-socket /tmp/nf.sock` + `{"cmd":"open","item":"324029"}` + `screenshot`，
调试通道的 `view` 现在也带 `detail_geometry` 探针）：

| 视口 | 截图 | backdrop 高 | `body.y` | `body.height` | `y + h + 112 − backdrop` | 标题字形顶 y | 标题↔简介字形间隙 |
|---|---|---|---|---|---|---|---|
| 1280×800 | `/tmp/detail-1280.png` | 624 | 230 | 282 | **0.0** | 231（< 624 ✓ 在 hero 内） | 378 − 280 = **98px** |
| 1920×1080 | `/tmp/detail-1920.png` | 842.4 | 448.4 | 282 | **0.0** | 455（< 842.4 ✓ 在 hero 内） | 597 − 498 = **99px** |

快照变化（2 张，其余 27 张逐字节不变）：`detail.png` / `netflix-detail.png`，
bbox `(148, 217)-(845, 800)`、76 107 像素——内容块从 hero 下方（y ≈ 648）整体上移到
hero 内（y = 211），标题 / 原文名 / 元信息 / 简介 / 按钮 / 版本行全部跟着上移，
backdrop 与渐变层逐像素不变。

回归断言：`crates/nova-ui/tests/detail_layout.rs`（body 底端 + 112 = hero 下缘 ±2px、
`body.y > 0`、标题底 ≤ 简介顶 ≤ 简介底 ≤ 按钮顶、内容比 hero 高时兜底到 0）。

## 11. 播放器

### 11.1 口径与前提（先读这一段）

一手素材：`/tmp/live_akira.css`（会员端 403KB 样式表，播放器的全部规则都在里面）。
**但播放器没法实测像素**：`/watch/<id>` 未登录 302 回登录页；免登录的详情页里播放器又被
登录引导替代。所以本节与其它节的口径不同：

- **比例 100% 来自 CSS 原文**（下面 11.2 / 11.4 是原样摘录）；
- **基数由我们选定**：`Theme.player-unit: 16px`，所有播放器尺寸 = `player-unit × 官方 em`；
- **像素是我们算的 / 在我们自己的窗口里量的**，不是量 Netflix 的播放器。

官方基数本身就有三条互相矛盾的线索（所以“官方到底几 px”本来就没有唯一答案）：

```
.NFPlayer.container-large{font-size:24px}
.NFPlayer.container-small{font-size:18px}
html.watch-video-root{font-size:10px!important}            /* 1920×1080 命中这一档 */
@media only screen and (max-width:1280px){html.watch-video-root{font-size:9px!important}}
@media only screen and (max-height:720px){html.watch-video-root{font-size:9px!important}}
@media only screen and (max-width:1152px){html.watch-video-root{font-size:8px!important}}
@media only screen and (max-width:1024px){html.watch-video-root{font-size:7.2px!important}}
@media only screen and (max-width:960px){html.watch-video-root{font-size:6.6px!important}}
@media only screen and (max-width:854px){html.watch-video-root{font-size:6px!important}}
@media only screen and (max-width:640px){html.watch-video-root{font-size:5.3px!important}}
@media only screen and (max-width:426px){html.watch-video-root{font-size:4.8px!important}}
```

`container-*` 是 loading / postplay 小窗那层的字号，`watch-video-root` 才是全屏页面的 html 根。
**我们选了 16px**：介于 18 / 24 之间，落在桌面窗口上看着不憋屈；比 `watch-video-root` 的
10px 大一档（同样的 em 比例会比 Netflix 的播放器整体大 1.6 倍——这是基数的差，不是比例的差）。
这条差异是本节最重要的“不要当成一致”的地方。

### 11.2 官方原文摘录（逐条，未改写）

容器 / 全屏：

```
.NFPlayer{height:100%;transition:width .3s,height .3s,top .3s,left .3s,opacity .3s;width:100%}
.NFPlayer.nf-player-container{background:#000;bottom:0;left:0;overflow:hidden;position:absolute;
  right:0;top:0;user-select:none;z-index:1}
.NFPlayer.nf-player-container.active{cursor:default}
.NFPlayer.nf-player-container.inactive{cursor:none}
.watch-video{box-sizing:border-box;height:100%;left:0;margin:0;overflow:hidden;padding:0;
  position:absolute;top:0;width:100%}
```

进度条（`/tmp/live_akira.css`，`.nfp` 作用域；**这是本轮重做的核心**）：

```
.nfp .scrubber-container{align-items:center;cursor:pointer;display:flex;height:3.3em;
  position:relative;touch-action:none;width:100%}
.nfp .scrubber-container .scrubber-bar{flex-grow:1;flex-shrink:0;position:relative;width:auto}
.nfp .scrubber-container .track{background:#5b5b5b;height:.5em;position:relative;
  transition:height .2s ease;will-change:height}
.nfp .scrubber-container .buffered,
.nfp .scrubber-container .current-progress{bottom:0;display:flex;height:100%;left:0;
  position:absolute;right:0;top:0;width:100%}
.nfp .scrubber-container .current-progress{background:#e50914}
.nfp .scrubber-container .buffered{background:hsla(0,0%,100%,.2)}
.nfp .scrubber-container .scrubber-head{background:#e50914;border-radius:50%;cursor:pointer;
  height:2.3em;margin:-1.15em;opacity:1;position:absolute;top:50%;
  transform:scale(1) translateZ(0);transform-origin:50% 50%;transition:transform .2s ease;
  width:2.3em}
.nfp .scrubber-container .scrubber-head:hover{transform:scale(1.2) translateZ(0)}
.nfp .is-scrubbing .scrubber-head{transform:scale(1.2) translateZ(0)}
.nfp .is-scrubbing .scrubber-container .track,
.nfp .scrubber-container:hover .track{height:1em}
```

控制层骨架（全屏播放器 `PlayerControlsNeo`）：

```
.PlayerControlsNeo__gradient-bottom{background-image:linear-gradient(0deg,rgba(0,0,0,.7) 0,transparent);
  bottom:0}
.PlayerControlsNeo__gradient-top{background-image:linear-gradient(180deg,rgba(0,0,0,.7) 0,transparent);top:0}
.PlayerControlsNeo__gradient-bottom,.PlayerControlsNeo__gradient-top{height:20%;left:0;opacity:0;
  position:absolute;right:0;transition:opacity .3s ease}
.PlayerControlsNeo__layout--active .PlayerControlsNeo__gradient-bottom,
.PlayerControlsNeo__layout--active .PlayerControlsNeo__gradient-top{opacity:.75}
.PlayerControlsNeo__layout--dimmed{background-color:rgba(0,0,0,.5)}
.PlayerControlsNeo__layout--dimmed .PlayerControlsNeo__core-controls,
.PlayerControlsNeo__layout--inactive .PlayerControlsNeo__core-controls,
.PlayerControlsNeo__progress-control-row--row-hidden{opacity:0}
.PlayerControlsNeo__core-controls .top-left-controls{margin:4em}
.PlayerControlsNeo__core-controls .button-nfplayerBack{font-size:2.8em}
.PlayerControlsNeo__progress-container{align-items:center;display:flex;padding-left:3em;
  padding-right:3em;transition:opacity .2s ease}
.PlayerControlsNeo__button-control-row{align-items:center;display:flex;
  justify-content:space-between;padding-right:.9em}
.PlayerControlsNeo__button-control-row-left,
.PlayerControlsNeo__button-control-row-right{align-items:center;display:flex;padding:0 4em}
.PlayerControls--control-element{transform:translateZ(0) scale(1);
  transition:transform .25s cubic-bezier(.5,0,.1,1),opacity .25s}
.PlayerControls--control-element.progress-control{align-items:center;display:flex;
  margin:0 2.5em 0 0;width:100%}
.PlayerControls--control-element.video-title{align-items:center;box-sizing:border-box;display:flex;
  flex-grow:1;margin:0 1.5em;overflow:hidden;padding:0 0 1.68em}
.PlayerControls--control-element.video-title h4{font-size:2.2em;margin:0 .5em 0 0;padding:0;
  white-space:nowrap}
.PlayerControls--control-element.video-title span{color:#adadad;display:inline;font-size:2em;
  padding-right:.5em}
.time-remaining__time{font-size:1.8em}
.watch-video--bottom-controls-container-scrim{
  background-image:linear-gradient(0deg,rgba(0,0,0,.3) 0,transparent)}
```

按钮与图标：

```
.nfp-button-control{align-items:center;background:transparent;border:0;box-sizing:content-box;
  cursor:pointer;display:flex;flex:0 0 auto;justify-content:center;outline:none;fill:#fff}
.nfp-button-control.default-control-button{height:3.22222222em;padding:0 .83333333em}
.nfp-button-control.circle-control-button:before{background:rgba(65,65,65,.85);border-radius:50%;
  content:"";height:2em;left:50%;position:absolute;top:50%;
  transform:translate(-50%,-50%);width:2em}
.PlayerControlsNeo__bottom-controls .nfp-button-control{border:3px solid transparent;
  border-radius:.3em;box-sizing:border-box;display:flex;flex:0 1 auto;font-size:2.8em;
  height:3.6em;padding:0 0 .6em;width:3.4em}
.PlayerControlsNeo__bottom-controls .nfp-popup-control{display:flex;flex:0 1 auto;font-size:2.8em;
  height:3.6em;width:3.4em}
.nfp .svg-icon{border:1px solid transparent;height:1.6em;margin:0 -1px;overflow:visible;
  width:1.6em;stroke-width:0;stroke:#fff;font-size:1.1em}
.nf-kb-nav-wrapper.kb-nav .nfp-button-control:focus,
.nf-kb-nav-wrapper.kb-nav .scrubber-head:focus,
.nf-kb-nav-wrapper.kb-nav .speed-controller .value-container:focus .dot{outline:3px solid #fff}
.nf-kb-nav-wrapper.kb-nav .PlayerControlsNeo__bottom-controls .nfp-button-control:focus{
  border:3px solid #fff;outline:0}
```

loading 层（顶部渐变 / 标题 / 返回，本轮照它取的值）：

```
.watch-video .player-loading .gradient{background:linear-gradient(180deg,rgba(0,0,0,.651),transparent);
  height:11.11111111em;left:0;position:absolute;right:0;top:0}
.watch-video .player-loading .top-left-controls,
.watch-video .player-loading .top-right-controls{margin:4.5em}
.watch-video .player-loading .title{font-size:2em;font-weight:500}
.watch-video .player-loading .playable-title{font-size:3.3em;font-weight:500}
.watch-video .player-loading .subtitle{font-size:1.33em}
.watch-video .player-loading .metadata{color:#e4e4e4;font-weight:500;left:2.5em;top:3.5em}
.watch-video .player-loading .player-exit{color:#fff;height:1.4em;width:1.4em;opacity:.5;
  transition:opacity .2s ease-out;fill:#fff}
.watch-video .player-loading .player-exit:hover{opacity:1}
.watch-video .BackToBrowse>svg{fill:#dadada;height:6.75em;width:6.75em;padding:.75em}
.nfp.AkiraPlayer .player-loading .top-left-controls,
.nfp.AkiraPlayer .player-loading .top-right-controls,
.watch-video .player-loading .top-left-controls,
.watch-video .player-loading .top-right-controls{margin:4.5em}
```

弹出层（音轨 / 字幕菜单）：

```
.nfp-popup-control .popup-content{background:#262626;border-radius:.3em;font-size:.8em;overflow:hidden}
.nfp-popup-control .popup-content-wrapper{bottom:3.5em;margin:-.5em;opacity:0;padding:.5em;
  pointer-events:none;position:absolute;right:.5em;transform:scale(.95) translateZ(0);
  transition:opacity .16s ease,transform .2s ease,visibility 0s linear .16s;visibility:hidden}
.audio-subtitle-controller{background:#282828;direction:ltr;display:flex}
.audio-subtitle-controller .track-list h3{margin:0}
.audio-subtitle-controller .track-list .list-header{color:#fff;cursor:default;font-size:.94444444em;
  font-weight:500;padding:.75em 2.6em;white-space:nowrap}
.audio-subtitle-controller .track-list li{color:#b3b3b3;cursor:pointer;font-size:.72222222em;
  outline:none;padding:1em 3.4em;position:relative;white-space:nowrap}
.audio-subtitle-controller .track-list li:hover{background:hsla(0,0%,100%,.1);color:#fff}
.audio-subtitle-controller .track-list .selected{color:#fff;font-weight:500}
```

### 11.3 官方 em 比例 → `Theme.player-unit` → 实际像素

`player-unit = 16px`。左列是 CSS 原文里的 em 值（原样），右两列是算出来的像素
（真机 = `--window-size 1920x1080` 的实拍量测，无头 = 1280×800 快照量测）。

| 项 | 官方原文 | em | 我们的 token / 像素 | 真机实测 | 无头实测 |
|---|---|---|---|---|---|
| 尺寸基数 | `.NFPlayer.container-*{font-size:18px/24px}` + `html.watch-video-root{font-size:10px}` | 1em | `Theme.player-unit: 16px` | — | — |
| 控制层边距（左/上/右/下） | `.top-left-controls{margin:4.5em}` | 4.5 | `player-gutter` = **72px** | 72 | 72 |
| 进度条可点区高 | `.scrubber-container{height:3.3em}` | 3.3 | `player-hit-height` = **52.8px** | 53 | 53 |
| 轨道静息高 | `.track{height:.5em}` | 0.5 | `player-bar-height` = **8px** | 8 | 8 |
| 轨道 hover / 拖动高 | `:hover .track,.is-scrubbing .track{height:1em}` | 1 | `player-bar-height-hover` = **16px** | — | 16 |
| 拖动头直径 | `.scrubber-head{height:2.3em;width:2.3em;margin:-1.15em}` | 2.3 | `player-bar-knob` = **36.8px** | 36 | 36 |
| 拖动头 hover / 拖动放大 | `.scrubber-head:hover{transform:scale(1.2)}` | 1.2 | `player-bar-knob-scale` | 44.2 理论 | 44 |
| 轨道 / 缓冲 / 已播颜色 | `#5b5b5b` / `hsla(0,0%,100%,.2)` / `#e50914` | — | `player-track` / `player-buffered` / `player-progress` | 逐像素一致 | 逐像素一致 |
| 顶部渐变高 | `.player-loading .gradient{height:11.11111111em}` | 11.111 | `player-top-scrim-height` = **177.8px** | — | 178 |
| 顶部渐变颜色 | `linear-gradient(180deg,rgba(0,0,0,.651),transparent)` | — | `player-top-gradient`（`#000000A6`） | — | 渐变已生效 |
| 底部渐变（主层） | `.PlayerControlsNeo__gradient-bottom{…rgba(0,0,0,.7)…;height:20%}` + `.75` | .7×.75 | `player-scrim-bottom: #00000086`（0.525 预乘） | — | — |
| 底部渐变（第二层） | `.watch-video--bottom-controls-container-scrim{…rgba(0,0,0,.3)…}` | .3 | `player-scrim-floor: #0000004D` | — | — |
| 底部渐变高度 | `height:20%` | 20% | `max(视口高×0.2, player-scrim-min-height)`，1280×800 → **322.4px** | — | 322 |
| 左上返回图标 | `.player-exit{width:1.4em;height:1.4em;opacity:.5}` | 1.4 | `player-back-size` = **22.4px** | 22×20（墨迹） | 22×19 |
| 标题字号 / 字重 | `.title{font-size:2em;font-weight:500}` | 2 | `player-title-size` = **32px** / `player-title-weight` 500 | 32 | 32 |
| 副标题字号 / 颜色 | `.subtitle{font-size:1.33em}` / `video-title span{color:#adadad}` | 1.33 | `player-subtitle-size` = **21.3px** / `player-subtitle-color: #ADADAD` | — | 21 |
| 时间码 | `.time-remaining__time{font-size:1.8em}` | 1.8 | `player-time-size` = **28.8px** | — | 29 |
| 普通按钮盒高 | `.nfp-button-control.default-control-button{height:3.22222222em}` | 3.2222 | `player-control-size` = **51.6px** | — | 52 |
| 播放钮盒高 | `.PlayerControlsNeo__bottom-controls .nfp-button-control{height:3.6em}` | 3.6 | `player-play-size` = **57.6px** | 盒透明量不到；图标墨迹 16×18（理论 16.9×19.6） | 图标墨迹 16×18 |
| 按钮内边距 | `.nfp-button-control{padding:0 .83333333em}` | 0.8333 | `player-control-padding` = **13.3px** | — | — |
| 圆钮底 | `.circle-control-button:before{background:rgba(65,65,65,.85);border-radius:50%}` | — | `player-icon-bg: #414141D9` | — | — |
| 焦点环 / 常驻边框 | `:focus{outline:3px solid #fff}`、`border:3px solid transparent` | 3px | `player-focus-width: 3px` / `player-focus-color: #fff` | — | 3 |
| 弹出层底色 / 圆角 | `.popup-content{background:#262626;border-radius:.3em}` | 0.3 | `player-panel-solid: #262626`（圆角沿用 `radius-panel`） | — | — |

### 11.4 图标 → 官方 em 比例 → 我们的 px

官方 `.watch-video .svg-icon.svg-icon-*{width:…em;height:…em}`（左边是官方类名）。
注意官方还有一层字号嵌套（图标所在按钮 `font-size:2.8em`，`.nfp .svg-icon{font-size:1.1em}`），
**我们没有复制这层嵌套**：图标盒直接 = `player-unit × 比例`（任务书口径）。比例本身是官方的。

| 图标 | 官方宽 × 高（em） | 我们的 token | 我们的 px（16px 基数） |
|---|---|---|---|
| 播放 / 暂停 `nfplayerPlay` / `nfplayerPause` | 1.05555556 × 1.22222222 | `player-icon-play-w/h` | 16.9 × 19.6 |
| 音量 `volumeLow/Medium/Max/Muted` | 1.33333333 × 1.11111111 | `player-icon-volume-w/h` | 21.3 × 17.8 |
| 全屏 / 窗口 `nfplayerFullscreen` / `nfplayerWindowed` | 1 × 1 | `player-icon-fullscreen-w/h` | 16 × 16 |
| 字幕 `nfplayerSubtitles` | 1.33333333 × 1.11111111 | `player-icon-subtitles-w/h` | 21.3 × 17.8 |
| 集数 `nfplayerEpisodes` | 1.5 × 1 | `player-icon-episodes-w/h` | 24 × 16 |
| 下一集 `nfplayerNextEpisode` | .83333333 × 1 | `player-icon-next-episode-w/h` | 13.3 × 16 |
| 快退 10s `nfplayerBackTen` | 1.11111111 × 1.33333333 | `player-icon-back-ten-w/h` | 17.8 × 21.3 |
| 快进 10s `nfplayerFastForward` | 官方**没给**单独尺寸（与 BackTen 同形镜像） | `player-icon-forward-ten-w/h`（复用上一行） | 17.8 × 21.3 |
| 重播 `nfplayerReplay` | 1.5 × 1.5 | `player-icon-replay-w/h` | 24 × 24 |
| 停止 `nfplayerStop` | 1 × 1 | `player-icon-stop-w/h` | 16 × 16 |
| 投屏 `nfplayerMdx` / `MdxFull` | 1.22222222 × 1 | `player-icon-mdx-w/h` | 19.6 × 16 |
| 勾选 `nfplayerCheck` | .94444444 × .83333333 | `player-icon-check-w/h` | 15.1 × 13.3 |
| 返回 `nfplayerBack` | .72222222 × 1 | `player-icon-back-w/h` | 11.6 × 16 |
| 关闭 `nfplayerExit` | 1 × 1 | `player-icon-exit-w/h` | 16 × 16 |

实际用到的向量图形：播放三角、暂停双竖条、喇叭（含静音斜线）、左上返回箭头。
其余 10 个比例先存成 token（`theme.slint` 的“播放器图标”段），因为我们的传输行目前是
**文字标签按钮**（`-15s` / `+15s` / `音轨` / `字幕` / `1x` / `全屏`），不是官方那套 SVG 图标；
换成图标是另一件事（见 11.7）。

### 11.5 本轮到底改了什么（逐条对官方原文）

| # | 官方原文 | 落地 |
|---|---|---|
| 1 | `.scrubber-container{height:3.3em}` | `PlayerProgress` 整体高 = `player-hit-height`＝52.8px（旧 28px），命中区就是这一整条 |
| 2 | `.track{background:#5b5b5b;height:.5em}` | 轨道**整条铺满**、不透明 `#5B5B5B`、高 8px、**方头**（官方三条都没有 `border-radius`）；旧实现是半透明白 + 胶囊 |
| 3 | `.buffered{background:hsla(0,0%,100%,.2)}` | 缓冲段改成**叠在轨道上**的半透明层（旧实现把缓冲段的轨道挖空了，缓冲段会比官方暗） |
| 4 | `.current-progress{background:#e50914}` | 已播段 `Theme.player-progress` = `#E50914` |
| 5 | `.scrubber-head{height:2.3em;width:2.3em;margin:-1.15em;background:#e50914;border-radius:50%;opacity:1}` | 红色圆点、直径 36.8px，**常驻**（旧实现只在 hover 时出现） |
| 6 | `.scrubber-head:hover{transform:scale(1.2)}`、`.is-scrubbing .scrubber-head{transform:scale(1.2)}` | `player-bar-knob-scale: 1.2`，hover 与拖动都放大 |
| 7 | `.track{transition:height .2s ease}` | `animate bar-height { duration: Theme.motion-zoom; easing: cubic-bezier(.25,.1,.25,1) }`（CSS `ease` 的原值），knob 同步 |
| 8 | `.top-left-controls{margin:4.5em}` | 左上返回 / 底栏左右 / 菜单右边距统一 `player-gutter` = 72px |
| 9 | `.player-exit{width:1.4em;height:1.4em;opacity:.5;transition:opacity .2s ease-out}` + `:hover{opacity:1}` | 左上返回去掉 44px 圆钮底，改成 22.4px 图标、opacity .5 → 1、`motion-zoom`(200ms) + `ease-out` |
| 10 | `.player-loading .gradient{…rgba(0,0,0,.651)…;height:11.11111111em}` | 新增顶部渐变层（旧实现根本没有），高 177.8px |
| 11 | `.title{font-size:2em;font-weight:500}`、`.subtitle{1.33em}`、`video-title span{color:#adadad}` | 底部标题 32px/500、副标题 21.3px `#ADADAD`（旧 22px/600 + 14px muted） |
| 12 | `.PlayerControlsNeo__gradient-bottom{linear-gradient(0deg,rgba(0,0,0,.7) 0,transparent);height:20%;opacity:.75}` + `bottom-controls-container-scrim{…rgba(0,0,0,.3)…}` | 底部渐变从“两段式 0.94 平台”换成官方**两层**梯度（0.525 + 0.3），高度 = `max(视口×0.2, 控制块 + 两倍留白)` |
| 13 | `.nfp-button-control{height:3.22222222em;padding:0 .83333333em}`、`border:3px solid transparent` | 传输按钮 51.6px 高、左右内边距 13.3px、3px 透明边框（聚焦/选中时换白） |
| 14 | `.nfp-button-control{background:transparent}`（底栏按钮**无底色、无描边**） | **控制行统一成一套图标/文字按钮**：播放/暂停、音量、±15s、字幕、音轨、倍速、全屏全部 `background:transparent` + `3px solid transparent`、白色图标/文字、hover 只有极淡高亮（`player-control-hover`）。旧的深色圆底 + 描边（hero 视频圆钮那套）**整块删除** |
| 15 | `:focus{outline:3px solid #fff}` | `player-focus-width: 3px` + `player-focus-color: #fff`，画在“菜单已打开”的按钮上（见 11.6） |
| 16 | `.popup-content{background:#262626}`、`li{color:#b3b3b3}`、`.selected{color:#fff;font-weight:500}` | 弹出层底色 `#262626`；行高跟控制层一致（51.6px）；行内文字色沿用原来的 accent/muted（**没改**，见 11.6） |
| 17 | `.time-remaining__time{font-size:1.8em}` | 时间码 28.8px 等宽（旧 14px） |
| 18 | 图标比例表（11.4） | 播放/暂停/音量/返回四个图形按 `player-unit × 比例` 重画；`player-icon-glyph: 24px` 这个绝对值 token **已删除** |
| 19 | 官方播放器画面上**没有任何**常驻/可展开的诊断文字 | 删掉常驻的 `直连 / 转码` 短标签、硬解提示、`Ctrl+D` 展开的引擎行；数据仍在 `PlayerData`（`transport-mode` / `engine-line` / `hwdec-notice` / `show-engine-detail`），交给 `--debug-socket` 的 `view` 读。`Ctrl+D` 从播放页快捷键表整条移除（不留“按了没反应”的键） |
| 20 | `.nf-big-play-pause button .svg-icon{opacity:.7}`（中央大钮 = 半透明大图标，无底色无描边） | 中央暂停钮去掉 88px 深色圆底 + 描边，改成 3em 的半透明播放图标（`opacity: .7`） |
| 21 | `.time-remaining__time`（时间位 = **剩余**时长） + 底栏按钮分组 | 时间默认显示 `-2:05:26`（`TimeMode` 默认改成 `Remaining`），放在控制行**最右**；左侧一组 = 播放/暂停、音量、-15s、+15s，右侧一组 = 字幕、音轨、倍速、全屏，组间只有**一个** `horizontal-stretch: 1` |
| 22 | `.nfp .volume-controller`（图标 + 滑杆；`.scrubber-target{1em}` + `:hover{scale(1.2)}`、`slider-bar-container{#5b5b5b}`、`slider-bar-percentage{#e50914}`） | 音量 = 喇叭图标 + **悬停时向右展开的水平滑杆**（轨道 6.4px `#5B5B5B`、填充 `#E50914`、手柄 `1em`×1.2），点击/拖动滑杆直接设到百分比（新增 `volume-set(float)` 回调链）；`player-icon-bg` / `player-icon-border-width` 两个只为圆钮存在的 token **已删除** |

### 11.6 偏差与自定（不要当成“和 Netflix 一致”）

| 项 | 官方 | 我们 | 理由 |
|---|---|---|---|
| **基数** | `watch-video-root` 10px（1920×1080）/ `container-*` 18–24px | `player-unit: 16px` | 见 11.1；16px 是我们选的，**不是官方值**，同样的 em 比例我们会整体大 1.6 倍 |
| 控制行两种来源 | 播放器底栏：无底色无描边（`background:transparent`）；hero 视频圆钮：`rgba(22,22,22,.7)` 底 + `1px solid rgba(128,128,128,.7)` | 只用前者；后者**不再用在播放器里** | 这两条来自**不同来源**（§7 的实测是 hero 上的浮层圆钮），上一轮误把它套到了控制行，本轮按用户反馈改回官方底栏样式 |
| 底部渐变的 stop 位置 | 纯线性 `0deg`（从底到顶线性淡出） | `#00000086`（0.525）在底部 **75%** 保持不降，上面 25% 才淡出（第二层 `.3` 同样） | 我们的控制块（标题在顶、进度条、按钮）比官方高，标题会正好压在渐变的透明端点上；颜色与两层结构仍是官方的，只把 stop 下移（亮画面上标题/副标题要可读） |
| 底部渐变的高度 | `height:20%` | `max(视口高×0.2, 控制块 + 2×player-gutter)`（1280×800 → 322px） | 20% 在 800 高的窗口上只有 160px，装不下 178px 的控制块 |
| 边距 | `.core-controls .top-left-controls{margin:4em}`、进度行 `padding:0 3em`、按钮行 `padding:0 4em` | 统一 4.5em（72px） | 任务书口径（取 loading 层的 `4.5em`）；三处官方值不一致，统一成一个 token 避免“进度条和按钮左右不对齐” |
| 播放钮 | 全屏底栏的播放钮是**透明底 + 白色图标**（`.nfp-button-control{background:transparent}`） | 同样透明底 + 白色图标（57.6px 盒、图标按 1.0555×1.2222 em），hover 只有极淡高亮 | 已按官方统一；旧的"红底圆钮"已删除（hero 圆钮那套不适用控制行） |
| 音量滑杆方向 | 官方 `.volume-controller .slider-container{width:1.5em;height:8em}` 是**垂直** popup | 水平内联展开（图标右侧） | 任务书要求水平；比例/颜色仍照官方。滑杆总宽 `6em`（96px）是**我们自定**，官方没给总宽 |
| 控制行 hover 反馈 | 官方底栏 hover 只换 `fill`（图标本来就是白 → 视觉上几乎没有变化） | 一层 `hsla(0,0%,100%,.1)` 的极淡白底（官方弹层 `li:hover` 用的就是这个值） | 完全照官方会"悬停没反应"，取官方另一个 hover 值做等价反馈 |
| 时间模式默认 | 官方只有剩余时长 | 默认剩余，点一下切"已播/总长" | 多了个切换是功能增强（不是常驻额外元素） |
| 快退 / 快进步长 | 官方图标是 ±10s（`nfplayerBackTen` / `nfplayerFastForward`） | 文字 `-15s` / `+15s`（与键盘 ←/→ 的 15s 一致） | 形态统一了，步长保留我们的（改步长会连带改键盘语义与测试）；图标比例已在 11.4 备好 |
| 传输行内容 | SVG 图标（播放/快退10/快进10/集数/字幕/音轨/倍速/全屏） | 文字标签按钮（`-15s` / `+15s` / `音轨` / `字幕` / `1x` / `全屏`） | 图标比例已进 token（11.4），但把文字行换成图标行会连带改进度/菜单锚点，不在本轮范围 |
| 键盘焦点环 | `:focus{outline:3px solid #fff}`，由 `.kb-nav` 键盘导航驱动 | 同样的 3px 白边框，但画在“这个菜单已打开/选中”的按钮上 | 播放页没有逐控件的键盘焦点模型（键位由 `app.slint` 的全局 `FocusScope` 分派给 Rust），这是最接近的可见状态；等有了控件级导航再接真正的 `:focus` |
| `--dimmed` 整屏压暗 | `.PlayerControlsNeo__layout--dimmed{background-color:rgba(0,0,0,.5)}` | **没用** | 同一个规则组里还写着 `--dimmed .PlayerControlsNeo__core-controls{opacity:0}`：压暗时控制层是隐藏的，说明它是另一个状态（菜单/加载），不是“控件可见就压暗整帧”。token `player-dim` 保留备查 |
| 时间气泡 | 官方只有 trickplay **缩略图**（`.trickplay`），没有文字气泡 | 保留我们的文字气泡（`player-bubble-height`） | 我们没有生成 trickplay 缩略图的能力 |
| 诊断行（直连 / 转码 / hwdec / 引擎行） | 无对应规则（Netflix 播放画面上没有任何诊断文字） | **本轮已从画面删除**（连 `Ctrl+D` 一起）；数据留在 `PlayerData`，只给调试通道 | 它属于我们自己的调试残留，不是用户播放时需要的信息；删掉后标题行只剩标题 + 副标题（见 11.7） |
| 弹出层的行内排版 | `li{color:#b3b3b3;font-size:.72222222em;padding:1em 3.4em}`、`list-header{font-size:.9444em;padding:.75em 2.6em}` | 只对齐了底色（`#262626`）、行高与标题条高度；字号仍是 14/12px | 菜单不是本轮 5 项之一；`.7222em` × 16px = 11.6px 会让中文行变得难读（官方基数 10px 时才 7.2px，是“小字菜单”） |
| `cursor:none` | `.nf-player-container.inactive{cursor:none}` | 用 `TouchArea.mouse-cursor: MouseCursor.none` 实现（已有） | Slint 1.19 用户代码**确实**能设 `mouse-cursor`，所以这条是照做的，不是跳过 |

### 11.7 与官方播放器的元素对照

判断标准只有一条：**播放时用户真正需要的信息**才留在画面上，其余都是我们自己的诊断 / 调试残留。

| 元素 | 官方播放器 | 我们 | 处理 |
|---|---|---|---|
| 返回箭头 | `.player-exit{width:1.4em;height:1.4em;opacity:.5}` + hover `.5 → 1` | 左上 1.4em 箭头、opacity .5 → 1、200ms ease-out | **保留**（照官方） |
| 标题 + 副标题 | `.video-title h4{font-size:2.2em}` + `span{color:#adadad;font-size:2em}`；loading 层 `.title{2em;500}` / `.subtitle{1.33em}` | 32px/500 + 21.3px `#ADADAD` | **保留**（照官方排版梯度） |
| 进度条 + 把手 | `.scrubber-container` 全套 | 3.3em 命中区 / 8→16px 轨道 / 36.8px 红把手 | **保留**（照官方，见 11.3） |
| 时间 | `.time-remaining__time{font-size:1.8em}`，显示**剩余** | 28.8px 等宽、控制行最右、默认 `-2:05:26`；点一下切 `12:34 / 2:18:00` | **保留**（默认已按官方改成剩余；切换是增益） |
| 播放 / 暂停 | 底栏图标钮（透明底、白色图标） | 透明底图标钮 57.6px，图标按 1.0555×1.2222 em | **保留**（已统一成官方形态，见 11.11） |
| 快退 / 快进 | `nfplayerBackTen` / `nfplayerFastForward` 图标（±10s） | 文字钮 `-15s` / `+15s` | **保留**（功能等价、步长是我们选的 15s；图标比例已存 token 备用） |
| 音量 | 图标 + 滑条（官方 popup 是垂直的） | 图标（点击静音）+ 悬停时向右展开的**水平滑杆**，手柄红色圆点 | **保留**（滑杆按任务书做水平；数字百分比已从画面移除，见 11.11） |
| 音轨 / 字幕 | `audio-subtitle-controller` 弹层 | 同构弹层 + 官方 `#262626` 底 | **保留** |
| 倍速 | `speed-controller` 弹层（0.5×–1.5× 刻度） | 单钮循环 1× → 1.25× → 1.5× → 0.75× | **保留**（简化形态，功能在） |
| 全屏 | `nfplayerFullscreen` / `nfplayerWindowed` | 文字钮（`全屏` / `退出全屏`） | **保留** |
| 下一集 | `nfplayerNextEpisode` + 底栏钮 | **缺** | 暂不做（我们没有播放队列 / 连播上下文；`PlayerData` 里也没有 next 字段） |
| 集数选择 | `nfplayerEpisodes` + `.episode-list` | **缺** | 暂不做（同上） |
| 投屏 mdx | `nfplayerMdx` | **缺** | 不做（我们的播放器是本机 mpv） |
| 暂停时中央播放钮 | `.nf-big-play-pause`（图标 `opacity:.7`，无底圆） | 88px 半透明圆 + 图标 | **保留**（尺寸自定；半透明底是为了压住字幕区） |
| 缓冲指示 | **转圈动画**，无文字 | `缓冲中…` 文字角标（放官方 `top-right-controls` 的 4.5em 位置） | **保留**：Slint 1.19 没有内建 spinner，也没有循环动画能力（`Timer` 无法替代声明式动画）；文字是最小代价的等价信息。**这是画面上唯一一个“官方用动画、我们用文字”的常驻元素**（只缓冲时出现） |
| 错误提示 | fatal-error 视图 + 退出 | 错误卡 + 原因 + 重试 | **保留**（照官方结构简化） |
| 拖动时的时间气泡 | **trickplay 缩略图**（`.trickplay`） | 文字气泡（`12:34`） | **保留**（没有生成缩略图的能力；气泡只在拖动时出现，不常驻） |
| 常驻诊断：`直连 / 转码`、硬解提示、`Ctrl+D` 引擎行 | **没有这类东西** | 曾有三条常驻文字 | **已删除**（本轮）；数据留在 `PlayerData` 给 `--debug-socket` |
| 首帧前的空态 | 转圈 | `视频画面` + 一行 HDR 说明 | **保留**（只在没有画面时出现，播放时不渲染）；顺手删掉过时的开发提示「引擎接入后由 mpv 渲染到这里」 |

结论：删掉了 3 条常驻文字（都是我们自己的诊断），删掉了 1 行过时的开发提示；
其余“我们多的”都是**功能**（时间气泡、音量数字、菜单里的字幕延迟 / 外挂字幕），
它们要么只在交互时出现，要么在菜单里，不属于“播放画面上常驻的状态文字”。
官方有而我们没有的只有三项：**下一集 / 集数选择 / 投屏**，理由都写在表里。

菜单里的层级也顺手压了一档（`ActionRow`）：字幕延迟 / 外挂字幕从 14px 降到 **12px**、
行高 44px < 轨道行 51.6px，上方有 `Theme.hairline` 分隔线——轨道列表仍是菜单主体。
（Slint 里画分隔线的成本不高，所以直接在 `TrackMenu` 里放了一条 `Rectangle`，不用改结构。）

### 11.8 没找到 / 没做的官方规则（明确列出）

- **完整控制条布局**：官方 DOM 里是 `top-left-controls` / `top-right-controls` / `progress-container`
  / `button-control-row-left|right` 四块，但 CSS 只给 padding/margin，**没有给出各按钮的顺序、
  分组与完整间距**（那是 React 组件的结构，不在样式表里）。我们的顺序（播放 → ±15s → 时间 →
  音量 → 音轨 → 字幕 → 倍速 → 全屏）是自己的。
- **倍速菜单**：只找到 `.speed-controller{padding:1em 1.4em}`、`.values .value{width:6.1em;…}`、
  `.active-dot{…}` 这些内部零件，**没有找到整套弹层的容器尺寸**。我们仍是“点一下循环倍速”。
- **字幕渲染样式**：`.player-timedtext*` 只有字体族覆盖（`[lang=ja]{font-family:…}`）与
  `.VideoContainer--hide-subtitles`，字幕的位置/字号由浏览器 `::cue` 与引擎决定；
  我们这条链路由 mpv 渲染，界面上只能标注“引擎渲染，样式不可改”（已有）。
- **trickplay 缩略图**：规则有（`.trickplay{…border-radius:.3em;bottom:2.5em}`、
  `.tp-text{font-size:1.8em;padding:.5em 0}`），但缩略图本身是服务端生成的雪碧图，我们没有。
- **键盘导航状态机**：只有 `.nf-kb-nav-wrapper.kb-nav` 下的 `:focus` 样式，没有“焦点如何移动”的规则。
- **`--active` / `--inactive` / `--dimmed` / `--timedout` 四个状态的切换时机**：样式表里只有样式，
  没有时序（3 秒自动隐藏是我们的 `pages::player::TICK_INTERVAL` 相关逻辑）。

### 11.9 快照变化（本轮 8 张，其余 21 张逐字节不变）

`cargo test -p nova-ui --test snapshots` 重拍后与改动前逐像素比对（bbox = 差异外接框）：

| 快照 | 差异区域 bbox | 差异像素 | 为什么变 |
|---|---|---|---|
| `player.png` / `player-controls.png` | 全图 | 633 740 | 同一帧两存：① 底部换成官方两层渐变（顶部 45% 变透明、底部更暗）+ 新增顶部渐变；② 标题 22/600 → 32/500、副标题 14 → 21.3/`#ADADAD`，右侧三条常驻诊断（`直连`/硬解/引擎行）**整块删除**；③ 进度条 4px 半透明白胶囊 → 8px `#5b5b5b` 方头 + 36.8px 红把手常驻 + 缓冲段改为叠加；④ 返回钮 44px 圆钮 → 22.4px icon；⑤ **控制行统一**：红底播放圆钮 / 深色音量胶囊 + 描边 → 全部透明底白色图标/文字，音量改成图标 + 悬停滑杆，按钮从 `space-3` 改成 `space-2` 间距；⑥ 时间码从左侧 `12:34 / 2:18:00` 挪到**最右**并默认显示剩余 `-2:05:26`；⑦ 音量百分比数字从界面移除 |
| `player-controls-bright.png` | 全图 | 638 720 | 同上；另外这是唯一验证“亮画面上文字可读”的帧：官方两层渐变（0.525 + 0.3）+ 底部 75% 平台后，米白画面上的实测对比度 = 标题 3.9–5.2:1、时间码/按钮 8.3:1、副标题（官方 `#adadad`）2.3–3.7:1（比旧的两段式 0.94 平台低；副标题偏淡是官方颜色 + 官方渐变叠加的必然结果，要更黑就得偏离官方）|
| `player-buffering.png` | 全图 | 633 740 | 同 `player-controls` + 缓冲角标从 (right−gutter, 24px) 移到官方 `top-right-controls` 的 `margin:4.5em` 位置（72,72），高度改用 `player-badge-height`（2.25em = 36px，外观不变） |
| `player-volume-hint.png` | 全图 | 638 720 | 同 `player-controls`。这张的**语义变了**：以前验证“短时音量数字”，现在数字已从界面移除，验证的是“静音态下喇叭换成静音字形 + 控制行统一形态”（`volume-text` / `volume-hint` 数据仍在 viewmodel，只是没有元素读它） |
| `player-error.png` | 全图 | 699 889 | 同 `player-controls` + 错误卡底 `#141414` → `#262626`（官方 `.popup-content` 底色）、重试按钮跟着变 |
| `player-audio-menu.png` | 全图 | 650 956 | 同 `player-controls` + 菜单底 `#141414` → `#262626`、行高 52 → 51.6、菜单右边距 `Theme.gutter`(51) → `player-gutter`(72)、锚点跟着进度条变、`音轨` 按钮多一圈 3px 白描边（= `:focus` 环）；按钮从胶囊底色改成无底色后，菜单附近的像素也一起变 |
| `player-subtitle-menu.png` | 全图 | 714 761 | 同 `player-audio-menu` + 字幕菜单多 3 行动作行（`字幕延迟 ±0.5s` / `加载外挂字幕…`，字号 14 → **12**、行高 44px），行高变化被 5 行放大 |
| `player-controls-hidden.png` | — | 0 | 控制层不可见 → 顶部/底部渐变与所有控件一起 `opacity:0` 且不参与命中测试（**逐字节不变**是“隐藏时全不可点”的正面证据） |

其余 21 张（`home-*` / `detail*` / `library` / `search*` / `banner-*` / `add-server-*`）逐字节不变。

中途踩到的坑：海报卡底部“继续观看”进度条也引用了 `Theme.player-track`，把轨道色改成
`#5b5b5b` 会让首页 4 张快照出现 1 600+ 像素的差异；已拆出独立 token
`Theme.card-progress-track`（仍是半透明白），首页回到逐字节不变。

删除三条常驻诊断**没有**再动布局高度：它们和标题在同一行（`HorizontalLayout`），
行高由标题的 2em 行高撑开，所以控制块高度、进度条与按钮的 y 坐标都没变，
差别只是标题行右侧那三个文本块不再画（见 `player-controls*.png` 的标题行）。

### 11.10 真机验证（1920×1080）

```bash
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 NOVA_DATA_DIR=$HOME/.local/share/nova \
  ./target/release/nova-desktop --window-size 1920x1080 --play 6564 \
  --screenshot /tmp/player.png --screenshot-delay 8000
# screenshot: 1920x1080 -> /tmp/player.png
```

| 量 | 理论（官方 em × 16px） | 真机实测 |
|---|---|---|
| 进度条轨道高 | `0.5 × 16` = 8px | **8px**（行 904–911） |
| 拖动头直径 | `2.3 × 16` = 36.8px | **36px**（y 890–925） |
| 控制层左边距 | `4.5 × 16` = 72px | **72px**（轨道 x 72…1847，右侧同理 1920−72−1） |
| 控制层下边距 | 72px | **72px**（播放钮 y 951–1006，底 1007 ≈ 1080−72） |
| 播放/暂停图标 | `1.0555 × 1.2222 em` = 16.9 × 19.6px | **16 × 18px**（墨迹 bbox x 93–108 / y 970–987；按钮盒 57.6px 透明、像素上量不到底色） |
| 左上返回图标 | `1.4 × 16` = 22.4px | **22×20px**（墨迹 bbox x 72–93 / y 73–92） |
| 进度条 hover（无头） | `1 × 16` = 16px / 把手 `36.8 × 1.2` = 44.2px | **16px / 44px**（临时测试：指针移到 (600,628) 后拍帧，验证完已删除该测试文件） |
| 音量滑杆（无头） | 轨道 `.4em` = 6.4px / 手柄 `1em`×1.2 = 19.2px / 总宽 6em = 96px（自定） | **6px / 19px / 滑杆从图标右侧展开约 96px**（临时测试：指针移到音量钮 (160,699) 后拍帧，见 `/tmp/player-volume-open.png`） |
| 剩余时间右对齐 | 右侧留白 72px | **11px** 内边距 + 文字右缘 1837（容差 3px，等宽字体边距） |

图象：第一次跑（`--screenshot-delay 8000`）该条目报了「这个条目没有可播放的版本」→ 错误卡；
再跑就正常起播了（说明那是当次的服务端 / 选源结果，不是稳定状态）。三张图都留着当证据：

- `/tmp/player-controls-1920.png`（`--screenshot-delay 2000`）：控制层可见，上面的量测都出自它，
  标题行**只有**「阿基拉　1988 · 动画 / 科幻」，没有任何诊断文字；控制行是统一的无底色按钮 +
  左侧 `音量/-15s/+15s`、右侧 `字幕/音轨/1×/全屏`、最右剩余时间 `-2:04:46`。
- `/tmp/player-volume-open.png` / `/tmp/player-scrub-hover.png`：无头临时测试（验证完已删除），
  分别验证“音量滑杆悬停展开”与“进度条 hover 变粗”。
- `/tmp/player.png`（`--screenshot-delay 8000`）：控制层已按 3 秒规则自动隐藏，只剩画面。

顺带记一个**偶发**现象（与本轮改动无关）：某一跑里 mpv 首帧在 1920×1080 下上下 + 左右翻转
（片头的日文标题是倒的），随后用**同一条命令**再跑两次都是正的；`--window-size 1280x800` 也复现过一次。
既然不稳定复现，更像首帧 / 尺寸协商期的纹理朝向问题，记为**待排查**，本轮只改 `.slint`（不碰 GL），
没有追溯。

### 11.11 控制行统一 + 音量滑杆（用户反馈的两项）

前一轮把"hero 视频圆钮"的实测样式（深色圆底 `rgba(22,22,22,.7)` + `1px solid rgba(128,128,128,.7)`
描边）套到了播放器控制行上，结果是**同一行两种控件语言**：音量是深色胶囊钮，右边的
「字幕 / 音轨 / 1× / 全屏」是纯文字。用户直接反馈"音量按键也很不和谐"。本轮按官方底栏重做。

#### 官方原文（这一段决定"统一形态"）

```
.nfp-button-control{align-items:center;background:transparent;border:0;box-sizing:content-box;
  cursor:pointer;display:flex;flex:0 0 auto;justify-content:center;outline:none;fill:#fff}
.PlayerControlsNeo__bottom-controls .nfp-button-control{border:3px solid transparent;
  border-radius:.3em;box-sizing:border-box;font-size:2.8em;height:3.6em;padding:0 0 .6em;width:3.4em}
.PlayerControlsNeo__button-control-row{align-items:center;display:flex;
  justify-content:space-between;padding-right:.9em}
.PlayerControlsNeo__button-control-row-left,
.PlayerControlsNeo__button-control-row-right{align-items:center;display:flex;padding:0 4em}
.nf-kb-nav-wrapper.kb-nav .PlayerControlsNeo__bottom-controls .nfp-button-control:focus{
  border:3px solid #fff;outline:0}
/* 音量 */
.nfp .volume-controller .slider-container{align-content:center;align-items:center;background:#262626;
  border-radius:.3em;box-sizing:border-box;display:flex;flex-direction:column;height:8em;padding:1em 0;
  transition:opacity .3s ease;width:1.5em}
.nfp .volume-controller .slider-bar-container{background:#5b5b5b;box-sizing:border-box;height:100%;
  position:relative;width:.4em}
.nfp .volume-controller .slider-bar-percentage{background:#e50914;bottom:0;left:0;position:absolute;right:0}
.nfp .volume-controller .scrubber-target{height:1em;left:50%;margin:-.5em;position:absolute;
  transform:scale(1);transition:transform .2s ease;width:1em}
.nfp .volume-controller .scrubber-target:hover{transform:scale(1.2)}
.nfp .volume-controller .scrubber-head{background:#e50914;border-radius:50%;height:100%;
  position:absolute;width:100%}
```

#### 落地

| 项 | 之前 | 现在 |
|---|---|---|
| 播放 / 暂停 | 红底圆钮（`Theme.accent`，57.6px） | 透明底 + 白色 pause/play 图标（盒 57.6px），hover 只有极淡高亮 |
| 音量 | 深色圆底 + 1px 描边胶囊 + 短时百分比文字 | 透明底喇叭图标（点击静音）+ **悬停展开的水平滑杆** |
| -15s / +15s | 深色胶囊 + 文字 | 透明底 + 白字（`-15s` / `+15s`） |
| 字幕 / 音轨 / 倍速 / 全屏 | 透明底但边框色 `hairline-strong`、active 时换 accent 字色 | 透明底 + 白字，active 只留 3px 白焦点环、不换字色 |
| 中央暂停大钮 | 88px 深色圆底 + 描边 | 3em 半透明的播放图标（官方 `.svg-icon{opacity:.7}`） |
| 分组 | 播放、-15s、+15s、时间、`stretch`、音量+数字、音轨、字幕、倍速、全屏（时间在左、弹性间距在组内） | 左组：播放 / 音量 / -15s / +15s；**组间一个 `horizontal-stretch: 1`**；右组：字幕 / 音轨 / 倍速 / 全屏；最右：剩余时间 |
| 时间 | 左侧 `12:34 / 2:18:00` | 最右 `-2:05:26`（`TimeMode` 默认改成 `Remaining`；点一下仍可切已播/总长） |
| 音量数字 | `volume-text` + `volume-hint` 画在按钮右边 | **从界面移除**（官方没有数字）；`PlayerData.volume-text` / `volume-hint` 与 viewmodel 逻辑保留，只是没有元素读 |

#### 音量滑杆的取值（哪些是官方、哪些是我们自定）

| 项 | 值 | 来源 |
|---|---|---|
| 图标→滑杆的展开 | 鼠标悬停 / 拖动中展开，`Theme.motion`(180ms) + `ease-out` | 我们自定（官方是 popup，靠 CSS `:hover` 显示） |
| 滑杆方向 | **水平** | 任务书要求；官方 CSS 里 `.slider-container{width:1.5em;height:8em}` 是**垂直** popup（已在此说明，不假装一致） |
| 滑杆总宽 | `6 × player-unit` = 96px | **我们自定**（官方未给总宽） |
| 轨道粗细 | `.slider-bar-container{width:.4em}` = **6.4px** | 官方 |
| 轨道色 / 填充色 | `#5b5b5b` / `#e50914`（与进度条同一套 token） | 官方 |
| 手柄 | `1em` = **16px** 红色圆点，hover / 拖动中 ×1.2 | 官方（`.scrubber-target{height:1em;width:1em}` + `:hover{scale(1.2)}`） |
| 点击 / 拖动 | 按下与拖动都把 `mouse-x` 映射成 0..1 并调 `volume-set(float)` | 我们实现（官方是拖 popup 里的滑条） |
| 静音时 | 图标换静音字形，滑杆仍可拖动；往上拖自动解除静音 | 官方无明文（`<audio>` 语义），与键盘加音量同一口径 |
| 键盘聚焦展开 | **没做**（播放页没有逐控件焦点模型，同 11.6 的焦点环说明） | 已知缺口 |

`volume-set(float)` 是一条新回调链：`PlayerOverlay.volume-set` → `PlayerShellPage.volume-set`
→ `AppWindow.player-volume-set` → Rust `player_volume_set()`；它**只**写 `PlayerView.volume`
（+ 解除静音 + `note_volume_change()`）并把目标值交给已有的引擎回调 `on_player_volume`，
**没有**碰任何播放逻辑。
