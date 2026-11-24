# 路线图

每个阶段都有明确的**出口条件**。出口条件不满足就不进入下一阶段——尤其是阶段 1，
播放引擎选错会让后面所有 UI 工作白做。

## 阶段 0 — 骨架（已完成）

- [x] Cargo workspace、工具链、fmt/clippy/CI
- [x] `nova-core` 统一媒体模型 + 轨道选择 + 播放状态机（41 个单元测试）
- [x] 架构文档、ADR 流程
- 出口：`cargo test --workspace` 与 `cargo clippy -- -D warnings` 全绿

## 阶段 1 — 播放技术原型（选型已定：libmpv 零拷贝）

必须先做完，才能决定引擎，然后才开始写界面。

- [x] 环境摸底：GStreamer 1.28.9 / libmpv 0.42 / libVLC 3.0.24 头文件齐全
- [x] `spikes/playback-proto`：GStreamer 解码 → RGBA 帧，硬解探测，Slint 图像路径实测
- [x] `spikes/libmpv-proto`：libmpv SW 渲染 API headless 实测 + 逐段计时
- [x] libVLC 评估：vmem 路径会削弱硬解，Rust 绑定停更 → 排除
- [x] ADR-0001：主引擎定为 libmpv（OpenGL 零拷贝），GStreamer 备选，libVLC 排除
- [x] **Gate 1.5-A 真机通过**：Slint 窗口 + libmpv 零拷贝跑通（Arc B580，`hwdec-current=vaapi`），
      render p50 206 µs（1080p60）/ 274 µs（4K），CPU 0.07–0.14 核

出口条件：

1. ~~1080p60 一帧"解码 + 转 RGBA + 交给界面"的 p95 成本 < 8ms~~ → **已修正**：CPU 回读路径在 4K 上不可能达标
   （实测 21–70 ms/帧），真正的出口条件改为"**零拷贝路径**在 1080p60/4K30 下不掉帧"。
2. 三个平台上都能确认硬解**真实生效且无回读**（本机缺 VA-API 插件，Windows/macOS 未测）
3. 许可证与本项目兼容（ADR-0002）
4. 结论写进 ADR，含被否决方案与反转条件

### Gate 1.5（阻塞阶段 4，不阻塞阶段 2/3）

- [x] **A** Slint + libmpv OpenGL 零拷贝实机验证（`spikes/slint-mpv-gl`）
  - [ ] A' 解锁屏幕后补一次窗口模式（`render_mode=window`），确认 1080p60 无丢帧、缩放/全屏不崩
- [ ] **B** 真实 4K HEVC 10bit / HDR 片源 + Windows(d3d11va)/macOS(videotoolbox) 硬解实测
- [ ] **C** mpv `-Dgpl=false` 构建与三平台分发可行性
- [x] FFI 层按 ADR-0001"复用前必须加固"的五条改造（已在 `spikes/slint-mpv-gl/src/sys.rs` 落地）
- [ ] **D** `hwdec-current` 运行时断言 + 帧回调 watchdog（ADR-0001 明确要求，防静默降级）

阶段 1 没通过 Gate 1.5 就不能开始阶段 4（播放与进度）。但阶段 2/3 可以并行推进。

## 阶段 2 — 服务端接入（无界面）— 已完成

**不依赖播放引擎结论，可以与 Gate 1.5 并行。**

