# 播放器调研结论（阶段 4 开工前）

日期：2026-11-16。来源标注：**[实测]** = 在本机验证；**[文档]** = 官方文档/源码；**[未验证]** = 无法确认。

## 一、HDR 的现实结论（影响产品表述）

**[文档+实测]** Slint 1.19 的 `BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture` 只接受 `GL_RGBA`（8bit）
纹理，Slint 自己的合成也是 8bit。所以：

> **HDR 内容必须在 mpv 侧色调映射到 SDR 8bit 再交给 Slint，没有 10bit 直通路径。**

这不是配置问题，是 Slint 纹理接口的硬限制。因此：

- `tone-mapping=auto`（gpu 默认 bt.2390）、`target-peak` 按显示器实际峰值设（SDR 默认 203 nits）、`dither-depth=auto`。
- 产品文案必须写"支持 HDR 片源的色调映射"，**不能**写"HDR 直通"。想要直通需要一条绕开 Slint 合成的独占全屏路径（未来 ADR，不在阶段 4）。
- 好处：`blend-subtitles=video` 让字幕在同一色调映射链里渲染，不会出现"画面暗了字幕还是亮的"。

## 二、libmpv 嵌入播放器的参数与调用约束

**[文档]** `MPV_RENDER_PARAM_ADVANCED_CONTROL=1` 后，渲染线程**绝不能等待 core**，否则死锁；
update callback 可能在"没有新帧"时也被调用，所以每次回调都要调 `mpv_render_context_update()`。
`mpv_render_context_report_swap()` 可选，但一旦用就要每次 present 后用（不一致会导致坏时序）。

**[文档]** `MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME` **默认 1**：`render()` 会阻塞到目标显示时间。
这正是 A/V 同步需要的；`--untimed` 对 render API 无效（我此前在 `spikes/libmpv-proto` 实测过
`untimed=yes` 帧间隔仍是 33.33ms，原因就在这里）——要测吞吐只能设 `BLOCK_FOR_TARGET_TIME=0`。

**[文档+实测]** `hwdec=vaapi` 在 render API 下必须传 `MPV_RENDER_PARAM_DRM_DISPLAY_V2.render_fd`
（`/dev/dri/renderD128`），否则 `create_drm_va_display` 返回 NULL 并**静默回退软解**
（源码 `video/out/hwdec/hwdec_vaapi.c`）。本机实测：不传 → `hwdec-current=no`；传 → `vaapi` + EGL dmabuf。

**[文档]** `video-sync=audio` 是最鲁棒的选择；`interpolation` 只在 `display-*` 模式有效，
`audio` 模式下开了会被静默忽略。我们用 UI 驱动的渲染循环，选 `audio` 并明确关掉 `interpolation`。

## 三、Infuse 的基线（对标，官方文档能给的部分）

**[文档]** 官方帮助中心**不公开**起播时间、控制层隐藏延迟、上报间隔这些数字，所以那些"Infuse 感觉"
只能作为设计目标，不能当作规格引用。能确认的行为：

| 项 | Infuse 的实际做法 | 出处 |
|---|---|---|
| 直连优先 | "Infuse will attempt to direct stream all content"（Plex 场景） | firecore.com/hc/…/360006462093 |
| 进度同步 | 观看历史 / 播放位置 / 评分 / 元数据**双向**同步 | …/4405110679447 |
| 音轨切换 | 即时切换快捷键（Cycle Audio/Subtitles） | …/217735707 |
| 字幕样式 | 文本字幕可调；**PGS 等图像字幕样式不可改** | …/215090967 |
| Atmos | 仅 E-AC3；TrueHD / DTS:X 不支持 | …/217735707 |
| 跳转步长 | macOS `←`/`→` = 15s，`⌘←`/`⌘→` 按章节 | …/215090987 |
| 杜比视界 | 发行说明提到改进 Profile 8 支持 | firecore.com/releases |

对我们的直接影响：**跳转步长改成 15s**（我先前写的 10s 不是 Infuse 的行为），
并且"字幕样式可调"要限定在文本字幕、UI 上明说 PGS 不可改样式。

## 四、Jellyfin / Emby 播放协议（本机 Jellyfin 12.4.0 实测）

| 用途 | 端点 | 认证 |
|---|---|---|
| 能力协商 | `POST /Items/{id}/PlaybackInfo`（body 带 `DeviceProfile`） | `Authorization: MediaBrowser Token=` |
| 原始文件直链 | `GET /Items/{id}/File`、`GET /Videos/{id}/stream?static=true` | 头（`static=true` 时 `api_key` 也行） |
| 重封装流 | `GET /Videos/{id}/stream`（无 `static`） | 头 |
| 转码/HLS | `PlaybackInfo` 返回的 `TranscodingUrl`（**相对路径**） | 头（去掉 `ApiKey` 后仍 200） |
| 外挂字幕 | `/Videos/{itemId}/{mediaSourceId}/Subtitles/{index}/Stream.{format}` | 头 |
| 会话上报 | `POST /Sessions/Playing`、`/Playing/Progress`、`/Playing/Stopped` | 头 + body 带 `PlaySessionId` |

要点：

- **交给 mpv 的 URL 不要带 `api_key`**：用 `http-header-fields=Authorization: MediaBrowser Token="…"`。
  实测去 `ApiKey` 后直链与转码 URL 都仍然 200，这样令牌不会出现在 mpv 的日志/错误里。
