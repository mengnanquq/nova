# libmpv software 渲染 API 原型：实测事实与数字

> 目的：给播放引擎选型的 ADR 提供与 GStreamer 方案**可比**的数据。
> 本文只写实测数字和事实，不写选型建议。
>
> 全部数字来自本机单次会话（2026-11，Arch Linux，12 核，load average ≈ 4）。
> 计时噪声约 ±10~15%，表中给的是 3 次运行的 p50 中位数。

---

## 0. 结论速览（一句话一条）

1. `MPV_RENDER_API_TYPE_SW` 确实能在**完全没有显示服务器**的环境里解出视频帧，不需要窗口、GL 上下文、Xvfb。
2. mpv 的 safe binding `libmpv2 7.0.1` **不支持** SW 渲染 API（`RenderParamApiType` 只有 `OpenGl`，`render()` 把参数硬编码成 FBO+FlipY），必须直接用 `libmpv2-sys 4.0.1` 做 FFI。
3. SW 后端是**天然零拷贝**：mpv 直接把最终像素写进调用方给的内存，不存在"mpv 内部截图 → 再拷出来"的中间步骤。
4. 但 libmpv **不给你"解码完的帧"**：`mpv_render_context_render()` 会把缩放 + 色彩转换 + 写内存一起做掉，成本 0.10~1.05 ms/帧（320×240 ~ 1920×1080）。
5. 帧的**供给节奏由播放时钟决定**，`--untimed=yes` 对 `vo=libmpv` 无效（实测）；要测吞吐必须把播放速度顶到上限（`speed=100`），否则测到的是 33.3 ms 的帧间隔。
6. 本机 `hwdec=auto-safe` **真的启用了硬解**：`hwdec-current=vaapi-copy`（GStreamer 侧没有 vaapi 插件，但 mpv 自己有 libva 后端）。vaapi-copy 反而比软解慢（1080p：2.59 ms vs 1.95 ms/帧）。
7. Arch 的 libmpv 是 **GPL 构建**（`List of enabled features` 里有 `gpl`），不是 `-Dgpl=false` 的 LGPL 版。
8. SW 渲染出来的第 4 字节是 **0**（不是 255）：`rgb0`/`bgr0` 的 alpha 必须自己补，Slint 一类的消费者要额外处理。

---

## 1. 环境事实

| 项 | 实测值 |
|---|---|
| mpv | `v0.42.0`，`libmpv.so.2.6.0`（`/usr/lib/libmpv.so.2.6.0`） |
| dev 头文件 | `/usr/include/mpv/{client.h,render.h,render_gl.h,render_d3d11.h,stream_cb.h}` |
| `render.h` 里的 render API | 只有三种：`MPV_RENDER_API_TYPE_D3D11 "d3d11"`、`MPV_RENDER_API_TYPE_OPENGL "opengl"`、`MPV_RENDER_API_TYPE_SW "sw"`。**没有 Vulkan** |
| `mpv --version` | 有 `built on`、`libplacebo`、`FFmpeg`、`FFmpeg library versions`；**没有** `built with`/`List of enabled features` 行（该字符串只存在于 .so 里，见 §7） |
| FFmpeg | 库 n9.0.2 / 运行时 n9.1；`ffmpeg -version` 的 configuration 含 `--enable-gpl --enable-version3` |
| 显示服务器 | **无**：`DISPLAY`/`WAYLAND_DISPLAY` 均为空，`xvfb-run`/`Xvfb` 未安装；`/tmp/.X11-unix/` 里有两个别人会话的 socket，但 `DISPLAY=:0 xdpyinfo` 报 `Authorization required`（本进程既没有授权也没有 DISPLAY） |
| GPU 设备 | `/dev/dri/card0`、`/dev/dri/renderD128` 存在 |
| VA-API | `vainfo` 可用：`VA-API 1.24`，Intel iHD 驱动 26.3.5，支持 `VAProfileNone/VideoProc` 等 |
| GStreamer | 1.28.9；**没有** `vaapi`/`vah264dec`/`vah265dec`/`nvdec` 插件；有 `avdec_h264`/`avdec_h265`/`decodebin3`/`appsink` |