- [x] `nova-secrets`：`SecretString`（不可序列化、Debug 脱敏、`expose()` 才能拿明文）+ 平台凭证存储 + 内存实现
- [x] `nova-provider`：`MediaProvider` trait、错误模型、重试（POST 不重试）、地址校验、websocket 之外的 API 全覆盖
- [x] Jellyfin / Emby 共用映射核心 + 各自差异（`x-emby-authorization`、`Password` 字段、`X-Emby-Token` 响应头）
- [x] `nova-storage`：SQLite（WAL、`user_version` 前向迁移、事务回滚）、条目/资源/偏好/同步状态/图片 LRU
- [x] 真实 Jellyfin 开发服务端：`scripts/jellyfin-dev.sh`（含 Jellyfin 12 的初始化顺序坑，见 `docs/testing.md`）
- [x] 安全加固：拒绝基址 userinfo、拒绝 `..` 路径段、设备名严格校验、429 `Retry-After`、数组长度上限（见 provider 测试）
- [x] `nova-cli`：`login / servers / libraries / items / info / play / progress`，全部走 provider
- [x] 真机集成测试（Jellyfin 12.4.0）：登录 → 1 个库 → 8 条 → 详情时长 → 1 个资源 → `Range 206 video/mp4` → 进度上报
- [x] 跨进程会话恢复：进程 1 `login`，进程 2/3 不重新登录即可工作（`user_id` 落库 + 惰性 `/Users/Me`）
- [x] 离线缓存：`items --cached` 在服务端不可达时仍可列出条目
- [x] 防泄漏实测：SQLite 里密码与令牌出现次数均为 **0**，只存 `credential_key`

**出口条件已满足。** 全仓 251 个测试通过，clippy/fmt 干净。

### 阶段 2 遗留债（记录在案，不阻塞阶段 3）

1. **列表响应不带 `ParentId`**（Jellyfin 12.4 实测），所以 `MediaItem.parent_id` 在列表路径恒为 `None`，
   缓存无法按媒体库直接检索。最省事的修法：list 请求已知 `parent`，映射时用它回填。
2. **`Limit` 数的是服务端原始记录**：库里混着 `Folder` 时，`--limit 5` 可能只得到 3 条可播放条目。
   需要能表达"只要可播放类型"的查询参数。
3. **无法只发 `Stopped`**：用户中途退出且没有最后一次进度时，`PlaybackProgress` 表达不了"结束会话"。
4. `public_info()` 返回完整 `ServerInfo`（`auth=Unauthorized`、`user_id=None`），
   容易被误用来覆盖本地记录；应改为窄结构体 `PublicServerInfo`。
5. `Library` 没有被缓存（`nova-library` 落地时一起做）。
6. `nova-core` 的 `Page<T>` / `SortBy` / `Library` 仍在 provider 里，等 `nova-library` 一起上移。
7. `ServerInfo.version` 现在能填了（`authenticate()` 会拉一次 `/System/Info/Public`），
   但 `--json` 与界面展示尚未使用。

### 阶段 2 遗留债（记录在案，不阻塞）

1. `nova-core` 的 `Page<T>` / `SortBy` / `Library` 目前在 provider 里临时定义；等 `nova-library`
   做多服务器聚合时一起搬到 core，避免三处各写一份。
2. `MediaSource` 缺 `runtime`：单集时长来自 `MediaSources[].RunTimeTicks`，现在无处安放，
   界面显示时长前必须补（阶段 3 前）。
3. `ImageSet` 没有版本 tag：图片缓存以 URL 为键，服务端换图后 URL 不变会一直命中旧图。
   需要 `ImageTags` 参与缓存键（阶段 3 前）。
4. `Language` 的 ISO 639-2 归一化表现在 provider 里；播放引擎选字幕也会用到，届时应上移到 core。
5. `ServerInfo.version` 恒为 `None`（认证响应里没有版本号），要填就得额外请求一次
   `/System/Info/Public`；等有实际用途再补。

## 阶段 3 — 界面（只读浏览）— 主体完成

- [x] Slint 组件：首页、媒体库、详情页、播放页外壳（`crates/nova-ui`，11 个 `ui/*.slint`，设计令牌集中在 `theme.slint`）
- [x] 纯 Rust 视图模型（21 测试）：进度/时长/元信息格式化、badge 优先级、焦点网格移动、导航栈
- [x] 图片：`nova-images`（24 测试）——sha256(url+tag) 为键、原子落盘、坏缓存自愈、并发去重、LRU；界面只消费 `slint::Image`
- [x] 装配层 `nova-app`：先渲染 SQLite 缓存、后台刷新、令牌过期/未连接降级、图片并发 4
- [x] 无头快照测试（5 张 1280×800 PNG）+ 键盘导航测试
- [x] 端到端：真实 Jellyfin 数据渲染成快照（`/tmp/home-real.png`），真实海报与占位共存且不跳布局
- [ ] 搜索输入即时过滤（**被 nova-ui 的接口缺口卡住**，见下）
- [ ] 排序交互（目前只有静态外观）
- [ ] 真实窗口下的滚动/焦点手感（需要解锁屏幕）

