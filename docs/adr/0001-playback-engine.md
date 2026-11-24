# ADR-0001：播放引擎选型

- 状态：**Accepted（Gate A 已通过；B/C 待补）**
- 日期：2026-11-16
- 相关：ADR-0002（许可证）

## 背景

Nova 要在 Windows / macOS / Linux 播放本地与 Emby/Jellyfin 提供的媒体，并把画面渲染进 Slint 窗口。
界面与引擎之间必须有稳定接缝，使引擎整体可替换：

```
nova-ui (Slint) ← VideoFrame 或 GL 纹理 ← nova-playback ← 引擎实现
```

引擎决定许可证、分发体积、硬解覆盖、HDR 能力，以及"字幕样式能不能自定义"，因此先做原型再定。

## 原型与证据

| 原型 | 覆盖 | 位置 |
|---|---|---|
| `spikes/playback-proto` | GStreamer 1.28.9 → NV12 → RGBA → Slint `Image`，硬解探测，PNG 导出 | `FINDINGS.md` |
| `spikes/libmpv-proto` | libmpv 0.42 SW 渲染 API → RGBA，headless，逐段计时 | `FINDINGS.md` |
| `spikes/slint-mpv-gl` | **Slint 窗口 + libmpv OpenGL render API 零拷贝真机路径** | `FINDINGS.md` + `evidence/*.png` |

测量环境：Ryzen 5 5600（6c/12t）、31 GiB、Arch。GPU 为 **Intel Arc B580（BMG G21，Mesa iris 26.3.1，OpenGL 4.6 硬件加速）**，
VA-API 驱动 iHD 26.3.5 支持 HEVC Main/Main10/Main12 与 AV1 解码。测量时无显示会话接入本 shell，
load ≈ 3–4。数字为 3 次运行 p50 中位数，单位 ms/帧。素材为 ffmpeg 本地生成
（`testsrc2` + 噪声），1080p/4K HEVC 10bit 分别约 49 / 47 Mbps。

### 关键前置事实：回读就是杀手（ffmpeg 独立验证，与引擎无关）

| 素材（全部帧跑完） | VA-API 解码零回读 | VA-API 解码 + 回读 | 纯 CPU 软解 |
|---|---|---|---|
| 1080p HEVC 10bit（90 帧） | 2.32 ms/帧（431 fps） | 4.88 ms/帧（205 fps） | 5.10 ms/帧（196 fps） |
| 4K HEVC 10bit（60 帧） | **4.28 ms/帧（233 fps）** | **20.5 ms/帧（49 fps）** | 12.2 ms/帧（82 fps） |

**4K 下一次 GPU→CPU 回读要 16 ms/帧，比 60Hz 的整个帧预算还多，使硬解比软解慢 2.4 倍。**
硬件本身完全不是瓶颈（零回读 4K60 只需 4.3 ms）。所以：

> 任何“引擎解好帧 → 转成 CPU 像素 → 交给界面”的架构，都把硬件解码的好处全部扔掉。
> 帧必须全程留在 GPU 上（dmabuf → EGL 导入 → 纹理合成），这不是优化，是唯一可行方案。

下面的 CPU 回读数字因此只用于两点：量化“回读代价”与提供无 GPU 时的兜底基线。

### 每帧成本（CPU 回读路径，仅作回读代价/兜底基线）

| 素材 | GStreamer 解码 | GStreamer 转 RGBA | libmpv 解码+渲染 | 再加"紧凑 RGBA + alpha" |
|---|---|---|---|---|
| 1080p H.264 8bit | 0.94–1.06 | 7.08¹ | 1.95 | 2.90 |
| 4K H.264 8bit | 3.70–3.87 | 27.8¹ | 14.94 | 22.8 |
| 1080p HEVC 10bit | 5.83 | 15.63¹ | 3.35（软解）/ 4.75（vaapi-copy） | 4.46 |
| 4K HEVC 10bit | 17.36 | 52.54¹ | **14.03（软解）/ 23.42（vaapi-copy）** | **20.3 / 29.5** |

¹ 本原型自写的标量 NV12→RGBA，不是 GStreamer 的 `videoconvert`。对照实测（整条管线跑完 90 帧）：
`avdec_h265 ! videoconvert ! RGBA` 在 1080p 10bit 上约 16.8 ms/帧、4K 约 63.7 ms/帧。
两者是同一量级：**8bit→8bit 的格式转换 GStreamer 很快（0.6 ms/1080p），10bit 降 8bit + RGBA 就非常贵。**