关键配置（本原型的 `Session::open`）：`vo=libmpv`、`config=no`、`load-scripts=no`、`audio=no`、`idle=yes`、`keep-open=yes`、`hwdec=auto-safe`、`video-sync=desync`，且 **render context 必须在任何 VO 创建之前建好**（`render.h` 的 "Context and handle lifecycle"）。

实测反证：把 `vo` 覆盖成 `null`（`--opt vo=null`）后，SW 渲染拿不到帧，`first_frame` 20 s 超时 → `vo=libmpv` 不是可选项，是必要条件。

---

## 2. binding 结论：safe 封装走不通 SW

`libmpv2 = "7.0.1"`（源码 `libmpv2-7.0.1/src/mpv/render.rs`）：

* `pub enum RenderParamApiType { OpenGl }` —— 只有一个变体，没有 `Software`/`Sw`。
* `RenderParam` 枚举里没有任何 `SW_*` 变体（只有 `FBO`/`FlipY`/`Depth`/… ）。
* `RenderContext::render(&self, fbo, width, height, flip)` 内部固定构造 `FBO`(type 3) + `FlipY`(type 4) 两个参数，签名里连"渲染目标内存指针"的位置都没有。
* `From<&RenderParam<C>> for u32` 的映射表里 type 到 13 为止，而 SW 渲染需要 17/18/19/20。

结论：**在 `libmpv2 7.0.1` 里无法表达 `MPV_RENDER_API_TYPE_SW`**（不是"难用"，是类型上不存在）。
因此本 crate 只依赖 `libmpv2-sys = "4.0.1"`（bindgen 生成的原始绑定，prebuilt bindings 里常量齐全），
所有 `unsafe` 集中在 `src/sys.rs`，每个块都有 SAFETY 注释说明指针来源/所有权/生命周期。

踩到的坑（svc 层细节，写下来省得重踩）：

* `MPV_RENDER_PARAM_SW_FORMAT` 的 data 类型是 **`char*`（指向格式名）**，不是 `char**`。
  传 `&mut fmt` 会返回 `MPV_ERROR_UNSUPPORTED (-18)`。
* `mpv_render_param` 数组必须以 `{ type_: 0 /* MPV_RENDER_PARAM_INVALID */, data: null }` 结尾。
* stride 建议按 64 字节对齐（`render.h` 明确说不对齐可能触发慢路径甚至整帧拷贝）；本原型的 `AlignedImage` 用"多分配 + 手动对齐"满足，`stride = align_up(width*4, 64)`。
* 更新回调 `mpv_render_context_set_update_callback` 在 mpv 的内部线程调用，回调里**不能**调用任何 mpv API，也不能让 panic 越过 FFI 边界（用 `Mutex` 毒化后 `into_inner()` 兜底）。

---

## 3. headless 可行性：是的

`cargo test -p libmpv-proto`（本机无 DISPLAY/WAYLAND）5 个测试全绿，其中：

* `sw_render_produces_stable_frame_geometry`：连续 5 帧，尺寸恒为 320×240，stride=1280（=320×4，本身已是 64 的倍数），帧内容非单色。
* `padded_stride_keeps_pixels_in_place`：1300×48 素材（1300×4=5200 → stride 补到 5248），行首/行尾/第二行行首像素都是红 → 带 padding 的 stride 落盘正确。
* `pure_red_reveals_byte_order_of_both_formats`：见 §5。
* `dump_writes_valid_png_header`：PNG 签名 + IHDR(64×48, bit depth 8, color type 6) + IEND 校验。
* `probe_reports_software_decoding_in_this_environment`：`pixelformat` 是系统内存格式、SDR 素材不被判成 HDR、轨道数=2。

命令行同样在无显示环境跑通：`probe` / `bench` / `dump` 全部有输出（见 §3 末尾复现命令）。

---

## 4. 每帧成本（实测）

### 4.1 测量方法（先说清楚，否则数字没意义）

