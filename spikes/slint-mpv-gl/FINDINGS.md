# Gate 1.5-A：Slint 窗口 + libmpv OpenGL render API 零拷贝 —— 实测记录

> 只写本机实测事实与数字，不写选型建议（选型在 ADR 里）。
> 素材：`target/testdata/`（HEVC 10bit 1080p ~48 Mbps / HEVC 10bit 4K / H.264 4K30）。
> 所有数字都不是估算：每一条都能用文末的命令复现。

---

## 0. 一句话

**成了**：Slint 窗口里显示的是 mpv 解码出来的真实视频画面（外加 Slint 自己画的半透明文字覆盖），
`hwdec=vaapi` 走的是真零拷贝（`hwdec-current=vaapi`、`video-params/pixelformat=vaapi`，
mpv 日志 `Using EGL dmabuf interop via GL_OES_EGL_image`），
1080p/4K 的 `mpv_render_context_render()` 都在 ~0.25 ms 量级，且几乎不随分辨率变化。

---

## 1. 运行环境（实测，不是假设）

| 项 | 值 |
|---|---|
| 会话 | KDE Plasma Wayland（`WAYLAND_DISPLAY=wayland-1`，`XDG_RUNTIME_DIR=/run/user/1000`） |
| **会话状态** | 本次测量期间 **`kscreenlocker_greet --immediateLock` 在跑 = 屏幕已锁**（见 §5.5，这件事影响很大） |
| GPU | `Mesa Intel(R) Arc(tm) B580 Graphics (BMG G21)` |
| GL | `OpenGL ES 3.2 Mesa 26.3.1-arch1.3`，GLSL `OpenGL ES GLSL ES 3.20`（vendor `Intel`） |
| mpv | `v0.42.0`，features 里有 `vaapi vaapi-drm vaapi-wayland egl dmabuf-interop-gl` |
| Slint | 1.19.0，`default-features=false` + `std, backend-winit, renderer-femtovg, compat-1-2`（femtovg → GLES3，glutin/EGL 后端） |
| 机器负载 | 测量期间 load average **11.2 ~ 14.2**（12 核，别的活也在占 CPU），下文数字有 ~10-20% 抖动 |
| DRM | 只有一个 render node：`/dev/dri/renderD128`（`card0` 存在但不给非 root 写） |

GL 身份是用 `glGetString` 从 Slint 给的函数指针里读的（不是 `eglinfo`、不是环境变量推断）：

```
GL renderer : Mesa Intel(R) Arc(tm) B580 Graphics (BMG G21)
GL vendor   : Intel
GL version  : OpenGL ES 3.2 Mesa 26.3.1-arch1.3        ← 是 GLES 而不是桌面 GL 4.6
GLSL        : OpenGL ES GLSL ES 3.20
```

---

## 2. 数据路径（本次验证的东西）

```text
mpv (vo=libmpv, hwdec=vaapi)
  │  VAAPI surface（GPU 显存里）
  │  EGL dmabuf interop（GL_OES_EGL_image）
  ▼
mpv_render_context_render(FBO=我们的 FBO, w=视频宽, h=视频高, FLIP_Y=0)
  │  颜色附件 = 我们自己 glGenTextures 的 GL_RGBA8 纹理（尺寸 = video-params/w×h）
  ▼
slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(texture_id, IntSize)
     .origin(BorrowedOpenGLTextureOrigin::BottomLeft).build()
  ▼
Slint 界面里的 Image（100%×100%，image-fit: contain）+ 半透明文字覆盖
  ▼
femtovg 把纹理合成到窗口 back buffer
```

关键点（全部实测）：

* GL 上下文只从 Slint 拿：`window.set_rendering_notifier(...)` 的
  `RenderingState::RenderingSetup` 里，从 `GraphicsAPI::NativeOpenGL { get_proc_address }` 取函数指针，
  用 `MPV_RENDER_API_TYPE_OPENGL` + `MPV_RENDER_PARAM_OPENGL_INIT_PARAMS` 建 render context，
  **然后** 才 `loadfile`（render context 先于任何 VO，符合 render.h 的 lifecycle 要求）。
* 每帧只在 `mpv_render_context_update()` 返回 `MPV_RENDER_UPDATE_FRAME` 时才 `render`：
  1080p 3 秒素材里 `render()` 的调用次数 = 显示的帧数（~82 次 / 2.8 s），没有“每帧重复渲染同一帧”。