**出口条件基本满足**：能一路点到详情页并看到元数据；播放页只是外壳（阶段 4 接引擎）。

### 阶段 3 遗留债

1. **`nova-ui` 缺搜索框的文本属性**：只有 `search-focused: bool` 与 `search()` 回调，没有输入文本，
   所以"输入即时过滤"接不上（过滤逻辑已在 `nova-app` 实现并可用 `--print-view --search` 验证）。
2. **`nova-ui` 没有横幅/通知位**：登录过期只能占用 hero 位置显示状态卡。
3. `nova-images::FetchError` 无法表达 provider 的错误分类（`FetchError::Transport` 需要不可构造的 `reqwest::Error`），
   provider 侧的 `Network/Decode/Server` 只能降级成"空体"。应把 `Fetcher` 的错误改成上层可自定义的泛型或 trait 对象。
4. SQLite 没有 libraries 表（`Library` 不是 `MediaItem`），离线启动时标签退化为"全部"。
5. 真实窗口模式（滚动、焦点手感、窗口缩放）未在解锁屏幕下验证。

## 阶段 4 — 播放与进度（已完成）

- [x] `nova-playback`：引擎无关的状态机/上报/恢复/选轨（55 测试，虚拟时钟，无 sleep）
- [x] `nova-playback-mpv`：libmpv OpenGL render API 零拷贝（27 单测 + 3 真机用例）
- [x] 引擎接进 `nova-app`：GL 在 Slint notifier 里、tokio 在后台、帧 watchdog、hwdec 诊断行
- [x] 播放页控制层（对标 Infuse）：渐变遮罩、进度三态、±15s、音轨/字幕菜单、3s 自动隐藏、快捷键
- [x] 起播与播放实测（Intel Arc B580，`hwdec-current=vaapi`）：

| 素材 | 首帧 | 渲染帧 | 丢帧 | 位置推进 |
|---|---|---|---|---|
| 1080p60 H.264 | **72 ms** | 298 | 1 | 5.00s / 5.11s |
| 1080p HEVC10 | 77 ms | 298 | 0 | 5.00s |
| 4K HEVC10 | **105 ms** | 92 | 0 | 3.00s |
| 1080p30 (120s) | 105 ms | 179 | 0 | 5.97s / 6.12s |

- [x] 服务端会话注册（`POST /Sessions/Playing`）与会话标识（`PlaySessionId`）——
  没有这两样时服务端日志是 `PlaybackStopped reported with null media info`，续播点永远不更新
- [x] **进度回写打通**（400 秒素材播 30 秒退出 → 服务端 `PlaybackPositionTicks=299666666`、`Played=false`）。
  根因是三个独立的缺陷叠加，都已修：
  1. 上报体缺 `PlaySessionId` → 服务端当成"无媒体信息的会话"直接丢弃（日志 `null media info`）
  2. 从未发 `POST /Sessions/Playing`（起播注册）→ 后续 Progress/Stopped 都无处归属
  3. `Stopped` 只在 `played=true` 时发送 → 正常退出永远不落盘。现在 `report_stopped` 是独立方法，
     由"退出播放"触发，与"看完"解耦
- [ ] 断线重连、缓冲提示（UI 已有位置，未接线）
- [ ] 字幕延迟/外挂字幕的界面入口已做，引擎侧 `sub-margin-y` 只做了字幕抬升
- [x] **续播闭环**：同一素材第二次起播从服务端位置继续（40s 退出 → 再播 3s → 服务端记 42.9s）。
  做法：起播前用 2 秒上限向服务端刷新条目与资源（失败/超时退回缓存），因为续播点只有服务端的是准的——
  只读本地缓存会出现"上次看到 45 秒，这次又从 0 开始"。