每帧的时间被切成互不重叠的四段：

| 段 | 含义 |
|---|---|
| `frame_gap` | 相邻两次渲染完成之间的间隔 = 拉帧 + 解码 + 渲染（端到端吞吐视角） |
| `render` | `mpv_render_context_render()` 调用本身的耗时 = SW 后端的缩放 + 色彩转换 + 写内存 |
| `copy` | 把 mpv 写好的缓冲区 memcpy 到"紧凑 RGBA"（stride=width*4）的参照拷贝 |
| `alpha` | 把第 4 字节写成 255（Slint 一类消费者需要） |
| `repeat` | 暂停后在同一帧上重复渲染（无解码、无新帧）→ 纯渲染成本 |
| `step` | 暂停后用 `frame-step` 逐帧推进：下发命令 → 帧就绪 → 渲染完成 |

前 10 帧作为 warmup 不计入。素材：h264 8-bit 4:2:0（`testsrc2` + 正弦 aac），300 帧样本。

### 4.2 主表（`speed=100`，p50 中位数，单位 µs）

| 源 | hwdec | 解码+渲染 (gap) p50 / p95 | 其中 `render` p50 | `copy` p50 | `alpha` p50 | 纯渲染 `repeat` p50 | `step` p50 |
|---|---|---|---|---|---|---|---|
| 320×240 @30 | auto-safe → **vaapi-copy** | **556** / 852 | 100 | 9 | 9 | 66 | 612 |
| 320×240 @30 | `no`（软解） | **330** / 398 ⚠时钟地板 | 81 | 8 | 9 | 70 | ⚠受循环 seek 污染 |
| 1280×720 @30 | auto-safe → vaapi-copy | **1386** / 2450 | 505 | 152 | 111 | 468 | 1651 |
| 1280×720 @30 | `no` | **678** / 880 | 442 | 96 | 119 | 360 | 1774 |
| 1920×1080 @30 | auto-safe → vaapi-copy | **2592** / 4092 | 1051 | 632 | 321 | 845 | 3099 |
| 1920×1080 @30 | `no` | **1949** / 2685 | 994 | 605 | 341 | 702 | 1309 |

图例/换算：

* 1080p 软解 ≈ **1.95 ms/帧 ≈ 513 fps 上限**（单线程串行渲染）。若按 60 fps 预算（16.6 ms），占 ~12%。
* 720p 软解 ≈ **0.68 ms/帧 ≈ 1475 fps 上限**；1080p vaapi-copy ≈ **2.59 ms/帧**。
* `render` 与 `repeat` 在 1080p 上分别是 994 / 702 µs：解码后的帧被 mpv 缓存着重复渲染更便宜（少了一次帧切换/图像重建）。

⚠ **时钟地板**：`speed=100` 下播放时钟仍以 333 µs/帧推进（= 33.33 ms / 100），所以任何测得 ≈330~600 µs 的行（320×240 全部、720p 软解的 gap）都是**上界**，真实成本可能更低。1080p 的行（1.9~2.6 ms）远高于地板，可信。

### 4.3 播放时钟：不能直接测成本（重要事实）

* `speed=1`（正常播放）：帧间隔恒等于 `1/container-fps`，实测 1080p 为 **33.32 ms**（p95 33.63 ms），其中 `render` 只有 1.46 ms —— 即 96% 的时间在等播放时钟。
* `--untimed=yes` **对 `vo=libmpv` 无效**：实测 `untimed=yes` 与 `untimed=no` 的帧间隔都是 33.33 ms（`mpv_get_property` 确认属性值确实是 `yes`）。`video-sync=desync` 同理无效。
* 有效的手段只有把播放速度顶到 mpv 的上限 `speed=100`：帧间隔被压到 `1/(fps*speed)`，成本才暴露出来。
* `mpv_render_context_render()` 默认还会**阻塞到该帧的目标显示时间**（`MPV_RENDER_PARAM_BLOCK_FOR_TARGET_TIME` 默认 1）。本原型在 bench 里把它设为 0，否则单次 `render()` 也会量到 33 ms。