* `MPV_RENDER_PARAM_FLIP_Y=0` + `BorrowedOpenGLTextureOrigin::BottomLeft` 是正确组合：
  回读出来的 PNG 方向正确（见 `evidence/*.png`，色条在上、mpv OSD 时间码在左下、Slint 覆盖文字在顶部）。
* 纹理尺寸随 `video-params/w × video-params/h` 变化重建 FBO（1080p→4K 切换时实测重建成功，
  4K 那两组的 `video=3840x2160`）。
* `AfterRendering`（GL 仍 current）用 `glReadPixels` 读**当前绑定的帧缓冲**：
  窗口路径下是窗口 back buffer（`GL_BACK`），`take_snapshot()` 路径下是 Slint 的离屏 FBO
  （`GL_COLOR_ATTACHMENT0`）。读到的像素直接算均值/方差/非黑占比，并写成 PNG。

---

## 3. 主表：三组对比（release，每组跑 2 次取中位数，顺序执行，非并行）

`mpv_render_context_render()` 是把解码结果渲染进我们 FBO 的那一次调用（不含解码、不含 Slint 合成）；
`回读 p50` 是 `glReadPixels` 读 960×540 窗口帧的耗时（= 任何“把像素搬回 CPU”的方案都要付的钱）。

| 配置 | hwdec-current | pixelformat | render p50 | render p95 | 回读 p50 | 端到端 | 丢帧 | 进程 CPU |
|---|---|---|---|---|---|---|---|---|
| HEVC10 1080p，`--hwdec vaapi` | **vaapi** | **vaapi** | **283 µs** | 614 µs | 3046 µs | 29.0 fps | 10.5 | **0.11 核** |
| HEVC10 1080p，`--hwdec no` | no | yuv420p10 | 1362 µs | 2557 µs | 2609 µs | 28.8 fps | 10 | 0.53 核 |
| HEVC10 4K，`--hwdec vaapi` | **vaapi** | **vaapi** | **274 µs** | 507 µs | 4492 µs | 29.5 fps | 6.5 | **0.12 核** |
| HEVC10 4K，`--hwdec no` | no | yuv420p10 | 3818 µs | 10947 µs | 6049 µs | 29.3 fps | 8.5 | 0.81 核 |
| H.264 4K30，`--hwdec vaapi` | **vaapi** | **vaapi** | 226 µs | 430 µs | 3524 µs | 29.2 fps | 10 | 0.12 核 |
| HEVC10 1080p，`--hwdec auto` | **vaapi** | **vaapi** | 244 µs | 456 µs | 2312 µs | 28.9 fps | 10.5 | 0.10 核 |

读法/边界：

* 端到端 fps 全部 ≈29，因为素材本身是 30 fps、且是**实时播放**（`speed=1`）。
  这一列只说明“跟得上”，不是吞吐上限。上限要看 `render` 列：
  3 ms/帧 ≈ 330 fps，0.27 ms/帧 ≈ 3700 fps。
* `丢帧` = `frame-drop-count`（mpv 渲染侧丢弃的帧）。10/90 左右主要是本节测量环境造成的：
  驱动渲染的方式每次都要做一次整窗回读（§5.5），会占用 GPU/CPU。
* `--hwdec auto`（= `auto-safe`）在本机现在也选中真零拷贝 `vaapi`（在 §5.1 的坑修掉之后）。
* 4K 软解的 p95 到 11 ms：10-bit HEVC 3840×2160 软解 + YUV→RGB 都在 CPU 上，
  单帧尖峰已经逼近 30 fps 的 33 ms 预算的 1/3。

### 3.1 窗口模式（合成器正常给帧回调）的单次参考

会话还没锁屏时跑的一次（当时 vaapi 还没通，所以这行是软解；`--hwdec` 走的是软解回退）：

| 配置 | render p50 | render p95 | 回读 p50 | 端到端 | 帧数 |
|---|---|---|---|---|---|
| HEVC10 1080p，软件解码，**窗口 back buffer 回读** | 1233 µs | 3074 µs | 3527 µs | 23.3 fps | 72 |

像素佐证：`evidence/window-mode-1080p-software-decode.png`（960×540，从窗口 back buffer 读的，
画面是测试图的彩条 + 移动的斜线 + 左下 mpv 自己的 OSD 时间码 + 顶部 Slint 的覆盖文字）。

---

## 4. 像素统计（“不是黑屏”的自动判定）

判定规则：`均值亮度 > 8` 且 `方差 > 100`（亮度 = Rec.709 加权，字节值直接算，不做线性化）。
每次运行最多取 6 个采样帧（第 2/10/20/32/48/70 帧；2 秒的 4K 素材只有 5 个）做 `glReadPixels`。

