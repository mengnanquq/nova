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
| 卡片放大 | `scale 1.35`（Netflix 观感），延迟 400ms 触发，过渡 200ms ease-out；放大卡宽 = 卡宽×1.35 |
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

- 预览视频、通知铃、多用户切换、Top 10 大数字行、Netflix 的加载骨架屏动画、
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