### 4.4 成本随输出尺寸的变化（1080p 源，软解，单次测量）

| 渲染目标 | `render` p50 |
|---|---|
| 1920×1080 | 1181 µs |
| 1280×720 | 1130 µs |
| 640×360 | 954 µs |

输出像素少了 9 倍，成本只降 20% → **成本主要压在输入侧的 YUV→RGB/色度处理上**，不是压在每个输出像素的缩放上（缩小仍要读全分辨率输入）。此行为单次测量，噪声大，只作定性参考。

### 4.5 参数可调性（这台构建里没有的旋钮）

* `render.h` 提到的 `sw-fast` 选项在此构建里**不存在**（`mpv_set_option_string("sw-fast")` → `MPV_ERROR_OPTION_NOT_FOUND (-5)`；`mpv --sw-fast=help` 同样报 option not found）。
* `zimg` 也不是一个可 set 的 option（同样 `-5`）。所以本表的数字是**默认缩放/转换器**的成绩，没有调优空间可挖。
* `scale=bilinear` 对 1080p `render` 影响在噪声内（846 vs 800 µs，无缩放时本来就用不到 scaler）。

---

## 5. 帧格式：字节序与 alpha（实测 + 测试断言）

素材：`ffmpeg -f lavfi -i color=c=red:size=64x48 -c:v libx264 -pix_fmt yuv420p`。

| `MPV_RENDER_PARAM_SW_FORMAT` | 首像素实测 (前 4 字节) | 断言 |
|---|---|---|
| `rgb0` | `[254, 0, 0, 0]` | R>200、G<60、B<60 |
| `bgr0` | `[0, 0, 254, 0]` | B<60、G<60、R>200 |

* 名字与内存顺序一致：`rgb0` = R,G,B,X；`bgr0` = B,G,R,X。254 而非 255 是 h264 yuv420p 往返误差。
* **第 4 字节**：`rgb0` 与 `bgr0` 实测都把该字节写成 **0**（全帧取值集合 `{0}`，测试断言"统一值"）。`render.h` 只说它"未初始化、通常是 0"，因此不能依赖它是 alpha；要 alpha=255 必须自己补（1080p 补一次 ≈ 321~341 µs，见 §4.2 的 `alpha` 列，实现是逐像素写 1 字节）。

---

## 6. 零拷贝

* SW 后端的渲染目标**就是调用方给的内存**（`MPV_RENDER_PARAM_SW_POINTER` + `SW_STRIDE`）：mpv 直接往里写最终像素 → **不需要"把帧拷进我们的 buffer"这一步**，这一段的成本是 0。
* 前提条件（`render.h`）：stride 必须是像素大小的倍数、建议 stride 与指针都 64 字节对齐；本原型提供的 `AlignedImage` 两个条件都满足（例如 1920×1080 → stride 7680，本身已是 64 倍数；1366×768 → 5464 被补到 5504）。
* 因此 §4.2 里的 `copy` 列只是一个**上界参照**：如果你不得不把帧拷进一块"紧凑"（stride=width*4）缓冲区（1080p ≈ 605~632 µs），这就是那次拷贝的价钱。
* 最后一段 `alpha` 修正无法避免（除非消费者接受 alpha=0 或自己忽略 alpha）。

---

## 7. 解码路径实况（`hwdec=auto-safe`）

| 属性 | h264 8-bit | hevc 10-bit (合成 HDR10) |
|---|---|---|
| `hwdec-current` | **`vaapi-copy`** | `vaapi-copy` |
| `hwdec-image-format` | `no` | `no` |
| `video-params/pixelformat` | `nv12` | `p010` |