| 配置 | luma 均值 | 方差 | 非黑像素占比 |
|---|---|---|---|
| HEVC10 1080p（vaapi） | 128.7 | 4356 | 100.0% |
| HEVC10 4K（vaapi） | 129.0 | 4421 | 100.0% |
| H.264 4K30（vaapi） | 128.7 | 4444 | 100.0% |
| HEVC10 1080p（软解） | 128.6 | 4364 | 100.0% |

* 全部 6 个采样帧都满足判定（`pass=true`）；程序在判定失败时以退出码 3 结束。
* 这个素材是三基色竖条 + 移动图形，所以 luma 均值稳定在 ~128.7、方差 ~4360；
  **“稳定”本身是好事**：说明每次回读拿到的都是同一类画面（彩条铺满窗口），不是时有时无的黑帧。
* 自检用例（`cargo test`）同时覆盖了“全黑 0/0 必须判负”“均匀灰（均值够亮但方差 0）必须判负”。

回读的 PNG（960×540，行序已翻成自顶向下）：

* `evidence/snapshot-mode-1080p-vaapi-zero-copy.png`
* `evidence/snapshot-mode-4k-vaapi-zero-copy.png`（`video=3840x2160`，纹理按 4K 建、缩到 960×540 显示）
* `evidence/window-mode-1080p-software-decode.png`（窗口 back buffer 路径）

三张图里都能看到：画面是完整彩条（方向正确、没有上下颠倒、没有黑边错位），
顶部有 Slint 画的半透明条 + 文字（`mpv+Slint hwdec=vaapi pixfmt=vaapi 3840x2160 frames=19 …`），
左下角有 mpv 自己烤进纹理的 OSD 时间码 —— 后者顺带证明 mpv 渲染的目标确实是**我们的纹理**。

---

## 5. 踩到的坑（都能复现，按“卡住的严重程度”排）

### 5.1 libmpv + render API 下 `hwdec=vaapi` 必须由应用交出 GPU 原生句柄，否则**静默退回软解**

第一次跑 `--hwdec vaapi` 的结果是 `hwdec-current=no`（请求 vaapi、实际软解），mpv 日志（`-v`，`msg-level=all=v`）：

```
[libmpv_render/vaapi] Using EGL dmabuf interop via GL_OES_EGL_image
[libmpv_render/vaapi] Trying to open a x11 VA display...
[libmpv_render/vaapi] Trying to open a wayland VA display...
[libmpv_render/vaapi] Trying to open a drm VA display...
[libmpv_render/vaapi] Could not create a VA display.
```

原因（mpv 源码 `video/out/hwdec/hwdec_vaapi.c` + `video/out/gpu/libmpv_gpu.c`）：
interop 路径要 `ra_get_native_resource(ra, "x11" | "wl" | "drm_params_v2")`，
而 libmpv 模式下这些 native resource **只**来自 `mpv_render_context_create()` 收到的
`MPV_RENDER_PARAM_X11_DISPLAY` / `MPV_RENDER_PARAM_WL_DISPLAY` / `MPV_RENDER_PARAM_DRM_DISPLAY_V2`。
不传 → 三条路都拿不到 `wl_display`/`DRM fd` → `Could not create a VA display.` → mpv 无提示地退回软解。

修法（本原型采用）：`mpv_render_context_create` 时额外传一个
`mpv_opengl_drm_params_v2 { fd: -1, crtc_id: -1, connector_id: -1, atomic_request_ptr: null, render_fd: open("/dev/dri/renderD128", O_RDWR) }`。
之后日志变成：

```
[libmpv_render/vaapi] Trying to open a drm VA display...
[libmpv_render/vaapi/vaapi] libva: VA-API version 1.24.0 → iHD_drv_video.so → va_openDriver() returns 0
[vd] Using hardware decoding (vaapi).
[vd] Decoder format: 1920x1080 vaapi[p010] …
[cplayer] VO: [libmpv] 1920x1080 vaapi[p010]
```

注意：`--vaapi-device` 这个 option 对 interop 路径无效（那条路只用 `ra` 上的 native resource，
不走 `va_create_standalone`）；只有 `vaapi-copy` 才会用到 `--vaapi-device`。

### 5.2 Slint 的 `get_proc_address` 活不过一次回调，而 mpv 要的是长期 loader