- `PlaybackInfo` 的 `DeviceProfile` 决定直连还是转码：`MaxStreamingBitrate` 调低即可复现
  `TranscodeReasons=ContainerBitrateExceedsLimit`。
- `POST /Sessions/Playing/Stopped` **可以单独发**（不需要先发 `Playing`），实测 204。
- `PositionTicks` 是 100ns 单位；`PlayMethod` ∈ `DirectPlay/DirectStream/Transcode`。
- `DeliveryMethod` 枚举：`Encode/Embed/External/Hls/Drop`；图形字幕通常是 `Encode`（烧录）。

### 安全发现（必须告知用户）

**[实测]** 本机 Jellyfin 12.4.0 上，`GET /Videos/{id}/stream?static=true` **在完全不带认证时也返回 206 文件字节**，
伪造 token 同样返回 206；而 `/Items`、`/UserViews`、`/Items/{id}/Download` 都正确返回 401。
OpenAPI 里该路径没有 `security` 声明。

也就是说：**任何知道条目 GUID 的人都能拉走原始文件**。这不影响我们的实现（我们仍然每个请求都带认证头，
不依赖这个行为），但对自建服务器的人来说是真实风险，应当在发布说明里提示用户检查 Jellyfin 版本/配置。

## 五、实现清单（引擎侧初始值）

```ini
vo=libmpv                     # 嵌入式渲染，必须有
gpu-api=opengl                # Slint 用 OpenGL 后端，上下文必须一致
hwdec=vaapi                   # Linux；Windows d3d11va、macOS videotoolbox
video-sync=audio              # 最鲁棒，不依赖显示 vsync
interpolation=no              # audio 模式下无效，显式关掉避免误解
blend-subtitles=video         # 字幕进同一条色调映射链
sub-auto=exact
sub-ass-override=style
tone-mapping=auto             # HDR → SDR
dither-depth=auto
audio=yes                     # 测试用 ao=null
```

render API 参数：`ADVANCED_CONTROL=1`（渲染线程不等待 core）、`BLOCK_FOR_TARGET_TIME=1`（A/V 同步）、
Linux 额外传 `DRM_DISPLAY_V2{render_fd}`。

首帧判定：`MPV_EVENT_PLAYBACK_RESTART` + `MPV_EVENT_VIDEO_RECONFIG` + render update 的
`MPV_RENDER_UPDATE_FRAME` 三者组合（官方没有单一定义，**需要实测确认**哪个最先到）。

## 六、未验证项（后续要补）

1. 图形字幕（PGS）的 `DeliveryMethod` 与服务端是否强制烧录（本机素材没有 PGS）。
2. `sub-file` 是否直接接受 http URL（手册未明确）；暂按"先下载到临时文件再 `sub-add`"实现。
3. PipeWire 下 `audio-spdif` 直通的现实可行性（Atmos/TrueHD 场景）。
4. Emby 的实际差异（本机没有 Emby）。
5. 首帧判定的三个事件在本机的到达顺序。

## 七、真实 Emby 联调（4.10.2.0，HTTPS + WAN）

服务端：一台真实的公网 Emby 实例（HTTPS，自备域名证书，`tls_verify_result=0`）。
具体主机名与账号不入库，只记录与判定逻辑相关的响应形状。

### 与 Jellyfin 的实质差异（都已在代码里处理）

| 差异 | 实测 | 影响 |
|---|---|---|
| `GET /UserViews` | **404**（Emby 没有这个端点） | 媒体库必须走 `/Users/{userId}/Views` |
| `GET /Users/Me` | **500** | 惰性解析用户标识在 Emby 上不可用；登录响应里的 `User.Id` 必须落库 |
| `/System/Info/Public` 无 `ProductName` | 只有 `LocalAddresses`/`RemoteAddresses` | 只靠产品名会把 Emby 认成 Jellyfin；改用"复数地址字段"兜底 |
| 认证密码字段 | 4.10 **只认 `Pw`**，发 `Password` 会 401 | 首选 `Pw`，401 时用 `Password` 重试一次 |
| `Limit` 语义 | 含 `Folder` 时 `--limit 3` 只返回 1 条（库总 9111 → 可播 4544） | 请求必须带 `IncludeItemTypes` |

### 播放实测（真实 WAN、4K HDR HEVC、15.6 Mb/s）

| 指标 | 值 |
|---|---|
| 首帧 | **4.97 s**（本机局域网 Jellyfin 是 55–105 ms） |
| 丢帧 | 0 |
| `hwdec-current` | `vaapi`（本地硬解，远端只负责发字节） |
| 直链 Range | 206 `video/x-matroska` |

**结论**：`--capture` 的首帧预算与"推进量"判定必须按链路区分。WAN 上首帧本身要几秒，
那几秒不可能推进；判定改为"首帧之后可用时间 × 90%"，首帧预算用 `--max-first-frame` 单独给。
把局域网口径当普适标准只会让工具说谎。

### 未验证

- Emby 上的**进度回写与续播**：代码路径与 Jellyfin 完全相同（已验证），但没有在 Emby 上完成
  闭环实测（`/Users/{id}/Items/{id}` 用临时脚本的鉴权写法拿到过 403，属于脚本问题，不是产品路径）。
- Emby 的图形字幕（PGS）烧录行为。