* 与预期不同：本机 GStreamer 没有 vaapi 插件，但 **mpv 自己带 libva 后端**，`auto-safe` 选中了 `vaapi-copy`（硬解 + 每帧下载回系统内存）。
* `vaapi-copy` 并不更快：1080p 上 `2.59 ms/帧` vs `--hwdec=no` 的 `1.95 ms/帧`（多出来的是 GPU→CPU 的那次拷贝）。720p 同理（1.39 vs 0.68 ms）。解码 8-bit h264 本身就是几毫秒级的事，硬解的收益被 copy 吃掉。
* `hwdec-image-format` 读出来是字符串 `no`（本机实测），不要把它当"图像格式"。它在真实硬解直通（`vaapi`、非 `-copy`）时才有意义，本机未出现该状态。
* 对 SW 渲染来说，硬件像素格式必须能映射到系统内存；`vaapi-copy` 恰好满足了（所以能正常出帧）。

---

## 8. 色彩/HDR 属性可用性（实测）

SDR 素材（bt.709 / bt.1886）与自造 HDR10 素材
（`-c:v libx265 -pix_fmt yuv420p10le -x265-params "hdr-opt=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=...:max-cll=1000,400"`）对比：

| 属性 | SDR | HDR10 |
|---|---|---|
| `video-params/primaries` | `bt.709` | `bt.2020` |
| `video-params/gamma` | `bt.1886` | `pq` |
| `video-params/colormatrix` | `bt.601`（SD 尺寸）/`bt.709` | `bt.2020-ncl` |
| `video-params/colorlevels` | `limited` | `limited` |
| `video-params/sig-peak` | 0.0 | **4.926108** |
| `video-params/min-luma` | （空） | 0.0001 |
| `video-params/max-luma` | （空） | 1000.0 |
| `video-params/max-cll` | （空） | **1000.0** |
| `video-params/max-fall` | （空） | **400.0** |
| `video-params/light` | `display` | `display` |
| `video-params/scene-max-r/g/b` | 空 | 空 |

* HDR 元数据的判定可以直接用 mpv 属性做：`primaries ∈ {bt.2020}` 或 `gamma ∈ {pq, hlg}`，`max-cll`/`max-fall` 就是 mastering/content-light 信息（与本机 `ffprobe -show_frames` 看到的 `Mastering display metadata` / `max-cll=1000,400` 一致）。
* `video-params/scene-*`（逐场景亮度）在本机为**空**，不要依赖它。
* 属性路径一次读一个标量即可（`mpv_get_property_string`），不需要解析 node 树；`track-list/count` + `track-list/N/{type,codec,demux-w,demux-h,demux-fps,demux-channel-count,demux-samplerate,lang,title}` 足够打印轨道表。
* 注意：`audio=no` 时 `audio-codec`/`audio-params/*` 全部返回不可用（这是 client.h 的 `MPV_ERROR_PROPERTY_UNAVAILABLE` 语义），轨道级信息要从 `track-list/*` 读。

---

## 9. 许可证结论

证据（全部本机实测）：

1. `pacman -Qi mpv` → `軟體授權: GPL-2.0-or-later AND LGPL-2.1-or-later`。
2. `strings /usr/lib/libmpv.so.2.6.0 | grep "List of enabled features"` → 该行存在，且**包含 `gpl`**（同时有 `ffmpeg libplacebo vaapi vulkan zimg dvdnav …`）。
   即 Arch 的 libmpv 是默认 GPL 构建，**不是** `-Dgpl=false` 的 LGPL 版。
3. `ldd /usr/lib/libmpv.so.2.6.0` 直接依赖 `libavcodec.so.63`（Arch 的 ffmpeg 用 `--enable-gpl --enable-version3` 构建）、`libdvdread.so.8`、`libdvdnav.so.4`、`libx264.so.165` —— 这些是 GPL-only 组件。
4. `mpv --version` 的 CLI 输出在这台机器上**没有** `built with` / `List of enabled features` 行（只有 `built on`、libplacebo、FFmpeg 版本），所以"用 `mpv --version` 看构建特性"在这台机器上不成立，要看 §9.2 的字符串或发行版元数据。