`GraphicsAPI::NativeOpenGL { get_proc_address: &'a dyn Fn(&CStr) -> *const c_void }` 的引用只在
`set_rendering_notifier` 那次回调里有效；mpv 的 `mpv_opengl_init_params.get_proc_address` 是个 C 回调，
会在之后每次 `mpv_render_context_render()` 里随时被调用。

本原型的做法（`src/glproc.rs`）：

1. notifier 回调期间把 Slint 的 loader 放进 thread-local（`LoaderScope`，离开回调立刻清空，
   所以不存在悬垂引用）；
2. 解析出来的**函数指针**进进程级缓存（指针与上下文同寿命，可以长期保存）；
3. mpv 在 notifier 之外来要函数（正常不会发生）时，退回 `eglGetProcAddress` →
   `dlsym(RTLD_DEFAULT)` 兜底并计数。

实测每次运行 `loader_symbols=709`（共解析 709 个不同 GL 符号）、`loader_fallbacks=0`（兜底 0 次），
即这 709 个入口点（glow 自己用的 + mpv 要的）全部经 Slint 的回调解析；程序**没有链接 `libGL`/`libEGL`**。

### 5.3 Slint 1.19 没从根模块导出 `IntSize`

`BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(texture_id, size: IntSize)` 的 `IntSize`
是 `i_slint_core::graphics::IntSize`（= `euclid::Size2D<u32>`，注意是 **u32**），
`slint::` 根下**没有**它（只有 `BorrowedOpenGLTextureBuilder`/`Origin`）。
本 crate 因此额外依赖 `i-slint-core = "1.19"` 只为了这一个类型（另一个可选办法是
`slint::private_unstable_api::re_exports::IntSize`）。

### 5.4 `export component X inherits Window` 的根元素不能有 `id`

```
error: Unknown property id in Window
error: Cannot take reference of an element
```
根 `Window` 上写 `id: root;` 直接编译不过；子元素里不带前缀引用本组件的属性即可。

### 5.5 会话锁屏时 Wayland 合成器不再发 frame callback → Slint 只渲染一帧，`request_redraw()` 变成空操作

现象：一切正常但 `frames=0`、`mpv_render_context_render() not being called or stuck.`、
mpv 侧 `Dropped: 91`。测下来是这样的：

* 独立对照组（`examples/redraw_probe.rs`，**完全不含 mpv**，只开一个 Slint 窗口 +
  每 8 ms 一个 `request_redraw()` + 改一次文字）：`renders=1`，2 秒内一次都不再重绘。
* 同一时刻 `gdbus call --dest org.freedesktop.ScreenSaver /ScreenSaver.GetActive` → `(true,)`，
  `ps` 里有 `kscreenlocker_greet --immediateLock`。
* 会话没锁的时候同一路径是正常的：§3.1 那次 3 秒渲染了 72 帧（≈24 帧/秒，受 30 fps 素材和播放时钟限制）。

也就是：**winit/Wayland 的 `RedrawRequested` 依赖合成器发 `wl_surface.frame` 回调，锁屏时没有。**
这不是 mpv 的问题，也不是本原型的 bug，但没有它就没法在锁屏期间采数据。

本原型的处理：超过 400 ms 没等到任何一次渲染，就切到 `take_snapshot()` 主动驱动
（`slint::Window::take_snapshot()` 会跑完整的一轮渲染，包括 `RenderingSetup`/`BeforeRendering`/
`AfterRendering`，只是渲染目标是离屏 FBO）。报告里的 `render_mode` 字段会写明：

* `render_mode=window`：合成器驱动，`AfterRendering` 读窗口 back buffer（真正的“上屏路径”）；
* `render_mode=snapshot`：`take_snapshot()` 驱动，`AfterRendering` 读 Slint 的离屏 FBO。

代价（必须说清楚）：`take_snapshot()` 内部每次都会把整帧读回一次 CPU，
所以 snapshot 模式的 `render`/CPU 数字有系统性污染 —— 同一配置软解 1080p 的
`render p50` 在窗口模式是 1233 µs、在 snapshot 模式是 1317~1408 µs（相差 ~10%），
而 4K 软解的尖峰和“丢帧”会被放大得更多。**表 3 的全部行都是 snapshot 模式**
（测量时屏幕锁着），表 3.1 那一行是窗口模式。

### 5.6 GL 状态必须自己保存/恢复