- [x] 长时间播放实测（150s）：位置 149.97s / 墙钟 150.14s（比值 0.999），丢帧 0
- [x] 内存与句柄实测（90s）：RSS 294,976 → 295,316 kB（+340 kB），fd 稳定 41，线程稳定 40
- [x] 自动重连：指数退避 0.5→8s、最多 5 次，重连期间显示"缓冲中"而不是错误卡
- [ ] **2 小时长跑未做**：本机只能连续跑到素材结尾（400s）。需要一段 ≥2 小时的素材或拼接播放，
      并且 `keep-open` 会掩盖"是否还在推进"，所以长跑脚本要按 `time-pos` 增量判断而不是看终点
- [x] **真实 Emby 联调**（4.10.2.0 / HTTPS / WAN）：登录、10 个媒体库、条目、直链 206、真实播放零丢帧
  —— 顺带修掉 4 个 Emby 差异（`/UserViews` 404、`/Users/Me` 500、无 `ProductName`、只认 `Pw`）
- 出口：进度与服务端一致（Jellyfin 已验证）；2 小时长跑（未做）；断线重连（已实现，**未能实测触发**——
  localhost + 整片进缓存，`docker pause` 50 秒都没造成断流）；Emby 的进度回写未闭环实测

### 服务端的续播策略（排查时会对你有用）

Jellyfin `UserDataManager.UpdatePlayState` 有三道闸门，任何一条不满足都会导致"位置不落盘"：

| 闸门 | 默认值 | 表现 |
|---|---|---|
| `MinResumePct` | 5% | 位置低于片长 5% → 位置清零 |
| `MaxResumePct` | 90% | 高于 90% 或距结尾 1 秒内 → 标记看完、位置清零 |
| `MinResumeDurationSeconds` | **300 秒** | **片长不足 5 分钟 → 直接标记看完** |

第三条最坑：用 2–5 秒的测试片验证续播，无论代码多正确都只会得到 `Played=true, position=0`。
验证续播必须用 ≥5 分钟的素材。

### 阶段 4 修掉的判定 bug

`--capture` 的通过判定只看"位置推进量"，碰上 `keep-open` 停在 EOF 就会误判（实测：660s 的请求
碰上 400s 素材，位置停在 400s，被报成"未通过"）。现在识别"已播到结尾"并单独注明。

### 阶段 4 已确认的事实（省得重踩）

- 首帧判定只能用"真的把一帧画进 FBO"，`VIDEO_RECONFIG` 与 `PLAYBACK_RESTART` 都会提前到达
  （`PLAYBACK_RESTART` 在纯音频文件上也触发）。
- `hwdec-current` 在首帧前读不到，此时**不能**判定降级（否则每台机器都会闪假警报）；
  且 `auto` 最终选中 `vaapi` 是成功，不是不匹配。
- mpv 0.42 不接受 `audio=yes`（`audio` 是轨道选择）与 `sub-ass-override=style`（合法值 `no/yes/scale/force/strip`），
  实现在启动时降级并 warn。

## 阶段 5 — 发布

- [ ] Windows (MSI) / macOS (dmg + 公证) / Linux (AppImage + Flatpak)
- [ ] 崩溃日志本地落盘、用户可导出
- [ ] 隐私说明与实际行为一致（零遥测）

## 未决问题

1. **许可证**：ADR-0002 待项目所有者确认。引擎已定为 libmpv，故实际选项只有
   "GPL-3.0-or-later"（与 Slint 开源版一致）或"自建 LGPL 版 libmpv + 更宽松许可"，
   后者要多担一份构建与合规成本。
2. **跨平台硬解**：Linux/Arc B580 已实测通过；Windows（d3d11va）与 macOS（videotoolbox）
   等价验证未做，这是"硬件解码 极高"这一项仅剩的空缺。
3. **图形字幕（PGS）**：必须由引擎渲染到画面上，不能用文本字幕层代替。
   这会把"字幕样式自定义"限制在文本字幕上。
4. **音频输出**：原型全程 `audio=no`。多音轨切换、直通（passthrough）、音画同步都还没验。