推论（事实层面）：
* 链接这份 `libmpv.so.2` 的二进制是 **GPLv2+ 衍生作品**；同时因为 Arch ffmpeg 是 `--enable-version3`，实际落点是 GPLv3 方向。本 workspace 自身声明 `GPL-3.0-or-later`，与这份 libmpv 不冲突。
* `libmpv2-sys` / `libmpv2` crate 自身的 license 是 LGPL-2.1（只覆盖 binding 代码），**不改变**被链接的 libmpv 的 GPL 性质。
* 要得到 LGPL 版 libmpv 必须自己用 `-Dgpl=false` 构建；那会去掉 dvdnav/dvdread 等 GPL-only 组件（本机没有该构建，未验证）。

---

## 10. 若要在 Slint 里用，必须绕的弯（只列事实）

1. **buffer 所有权**：Slint 消费的是 `slint::SharedPixelBuffer<Rgba8Pixel>`，其布局是紧凑的 `width*4`、无行尾 padding。mpv SW 渲染要求 stride 可对齐（建议 64 的倍数），两者只有在 `width*4 % 64 == 0` 时才重合。宽度不满足时（如 1366 → 5464）必须逐行拷进紧凑缓冲区：1080p 实测 ≈ 605 µs/帧。
   ⚠ Slint 是否提供"把外部内存零拷贝变成 `SharedPixelBuffer`"的稳定 API —— **未验证**（本机没有 Slint 依赖），需要按所选 Slint 版本核对其 API。
2. **alpha**：mpv 的 `rgb0`/`bgr0` 第 4 字节实测写 0。若 Slint 把 RGBA 当预乘 alpha，alpha=0 等于全透明，必须补 255（1080p ≈ 321 µs）。
3. **帧调度要自己写**：mpv 只在"有新帧"时回调，渲染时机、丢帧策略、与 UI 帧的合并都在应用侧；`mpv_render_context_render()` 默认会阻塞到目标显示时间，要么关掉（`BLOCK_FOR_TARGET_TIME=0`），要么接受它按播放时钟节流（§4.3）。
4. **帧尺寸/letterbox 要自己决定**：SW 渲染的目标尺寸由调用方给，mpv 会按"其他 VO 的显示规则"缩放并加黑边。想让画面和 Slint 布局对齐，要么用视频尺寸 + 自己做布局，要么给窗口尺寸 + 让 mpv 缩放（成本见 §4.4）。
5. **时间戳**：渲染 API 侧能拿到的是 `video-pts`/`time-pos` 这类属性；`render.h` 另有 `MPV_RENDER_PARAM_NEXT_FRAME_INFO`（给 `target_time`，mp_time 域），本原型未实测。
6. **构建/运行依赖**：需要系统里有 `libmpv.so` 与 dev 头文件（本机 `pkg-config --modversion mpv` = 2.6.0）；`libmpv2-sys` 的 build.rs 是 `cargo:rustc-link-lib=mpv`，没有 pkg-config 探测。

---

## 11. 与 GStreamer 方案相比：多出/少掉的具体工作量（只列事实）

**mpv 这边多出来的**（本原型实际写了/踩了才知道的）：

* 自己写 FFI 薄层：`src/sys.rs` 约 590 行（含注释），覆盖 `mpv_create/initialize/set_option/command/get_property_string/wait_event/terminate_destroy` + `mpv_render_context_{create,set_update_callback,update,render,free}` + `AlignedImage` 对齐分配。因为没有可用的 safe 封装（§2）。
* 自己处理 thread/回调契约：渲染更新回调不得调用任何 mpv API、不得 panic 越界；`mpv_render_context_free()` 必须在 `mpv_terminate_destroy()` 之前（结构体字段顺序/Drop 顺序成了正确性的一部分）。
* 自己处理播放时钟：`untimed` 无效 → 测吞吐必须先想清楚怎么绕过它（`speed`），实际播放时又要接受它按 fps 节流。
* 自己补 alpha、自己在需要时做 stride→紧凑的逐行拷贝。
* 自己决定渲染目标尺寸（=自己做 letterbox/缩放策略）。

**mpv 这边少掉的**（GStreamer 侧要写、这边不需要的）：