`render_gl.h` 明说 mpv 只把上下文“尽量”还原到默认值，明确不还
`glViewport` / `glScissor`（box）/ `glBlendFuncSeparate` / `glClearColor`，且会关掉 `GL_DITHER`。
本原型在 `render_into()` 前后保存/恢复
`FRAMEBUFFER_BINDING`、`VIEWPORT`、`SCISSOR_BOX`+`SCISSOR_TEST`、`BLEND`、`CLEAR_COLOR`、
`ACTIVE_TEXTURE`、`TEXTURE_BINDING_2D`。恢复之后 femtovg 照常画（覆盖文字没花、没丢），
可以认为这个状态集合够用。

### 5.7 `frame-drop-count` 必须在 session 活着的时候读

本原型的资源释放发生在 `AfterRendering`（GL 还 current 的时候），此后 `mpv_handle` 已经销毁，
报告阶段再 `get_i64("frame-drop-count")` 一律返回 `None`。
改成播放过程中每 tick 采样一次才拿得到数（现在是 `frame_drop=10` 这种真实值）。

### 5.8 winit 的事件循环一个进程只能建一次

`tests/live.rs` 里如果在一个测试进程里连开两个窗口，第二个会直接失败：

```
Error initializing winit event loop: EventLoop can't be recreated
```

所以 `tests/live.rs` 的每个用例都 `std::process::Command` 另起一个 `slint-mpv-gl` 进程。

### 5.9 其它小坑

* `mpv_opengl_fbo::internal_format` 要填 `GL_RGBA8`（0x8058），否则 mpv 只能猜目标格式（影响 dithering）。
* 参数数组必须以 `{ type_: 0 /* MPV_RENDER_PARAM_INVALID */, data: null }` 结尾。
* `mpv_render_param` 里的 `FLIP_Y`/`BLOCK_FOR_TARGET_TIME` 都是 `int*`（不是 bool*）；
  `BLOCK_FOR_TARGET_TIME=0` 很重要，否则 `render()` 会阻塞到该帧的显示时间（UI 线程被播放时钟绑住）。
* libmpv 默认**不**给客户端发日志，`terminal=yes` 也不够；要 `mpv_request_log_messages("v")`
  才会收到 `MPV_EVENT_LOG_MESSAGE`。上面 §5.1 的日志就是这样拿到的。
* 4K HEVC 10-bit（~48 Mbps）硬解时偶发 p95 尖峰（500 µs ~ 4 ms），1080p 没有；一次运行内
  4K 的 `frame_drop` 比 1080p 少（6.5 vs 10.5），推测与素材只有 2 秒、以及首帧 interop 建立有关（未验证）。

---

## 6. 交付物 / 怎么跑

```bash
# 构建 & 检查
cargo build --release -p slint-mpv-gl
cargo fmt  -p slint-mpv-gl -- --check
cargo clippy -p slint-mpv-gl --all-targets -- -D warnings
cargo test -p slint-mpv-gl                       # 12 个纯 CPU 用例（不建窗口）
# 需要真窗口 + GPU 的端到端用例（每个用例另起进程，KDE 锁屏时会自动走 snapshot 模式）
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
  cargo test -p slint-mpv-gl --test live -- --ignored --nocapture

# 跑一次
export WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000
./target/release/slint-mpv-gl target/testdata/hevc10_1080p_3s.mp4 \
    --seconds 5 --hwdec vaapi --frames-out /tmp/frames

# 三组对比（表 3 的来源）
./target/release/slint-mpv-gl target/testdata/hevc10_1080p_3s.mp4 --seconds 5 --hwdec vaapi
./target/release/slint-mpv-gl target/testdata/hevc10_1080p_3s.mp4 --seconds 5 --hwdec no
./target/release/slint-mpv-gl target/testdata/hevc10_4k_2s.mp4    --seconds 5 --hwdec vaapi
./target/release/slint-mpv-gl target/testdata/hevc10_4k_2s.mp4    --seconds 5 --hwdec no
./target/release/slint-mpv-gl target/testdata/testsrc_4k30_3s.mp4 --seconds 5 --hwdec vaapi

# 验证“锁屏导致合成器不发帧回调”的对照组（不含 mpv）
WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
  cargo run -p slint-mpv-gl --example redraw_probe -- 2
```

环境变量（调试/多 GPU 用）：

| 变量 | 作用 |
|---|---|
| `SLINT_MPV_GL_FORCE_SNAPSHOT=1` | 强制用 `take_snapshot()` 驱动（不需要合成器帧回调） |
| `SLINT_MPV_GL_RENDER_NODE=/dev/dri/renderD129` | 指定给 mpv 的 DRM render node |

CLI：