### Slint 侧的帧交付成本

| 路径 | 1080p | 4K |
|---|---|---|
| `SharedPixelBuffer::new` + memcpy（官方示例写法） | 1.13 ms（6.8%） | 5.6 ms（33.7%） |
| `SharedPixelBuffer::clone_from_slice` | 0.16 ms（**1.0%**） | 1.58 ms（9.5%） |
| `Image::from_rgba8` 本身 | ≈0 | ≈0 |

`Image::from_rgba8` 只做所有权转移，不拷数据；`new()` 会先清零再被覆盖一遍，是浪费。
**无 backend 时 `Image::from_rgba8` / `size()` / `to_rgba8()` / `clone` / `==` 全部不 panic**（catch_unwind 实测），
因为它只碰 `SharedVector` 与元数据。

## 原型得出的、改变结论的事实

1. **CPU 回读路径在 4K 上全面不合格。** 4K HEVC 10bit：GStreamer 17.4+52.5 ≈ 70 ms/帧（14 fps），
   libmpv 软解 14.0+4.0+3.1 ≈ 21 ms（47 fps）。4K60 无解，4K30 也无余量。
   与上面 ffmpeg 的 16 ms 回读代价互相印证：瓶颈是数据在总线上来回搬，不是解码器。
   → 必须走 GPU 零拷贝，CPU 帧只能作为兜底。
2. **`hwdec=auto-safe` 在回读设计里是陷阱。** 本机 mpv 选了 `vaapi-copy`，4K 下比软解**慢 67%**
   （23.4 vs 14.0 ms）——硬解省下的时间全被 GPU→CPU 回读吃掉。所以"能不能硬解"和"硬解有没有用"
   是两个问题，只有零拷贝路径才让硬解真正兑现。
3. **GStreamer 在本机的硬解元素数为 0**（Arch 的 `gst-plugin-va` 未安装；`nvcodec` 装了但 0 个元素因为缺 `libcuda.so.1`），
   而 mpv 自带 libva 后端、`hwdec-current` 立即生效。注意这是**打包差异而非能力差异**：
   本机 VA-API 完全可用（see 上表），装 `gst-plugin-va` 后 GStreamer 也能硬解。
   但对产品而言这就够了：分发时漏装一个插件，用户端就会静默退化成软解；而播放器分发的正确性
   不能依赖用户系统的包管理器。
4. **libmpv 的 SW 渲染是天然零拷贝**：mpv 直接把最终像素写进调用方缓冲区。
   如果消费者能接受 mpv 的 stride 与 `rgb0`/`bgr0`（第 4 字节实测为 0，不是 255），连那次 3–4 ms 的拷贝也可以省掉。
5. **`libmpv2 7.0.1` 无法使用 SW 渲染 API**（`RenderParamApiType` 只有 `OpenGl`，`render()` 硬编码 FBO+FlipY），
   原型改用 `libmpv2-sys` 直接 FFI，约 590 行（含注释）。这是**一次性成本**，且是 unsafe 集中区。
6. **Slint 官方参考实现已存在**：`slint-ui/slint` 仓库 `examples/gstreamer-player`，
   Linux 用 `gst-gl` + `glsinkbin` + EGL 把视频纹理交给 Slint（`BorrowedOpenGLTextureBuilder`），其他平台回退 CPU 缓冲。
   这既证明零拷贝可行，也说明**官方只覆盖了 Linux**。
7. **许可证**：本机 Arch 的 libmpv 是 GPL 构建（`List of enabled features` 含 `gpl`）；
   mpv 要 LGPL 必须 `-Dgpl=false` 自行构建。GStreamer 核心 LGPL-2.1+。

## 决策

**主引擎选 libmpv，走 OpenGL render API 零拷贝路径；GStreamer 保留为备选；libVLC 排除。**

主路径已经真机跑通，不再只是推断：

```
mpv (vo=libmpv, hwdec=vaapi) → 我们的 FBO + GL_RGBA8 纹理
  → BorrowedOpenGLTextureBuilder → Slint Image → femtovg 合成到窗口
```

实测（Intel Arc B580，Mesa iris 26.3.1，GLES 3.2）：