* 不需要 `appsink` + pipeline 状态机 + caps 协商：没有"协商成什么格式"这件事，目标格式由 `SW_FORMAT` 一个字面量决定（`rgb0`/`bgr0`）。
* 不需要 GStreamer 插件/元素存在性探测：解码能力由 mpv 内建的 ffmpeg 提供（例如本机 GStreamer 缺 vaapi/nvdec，而 mpv 自己能用 `vaapi-copy`）。
* 不需要 `gst-launch` 式拼装/`decodebin3` 元素追踪来回答"实际用了哪个解码器"：`hwdec-current` 一个属性就够。
* 不需要单独的"帧→RGB"转换步骤：SW 渲染把 YUV→RGB、缩放、letterbox 一起做完，直接落在最终缓冲区里。

**两边都要付的**：每帧至少一次"从解码器产物到可交给 UI 的 RGBA"的全画面处理（这里 ≈0.1~1.05 ms，见 §4.2 的 `render` 列），以及一次向 UI 侧的交付（零拷贝或 memcpy）。

---

## 12. 复现命令

```bash
# 素材（ffmpeg n9.1）
ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30:duration=5 \
       -f lavfi -i "sine=frequency=440:duration=5" \
       -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac -shortest hd720.mp4
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30:duration=5 \
       -f lavfi -i "sine=frequency=440:duration=5" \
       -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac -shortest hd1080.mp4
ffmpeg -f lavfi -i color=c=red:size=64x48:rate=10:duration=1 \
       -c:v libx264 -pix_fmt yuv420p red.mp4          # 字节序断言用
ffmpeg -f lavfi -i color=c=red:size=1300x48:rate=10:duration=1 \
       -c:v libx264 -pix_fmt yuv420p red_padded.mp4   # stride padding 断言用

# 构建/检查
cargo build -p libmpv-proto --release
cargo test  -p libmpv-proto
cargo clippy -p libmpv-proto --all-targets -- -D warnings
cargo fmt -p libmpv-proto -- --check

# 用法
./target/release/libmpv-proto probe hd720.mp4
./target/release/libmpv-proto bench hd720.mp4 -n 300                 # 默认 hwdec=auto-safe, speed=100
./target/release/libmpv-proto bench hd720.mp4 -n 300 --hwdec no      # 纯软解对照
./target/release/libmpv-proto bench hd1080.mp4 -n 200 --size 1280x720  # 指定渲染目标尺寸
./target/release/libmpv-proto dump  red.mp4 -n 3 -o out              # 导出 PNG
./target/release/libmpv-proto bench hd720.mp4 -n 100 -v              # 打开 mpv 自己的日志
./target/release/libmpv-proto bench hd720.mp4 -n 100 --opt <name>=<value>  # 透传任意 mpv option
```

---

## 13. 已知限制 / 未验证

* 单机单次会话的数据；load average ≈ 4 时同配置 3 次运行的 p50 波动 ±10~15%。
* `speed=100` 仍留下 333 µs/帧 的时钟地板，接近该值的行（320×240、720p 软解的 gap）只是上界。
* `step` 一列在 1 秒素材上会被 `loop-file=inf` 的循环 seek 污染（表现为 p50 ≈ 2 ms 且 min 极低），只有 720p/1080p（5 秒素材）的数字可信。
* 未测 4K、未测 10-bit 的 SW 渲染成本（`p010` 源只验证了 probe 的色彩属性，没跑 bench）。
* 未测 `MPV_RENDER_PARAM_NEXT_FRAME_INFO`（`target_time`）；未测 `MPV_RENDER_PARAM_ADVANCED_CONTROL`（开了之后必须严格在回调后调用 `update()`，否则会真死锁）。
* 未验证 Slint 侧能否零拷贝接收外部缓冲区（本机无 Slint 依赖）。
* 未验证 LGPL 版 libmpv（`-Dgpl=false`）的行为差异。
* 本原型只支持本地文件；HTTP/流式、seek、字幕/OSD 都没做（SW 渲染会把 OSD/字幕一起烤进画面，这一条是 `render.h` 的说法，本次未触发）。
