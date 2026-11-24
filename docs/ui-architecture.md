# UI 架构与视觉规范（阶段 3）

阶段 3 的目标是**能看**：首页、媒体库、详情页、播放页外壳，能一路点进去，图片有占位与失败退化，
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
```

导航也是纯 Rust：

```rust
pub enum Route { Home, Library { id: String, title: String }, Detail { item_id: String }, Player { item_id: String } }
pub struct Navigator { /* 栈 + back() */ }
```

## 图片契约

界面**不下载图片**。它只做两件事：

1. 在视图模型里声明"我需要这些图片"：`ImageKey { item_id, kind }`（`Poster`/`Backdrop`/`Logo`）。
2. 提供 `set_image(&ImageKey, slint::Image)` 供 app 推入。

没推入时界面显示**占位**（纯色 + 首字母/图标），推入失败时显示**退化态**（与占位一样但不重试）。
缓存键必须含服务端的版本 tag（`ImageSet::poster_tag`），否则换图后永远命中旧图。

## 键盘 / 遥控器导航

| 键 | 行为 |
|---|---|
| `←` `→` | 行内移动焦点（首页是横向行，媒体库是网格） |
| `↑` `↓` | 行/网格之间移动 |
| `Enter` / `Space` | 打开详情 / 开始播放 |
| `Esc` / `Backspace` | 返回上一级（栈空时无动作） |
| `Ctrl+F` | 聚焦搜索框（阶段 3 只做输入与过滤本地缓存） |

焦点必须有**可见焦点环**（不是只靠颜色变化），并且焦点环与选中态要区分开。

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
| 底部（全宽，`#000` 0→80% 渐变到透明） | 第一行：标题 + 副标题（第几季第几集 / 年份）；第二行：进度条；第三行：左侧 播放/暂停、-10s、+10s、时间 `12:34 / 2:18:00`；右侧 音轨、字幕、倍速、全屏 |
| 左上 | 返回箭头（Esc 同义） |
| 中央 | 暂停时显示大播放图标（半透明，不挡字幕） |

进度条：已播（accent）/ 已缓冲（`#FFFFFF` 20%）/ 轨道（`#FFFFFF` 18%）；拖动时时间气泡跟随，
松手才真正 seek（Infuse 的手感），拖动期间画面不动。

## 快捷键（客厅遥控与键盘都要能操作）

| 键 | 行为 |
|---|---|
| `Space` / `K` | 播放 / 暂停 |
| `←` / `→` | -15s / +15s（与 Infuse 一致） |
| `Shift+←` / `Shift+→` | -60s / +60s |
| `↑` / `↓` | 音量 ±5% |
| `M` | 静音 |
| `A` | 音轨菜单 |
| `S` | 字幕菜单（含"关闭"与延迟 ±0.1s） |
| `F` | 全屏切换 |
| `Esc` | 退出播放（回到详情页），**退出前必须提交一次进度** |

## 音轨 / 字幕菜单

- 列表来自引擎真实枚举，不是猜的；当前项打勾。
- 选择后**不重启播放**（引擎侧切轨），并立刻写回用户偏好（下次自动选同一条）。
- 字幕菜单额外提供：关闭、延迟 -0.5s/+0.5s、外挂文件（本地路径）。
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