| 配置 | `hwdec-current` | pixelformat | render p50 | CPU |
|---|---|---|---|---|
| 1080p60 H.264 | `vaapi` | `vaapi` | **206 µs** | 0.14 核 |
| 1080p HEVC10 | `vaapi` | `vaapi` | 283 µs | 0.11 核 |
| 4K HEVC10 | `vaapi` | `vaapi` | **274 µs** | 0.12 核 |
| 4K HEVC10 缩到 1080p 窗口 | `vaapi` | `vaapi` | 188 µs | 0.07 核 |
| 1080p HEVC10（软解对照） | `no` | `yuv420p10` | 1362 µs | 0.53 核 |
| 4K HEVC10（软解对照） | `no` | `yuv420p10` | 3818 µs | 0.81 核 |

零拷贝的四个独立判据：`hwdec-current=vaapi` 而**非** `vaapi-copy`；`video-params/pixelformat=vaapi`；
mpv 日志 `Using EGL dmabuf interop via GL_OES_EGL_image`；
**`render` 成本与分辨率无关**（1080p 283 µs vs 4K 274 µs，而软解 1362 → 3818 µs 随像素线性增长）。
`glReadPixels` 一帧 960×540 要 2.1–7.1 ms，恰好是零拷贝路径上不存在的那一步。

证明“画面真的是视频”不靠截图工具：在 `AfterRendering`（GL 仍为当前）用 `glReadPixels` 读窗口帧缓冲，
算均值/方差/非黑比并断言，同时存 PNG。六组全部 `luma≈128.6`、`variance≈4400`、`nonblack=100%`、`pass=true`；
PNG 里能同时看到 mpv 的彩条、mpv 烤进纹理的 OSD 时间码、以及 Slint 绘制的覆盖层。

理由（按权重）：

| 维度 | libmpv | GStreamer | 判据 |
|---|---|---|---|
| 渲染集成 | 有专为"嵌进别人的 GL 上下文"设计的 render API，`libmpv2` 已暴露 `OpenGl` | 需要 `gst-gl` + `glsinkbin` + EGL 或 D3D11 互通，官方示例仅 Linux | libmpv |
| 多音轨与字幕 | 内建：轨道切换、外挂字幕、PGS 叠加、字幕样式 | 需自己做 `subtitleoverlay`/自研叠加，样式自定义受限 | libmpv |
| HDR | 内建 libplacebo 色调映射与色彩管理；实测能读出 `bt.2020`/`pq`/`max-cll` | 需自己搭 `glcolorconvert`/tonemap，10bit 链路复杂 | libmpv |
| 硬解 | 自带 ffmpeg + 平台后端，`hwdec` 一套配置覆盖三平台 | 依赖各平台插件包是否正确分发 | libmpv |
| 播放稳定性 | mpv 饱和使用十年以上，seek/断网/时钟处理成熟 | 框架本身成熟，但"播放器"层面的能力要自己拼 | libmpv |
| 分发体积 | libmpv + ffmpeg 依赖，量大头在 ffmpeg | Windows 全量 516 MB / macOS 运行时 150 MB | libmpv |
| 许可证 | 默认 GPL 构建；LGPL 需自建 | 核心 LGPL-2.1+ | GStreamer（但见 ADR-0002：Slint 本身即 GPLv3） |
| 工作量为 | 自写 FFI（约 590 行，一次性）、自管 render 上下文生命周期 | 自拼管线、自做字幕/色彩/HDR | libmpv |

**libVLC 排除**：`libvlc_media_player.h` 的 vmem 回调注释明确说明该路径会削弱/禁用硬件加速，
且字幕需自己做 CPU 合成；Rust 绑定 `vlc-rs` 停更于 2018；还要多吃一份 LGPL 代码与一份体积。
与 mpv 相比没有任何一项胜出。

## Gate 1.5：定稿前必须通过的三项验证

决策是**条件性**的，因为本机没有显示服务器，最关键的路径一次都没跑过。

### A. Slint + libmpv OpenGL 零拷贝（阻塞）— **已通过**
- 实现：`spikes/slint-mpv-gl`（mpv 渲染进自有 FBO+纹理 → `BorrowedOpenGLTextureBuilder` → Slint 合成）。
- 已证：真实硬件渲染、真实视频像素、正确方向、与 Slint 图层正确合成、GL 入口点全部来自
  Slint 的 `get_proc_address`（709 个符号、兜底 0 次，没链 libGL/libEGL）。
- **尚未证**：解锁屏幕下的“无撕裂/无丢帧”和窗口缩放/全屏切换。测量时会话处于锁屏状态，
  Wayland 合成器不发 `wl_surface.frame` 回调，程序回退到 `take_snapshot()` 驱动渲染（由 `--frames-out`
  的 PNG 与 `examples/redraw_probe` 对照组共同证实），因此 `frame_drop=22/280` 不能当作播放器丢帧。
  补测只需解锁后重跑 `FINDINGS.md §6` 的命令，看 `render_mode=window`。