```
slint-mpv-gl <file> [--seconds 8] [--hwdec vaapi|no|auto] [--size 960x540] [--frames-out outdir] [-v]
```

`RESULT ...` 那一行是给脚本用的（本次的表就是这么拼出来的），含
`hwdec_current / pixfmt / video / window / gl_renderer / frames / e2e_fps / render_p50_us /
render_p95_us / readback_p50_us / frame_drop / decoder_frame_drop / luma / variance / nonblack /
loader_fallbacks / loader_symbols / pass / render_mode / cpu_seconds / cpu_cores`。

---

## 7. 零拷贝判定依据（只列证据）

1. **`hwdec-current=vaapi`，不是 `vaapi-copy`**；`video-params/pixelformat=vaapi`
   （软解时是 `yuv420p10`）。`-copy` 变体的特征是把解码结果下载回系统内存，
   `pixelformat` 会变成系统内存格式（`p010`/`nv12`）。本机实测：
   * 不传 5.1 里那个 DRM 参数时：`hwdec-current=no`、`pixfmt=yuv420p10`（软解）；
   * `--hwdec vaapi-copy` 未在本原型里跑（`pixelformat` 预期是 `p010`，未验证）。
2. **`render` 成本不随分辨率变化**：1080p 283 µs / 4K 274 µs（vaapi），
   而软解是 1362 µs / 3818 µs。像素数增加 4 倍时，软解路径涨了 2.8 倍，
   vaapi 路径基本不动 —— 说明 vaapi 路径不是逐像素在 CPU 上做 YUV→RGB/上传，
   而是把解码好的 GPU surface 直接 blit 进目标纹理。
3. **CPU 占用**：整个进程平均 0.11~0.12 个核（vaapi）vs 0.53（1080p 软解）/ 0.81（4K 软解）。
   其中还包含了 Slint 渲染和 snapshot 模式的额外回读。
4. **“下载到 CPU”的价钱被单独量出来了**：`glReadPixels` 读一帧 960×540 要 2.1~6.0 ms
   （实测 2.1~7.1 ms，中位数 ≈3.5 ms）。零拷贝路径上不存在这一步 —— 回读只在 6 个采样帧上为了验证才做。
5. 反证：mpv 日志里 `Using EGL dmabuf interop via GL_OES_EGL_image` +
   `Decoder format: 1920x1080 vaapi[p010]` + `VO: [libmpv] … vaapi[p010]`，
   说明进 VO 的就已经是 VAAPI surface，没有 hwdownload。

注意“零拷贝”的边界（事实层面）：**CPU↔GPU 之间不搬像素**，但 mpv 仍然要在 GPU 上把解码 surface
渲染（blit/色彩转换）进我们的 FBO 纹理，这 0.25 ms 量级的 GPU 工作省不掉；
它也不是“直接把解码器 surface 的 dmabuf 当纹理零处理地绑定给 Slint”那种更激进的零拷贝。

---

## 8. 已知限制 / 未验证

* 表 3 全部是 `render_mode=snapshot`（测量时屏幕锁着）。窗口模式的完整四组数据**没采到**：
  只在会话解锁时采到一组（表 3.1，软解 1080p）。要补的话：解锁后重跑 §6 的命令即可
  （报告里 `render_mode=window` 即为窗口模式，`AfterRendering` 读的是真正的窗口 back buffer）。
* snapshot 模式对 `render`/CPU/丢帧有系统性污染（§5.5），数值只能做**同模式内**比较，
  不能直接和窗口模式的数字混用。
* 未测 vsync/present 时序（`mpv_render_context_report_swap()` 有调用，但没有量化它对 mpv 计时的影响）。
* 未测 HDR 输出（素材是 SDR；HDR 会涉及 `--target-colorspace-hint`/EOTF 映射，本原型没碰）。
* 未测 `MPV_RENDER_PARAM_ADVANCED_CONTROL`（开了以后必须严格在回调后 `update()`，风险高，本原型没开）。
* 未测多窗口/上下文重建（Wayland surface 重建后 render context 是否还能用 —— 没做恢复逻辑）。
* 未测 `vaapi-copy` 与本原型的组合（用来量化“硬解 + 一次下载”的成本）。
* 4K HEVC 10-bit 的 p95 尖峰（0.5~4 ms）原因未定（推测与首帧 interop 建立/调度有关）。
* 素材只有 2~3 秒，`--seconds 5` 里后半段是 `keep-open` 的最后一帧（程序在 EOF 后 1 秒提前退出，
  报告里的 `duration_s` 就是实际统计窗口）。