### B. 真实片源 + 三平台硬解（阻塞 HDR 与"硬件解码 极高"两项评分）
- 真实 4K HEVC 10bit（含高码率与 B 帧）、HDR10、尽量含杜比视界样本。
- Windows：D3D11VA / MediaFoundation；macOS：VideoToolbox；Linux：VAAPI **零拷贝**（`hwdec=vaapi`，不是 `vaapi-copy`）。
- 通过标准：`hwdec-current` 为真硬解、零回读、4K30 稳定不掉帧。

### C. 分发构建可行性（阻塞发布计划）
- mpv `-Dgpl=false` 构建出的 libmpv 是否功能完整（部分滤镜/编码器会被排除，需确认不影响解码与渲染）。
- Windows：libmpv.dll + 依赖的最小集合与体积；macOS：dylib + 公证；许可证文件与源码提供方式。
- 若 LGPL 构建的 libmpv 不可用，则整体按 GPL 分发（与 ADR-0002 的建议一致）。

## 反转条件

出现以下任一情况即推翻本决策，改用 GStreamer：

1. Gate 1.5-A 无法在 Slint 的 GL 上下文里稳定工作，且 `gst-gl` 路径可行；
2. libmpv 的 LGPL 构建不可用，而项目最终要求非 GPL（ADR-0002 定为 MIT/Apache）；
3. Windows/macOS 上 libmpv 的分发或公证被平台政策阻断。

## 复用这套 FFI 前必须加固（代码审查结论）

原型的 `spikes/libmpv-proto/src/sys.rs` 生命周期目前是对的，但正确性依赖两个**隐式契约**。
搬进 `nova-playback-mpv` 之前必须改掉，否则一次字段重排就是 use-after-free：

| 位置 | 问题 | 要求 |
|---|---|---|
| `player.rs:34` | `Session` 靠字段声明顺序保证“先 free render context 再 terminate handle” | 写显式 `impl Drop`，不依赖布局 |
| `sys.rs:400` | `SwRenderer::drop` 里 `signal: Arc<FrameSignal>` 与裸指针并存，顺序靠字段顺序 | 改 `Option<Arc<_>>` + 显式 `take()` |
| `sys.rs:236` | `update_callback` 直接解引用 `data`，无空指针检查 | 入口加 `is_null()` 防御 |
| `sys.rs:181` | `MPV_EVENT_END_FILE` 分支直接解引用 `ev.data` | 同上 |
| `sys.rs:349` / `438` | `render_into` 不校验 `stride >= width*4`；`AlignedImage::new` 的 `stride*height` 未做溢出检查 | `checked_mul` + 入口断言，`AlignedImage` 字段私有化 |

这几条已经写进阶段 1 出口条件，不再重复列在会上。

## 后果

### 两个必须写进产品代码的坑（实测踩到）

1. **`hwdec=vaapi` 会静默退回软解。** 在 libmpv + render API 下，不传
   `MPV_RENDER_PARAM_DRM_DISPLAY_V2`（含 `render_fd=/dev/dri/renderD128`）时 mpv 报
   `Could not create a VA display.` 并退成 `hwdec-current=no`，用户完全无感。
   → `nova-playback-mpv` 必须：建 render context 时传 DRM render node；启动后**断言**
   `hwdec-current` 等于请求值，不等就在界面上明确提示“硬件解码未生效”，绝不允许静默降级。
2. **不能只靠合成器的 frame 回调驱动渲染。** Wayland 锁屏时合成器不发帧回调，
   `request_redraw()` 变成空操作。→ 需要帧回调 watchdog（超时则主动驱动），
   这也顺带解决部分合成器在窗口遮挡时的同类问题。

### 其它后果

- `nova-playback` 的接缝必须同时容纳两种交付形态：`VideoFrame`（CPU，用于兜底/截图/缩略图）
  与 `GL 纹理句柄`（零拷贝，主路径）。前者已证明（`SharedPixelBuffer::clone_from_slice` 1080p 仅 1.0% 帧预算），
  后者已真机跑通。
- 由于 `hwdec=*-copy` 在 4K 下比软解还慢，配置层**不得**默认使用 copy 类 hwdec。
- 主路径已锁定 OpenGL/EGL（`renderer-femtovg` + GLES）。Slint 的 WGPU 后端与 mpv 的 D3D11 渲染路径暂不考虑，
  但 Windows 上需要等价验证（D3D11 而非 GL）。
