# FINDINGS — playback-proto 实测记录

只记录本机实测数字与事实。不含选型建议（选型在 `docs/adr/`）。

## 0. 测量环境

| 项 | 值 |
|---|---|
| CPU | AMD Ryzen 5 5600（6 核 12 线程） |
| 内存 | 31 GiB（测量时约 15 GiB 已用） |
| 内核 | 7.3.5-zen1-1-zen |
| GStreamer | 1.28.9（dev 头文件齐全，1206 个元素注册） |
| gst 插件可用性 | `appsink` / `decodebin3` / `uridecodebin3` / `qtdemux` / `h264parse` / `avdec_h264` / `avdec_h265` / `videoconvert` 可用 |
| ffmpeg | n9.1（用于生成素材） |
| Rust | 1.100.0，workspace `[profile.dev] opt-level = 1` |
| 显示服务器 | 未使用。Slint 编译时 `default-features = false`，只开 `std` + `compat-1-2`；`cargo tree` 里没有 `winit` / 任何 backend feature，只有空的 `i-slint-backend-selector` |
| 后台负载 | 测量期间机器不是空闲的：load average ≈ 3.3，`qemu-system-x86` 占 ~137% CPU，另有桌面会话。下文"隔离测量"与"管线内测量"分开列，抖动原因见 §7 |

素材全部由 ffmpeg 现生成到 `target/testdata/`：

| 文件 | 参数 | 帧数 |
|---|---|---|
| `testsrc_320x240_2s.mp4` | testsrc2 320x240@30 2s，libx264 ultrafast，AAC 1ch 48k | 60 |
| `testsrc_1080p60_5s.mp4` | testsrc2 1920x1080@60 5s，libx264 ultrafast，AAC | 300 |
| `testsrc_4k30_3s.mp4` | testsrc2 3840x2160@30 3s，libx264 ultrafast | 90 |
| `hevc10_640x360_1s.mp4` | testsrc2 640x360@30 1s，libx265 `yuv420p10le`，`main-10` | 30 |

## 1. 解码器选择（运行时实测，不是预期值）

`probe` 用 `uridecodebin3`（内部就是 `decodebin3`）+ 管线上的 `deep-element-added` 信号收集实际插入的元素。

| 素材 | 视频解码器 | klass | rank | 判定 |
|---|---|---|---|---|
| H.264 320x240 / 1080p60 | `avdec_h264` | `Codec/Decoder/Video` | 256 | software |
| HEVC 10bit 640x360（`main-10`） | `avdec_h265` | `Codec/Decoder/Video` | 256 | software |
| AAC（所有带音轨的素材） | `avdec_aac` | `Codec/Decoder/Audio` | 256 | software |

实测元素链（H.264 + AAC 的 mp4，`deep-element-added` 顺序；320x240 与 1080p 结构相同）：

```
urisourcebin0 -> filesrc0 -> typefindelement0 -> parsebin0 -> typefind -> qtdemux0
-> h264parse0 -> capsfilter1 -> aacparse0 -> multiqueue1 -> identity0 -> identity1
-> avdec_h264-0 -> avdec_aac0
```

- `deep-element-added` **收不到** `uridecodebin3` / `decodebin3` 自身，只有它们内部创建的子元素（`urisourcebin`、`parsebin`、`multiqueue`、`identity`、解码器等）。
- HEVC 10bit 素材：解码器输出 `video/x-raw, format=I420_10LE`，`videoconvert` 转成 `NV12` 后才进 appsink（即 10bit → 8bit 的降位发生在 `videoconvert`）。所以本机 10bit HEVC 走软解 + 一次 10→8bit 转换。
- 两个 H.264 / HEVC 素材的 `appsink caps` 都协商到 `video/x-raw, format=NV12`。

### 硬解能力（本机事实）

| 插件 | 状态 |
|---|---|
| `va`（vaapi / vah264dec / vah265dec …） | **缺失** |
| `nvcodec` | 插件已安装 v1.28.9，但 **0 个元素**（`gst-inspect-1.0 nvcodec` 报 `Warning: CUDA library "libcuda.so.1" was not found`），所以 `nvh264dec` / `nvh265dec` / `nvdec` 均不可用 |
| `applemedia` / `d3d11` / `d3d12` / `mediafoundation` / `msdk` / `qsv` / `amfcodec` | 缺失 |
| `vulkan` | 已安装 v1.28.9，9 个元素，但白名单里的 `vulkanh264dec` / `vulkanh265dec` 不存在（该插件的 9 个元素都是 sink/convert 类） |

注册表中 `klass` 含 `Hardware` 的解码元素数量：**0**。
因此"这台机器能不能硬解 HEVC 10bit"的实测答案是：**不能**，且不是"理论上能但没选中"，而是本机没有任何可用的硬解解码元素（插件缺失 / 插件在但 CUDA 依赖缺失）。

`probe` 同时打印缺失元素白名单（`vah264dec, vah265dec, …, vulkanh265dec`，共 30 项）与 `GST_PLUGIN_FEATURE_RANK`（本机未设置）。

## 2. 三段耗时（release，median of 3 runs）

### 2.1 解码出帧 = `pull_sample()` 的帧间隔

测量方式：单独一趟、只拉帧不干别的（消费者不制造背压），所以间隔 ≈ demux/parse/解码/转 NV12 的生产速率。

| 素材（构建） | p50 | p95 | 吞吐 |
|---|---|---|---|
| H.264 1920x1080 @60（release，n=120×3） | 0.94 – 1.06 ms | 1.18 – 1.42 ms | 924 – 1048 fps |
| H.264 1920x1080 @60（dev，n=120×3） | 0.85 – 0.99 ms | 1.09 – 1.55 ms | 979 – 1155 fps |
| H.264 3840x2160 @30（release，n=90×3） | 3.70 – 3.87 ms | 4.44 – 5.39 ms | 258 – 276 fps |
| H.264 3840x2160 @30（dev，n=90×3） | 3.30 – 3.72 ms | 4.28 – 4.55 ms | 278 – 295 fps |
| HEVC 10bit 640x360 @30（dev，n=30） | 0.38 – 0.55 ms | 0.50 – 0.71 ms | 1961 – 2596 fps |

注意：如果和后续转换放在同一趟里，`pull_sample()` 的 p50 会变成 **0.013 ms**——因为解码线程早就把帧解好放在队列里，测到的是消费者自己的限速。所以本 bench 分两趟。

### 2.2 NV12 → RGBA（本项目里的标量 Rust 实现）

`convert::nv12_to_rgba`：整数定点 BT.709（≥720p）/ BT.601（SD），limited range，不读 caps 的 `colorimetry`。

| 素材 | 构建 | 复用目标缓冲 p50 / p95 | 每帧新建 Vec p50 / p95 |
|---|---|---|---|
| 1920x1080 | release | 7.08 – 7.16 / 7.35 – 7.88 ms | 6.68 – 6.70 / 6.80 – 6.91 ms |
| 1920x1080 | dev(opt-level=1) | 10.95 – 11.17 / 11.86 – 14.26 ms | 10.35 – 10.49 / 11.04 – 13.68 ms |
| 3840x2160 | release | 27.8 – 28.5 / 29.4 – 37.9 ms | 27.2 – 27.7 / 28.4 – 37.1 ms |
| 3840x2160 | dev(opt-level=1) | 42.6 – 43.4 / 45.9 – 54.4 ms | 42.0 – 42.3 / 43.2 – 53.6 ms |
| HEVC10 640x360 | dev(opt-level=1) | 1.15 / 1.49 ms | 1.15 / 1.27 ms |

换算：1080p release ≈ 292 Mpx/s，4K release ≈ 298 Mpx/s（都是"每像素约 3.4 ns"的标量实现）。
`NV12→RGBA` 是这三段里最贵的一段：1080p 占 16.6 ms 的 **43%**（release）/ **66%**（dev），4K 占 **170%+**。

### 2.3 memcpy 进 Slint buffer / `slint::Image` 构造

路径：`SharedPixelBuffer::<Rgba8Pixel>::new(w,h)` → `make_mut_bytes().copy_from_slice(rgba)` → `Image::from_rgba8(buffer)`。

隔离测量（`cargo test -p playback-proto --release --test proto -- --ignored --nocapture slint_path_isolated`，60 次/分辨率，median of 3 runs，管线未运行）：

| 分辨率 | `SharedPixelBuffer::new` | memcpy | 合计 `new`+copy | `clone_from_slice` 合计 | 纯 memcpy（复用目标缓冲） |
|---|---|---|---|---|---|
| 1920x1080 (8.3 MB) | p50 0.95 ms / p95 1.07 | p50 0.17 ms / p95 0.23 | p50 1.13 ms / p95 1.28 | p50 0.16 ms / p95 0.23 | p50 0.14 ms / p95 0.15 |
| 3840x2160 (33.2 MB) | p50 3.9 ms / p95 4.2 | p50 1.70 ms / p95 1.97 | p50 5.6 ms / p95 6.08 | p50 1.58 ms / p95 1.80 | p50 1.50 ms / p95 1.73 |

- `Image::from_rgba8` 本身 **≈0**：p50 显示为 0.000 ms（低于 0.001 ms 显示精度），p95 ≤ 0.001 ms（≤1 µs）。它只是把 `SharedPixelBuffer` 装进 `ImageInner::EmbeddedImage`，不拷数据。
- `new` 很贵是因为它做了 1.5 遍工作：`new` 先用 `Pixel::default()` 填满整块（RGB8/RGBA8 即清零），随后 `copy_from_slice` 再写一遍；1080p 0.95 ms 基本就是"33.2/8.3 MB 清零 + 缺页"，4K 3.9 ms 中相当部分是 glibc 大块 `mmap` + 缺页。
- 同样内容用 `SharedPixelBuffer::clone_from_slice(&data, w, h)`（内部按像素 clone，只拷一遍）总计：1080p 0.16 ms、4K 1.58 ms，与"纯 memcpy 复用缓冲"（0.14 / 1.50 ms）几乎相同。

管线内测量（release，边解码边测，median of 3 runs of `bench`）：

| 素材（素材尺寸路径） | alloc | memcpy | 合计 new+copy | clone_from_slice 合计 |
|---|---|---|---|---|
| 1080p，n=120 | 0.97 ms | 0.36 ms | 1.34 ms | 0.25 ms |
| 4K，n=90 | 3.9 ms | 1.89 ms | 5.81 ms | 1.71 ms |

与隔离测量的差异 ≤0.2 ms（1080p）/ ≤0.2 ms（4K），可以认为管线线程的干扰很小；个别运行出现过 4K copy p50 = 6.3 ms（≈5.2 GB/s）的异常值，同一轮里"纯 memcpy 复用缓冲"也是 6.2 ms，属于 §7 描述的系统级抖动，不是代码路径差异。

### 2.4 占 16.6 ms（1080p60 单帧预算）的比例

| 阶段 | 1080p p50 | 占 16.6 ms |
|---|---|---|
| 解码出帧（含 demux/parse + videoconvert→NV12） | 0.94 – 1.06 ms（release） | 5.7 – 6.4% |
| NV12→RGBA（复用缓冲，标量实现） | 7.13 ms（release）/ 11.02 ms（dev） | 43.0% / 66.4% |
| Slint 路径：纯 memcpy | 0.17 ms | **1.0%** |
| Slint 路径：`new` + memcpy + `from_rgba8` | 1.13 ms | **6.8%** |
| Slint 路径：`clone_from_slice` 总计 | 0.16 ms | **1.0%** |
| 三段合计（解码 + 转换 + Slint new+copy） | 9.2 ms（release）/ 14.1 ms（dev） | 55.4% / 85.0% |

4K（3840x2160）：解码 3.70–3.87 ms；NV12→RGBA 27.8–28.5 ms（release）；Slint `new`+copy 5.6 ms（33.7%）、纯 copy 1.70 ms（10.2%）、`clone_from_slice` 1.58 ms（9.5%）。三段合计 release ≈ 37.4 ms/帧。

## 3. 是否每次都必须整帧拷贝

是。读 slint 1.19.0 源码（`~/.cargo/registry/src/.../i-slint-core-1.19.0/graphics/image.rs`）确认：

- `pub struct SharedPixelBuffer<Pixel>` 内部是 `SharedVector<Pixel>`（image.rs:91），引用计数容器。
- `Image::from_rgba8(buffer)` 只是 `Image(ImageInner::EmbeddedImage { buffer: SharedImageBuffer::RGBA8(buffer), ... })`（image.rs:1035），**不拷贝**，但接管所有权。
- 没有"从 `&[u8]` 借用、零拷贝构造 `Image`"的公开 API：`new()`（image.rs:143，按 `Pixel::default()` 填满）和 `clone_from_slice()`（image.rs:163）都会写一遍数据。
- `make_mut_slice()`/`make_mut_bytes()` 走 `SharedVector::make_mut_slice` → `detach()`（`sharedvector.rs:230`），缓冲被共享（引用计数 >1）时会整块复制。

所以在"解码器输出 `Vec<u8>`（或 `gst::Buffer` 映射）→ Slint `Image`"这条路上，**每帧至少一次整帧拷贝**是不可避免的；能省的只是"清零再写"的第二遍（用 `clone_from_slice`，1080p 从 1.13 ms 降到 0.16 ms）。

## 4. 没有 backend 时 Slint 的真实行为（`catch_unwind` 实测）

`probe` 子命令里用 `std::panic::catch_unwind` 逐项实测，探测期间临时静音 panic hook。结果（本机，slint 1.19.0，`default-features = false, features = ["std","compat-1-2"]`，未初始化任何 backend）：

| 操作 | 结果 |
|---|---|
| `SharedPixelBuffer::<Rgba8Pixel>::new(64,36)` | ok：64x36，9216 字节 |
| `make_mut_bytes()` + `copy_from_slice` | ok：写入 9216 字节 |
| `Image::from_rgba8(...)` | ok：构造成功，`size=64x36` |
| `Image::size()` | ok：64x36 |
| `Image::to_rgba8()` | ok：返回 64x36 缓冲 |
| `Image::from_rgba8` + `clone` + `PartialEq` | ok：clone 后相等 |

结论：**没有 backend 时，`Image::from_rgba8` / `Image::size()` / `Image::to_rgba8` / `clone` / `==` 都不会 panic**。原因是这些操作只碰 `SharedVector` 里的像素和 `ImageInner::EmbeddedImage` 的元数据，不需要 renderer 或事件循环。

未被验证的部分（没做，也不在本原型范围）：需要 renderer 的操作（`Image::render_*`、创建 `Window`/组件、`invoke_from_event_loop`、软件 renderer 缩放）在无 backend 时的行为。`cargo tree` 显示 `i-slint-backend-selector` 编进来了但没有任何 backend feature，`winit` 不在依赖图里。

## 5. 遇到的 API / 实现坑

1. **`gst::Caps::from_string` 不存在**（0.25.4）：用 `text.parse::<gst::Caps>()`（`impl FromStr for Caps`）。
2. **glib 0.22 删掉了 `FromValueOptional`**：泛型 `Structure::get::<T>` 的约束要写 `for<'a> gst::glib::value::FromValue<'a> + 'static`。
3. **`gst::Fraction` 的 `Display` 输出 `30`，不是 `30/1`**：要 `format!("{}/{}", fraction.0.numer(), fraction.0.denom())`。
4. **`connect_deep_element_added` 在 `BinExt` 上，`Element` 不是静态 `Bin`**：改在 `pipeline`（`Pipeline: IsA<Bin>`）上连，子 bin 里的元素照样能收到。
5. **`VideoFrame::format()/plane_stride()` 在 `VideoFrameExt` trait 里**：必须 `use gstreamer_video::prelude::*`，否则报"method not found"。
6. **`VideoFrame::from_buffer_readable` 内部 `assert!(info.is_valid())`**：caps 解析失败时会 panic，调用前必须先判 `VideoInfo::is_valid()`。
7. **demuxer 的 sink pad / typefind 的 src pad 的 `current_caps()` 都是 `None`**：容器 caps 拿不到。最后用 `gst_pbutils::Discoverer`：顶层 `info.stream_info()` 才是 container（`video/quicktime, variant=iso`）；`info.container_streams()` 在该版本里不含顶层，返回空。
8. **`pull_sample()` 不能用来量解码耗时**（消费者慢时恒为 ~0），必须单独一趟纯拉帧。
9. **`SharedPixelBuffer::new` 是"清零 + 拷贝"两遍**；`clone_from_slice` 只拷一遍，1080p 差 7 倍（1.13 ms → 0.16 ms）。
10. `gst::ElementFactory::list_filter`（按 caps 列候选解码器）在 gstreamer-rs 0.25 里没有绑定，只能自己遍历 `Registry::features(ElementFactory::static_type())`。
11. `png` 0.17 的 `Encoder` 要显式 `set_color(ColorType::Rgba)` + `set_depth(BitDepth::Eight)`；dump 的 1080p PNG 约 456–469 KB/帧。

## 6. 其它实测事实

- `dump` 1080p 前 3 帧：`frame_0000.png` 456421 字节、`frame_0001.png` 465642、`frame_0002.png` 468975，文件头 `89 50 4E 47 0D 0A 1A 0A`，IHDR 宽高 = 1920x1080。
- `FrameSource::next_frame()` 每帧返回一个新的 `Vec<u8>`（1080p 8.3 MB / 4K 33.2 MB）；"每帧新建 Vec"比"复用缓冲"的转换成本高约 0.4–0.7 ms（release，1080p/4K），差异主要在分配与首次触碰内存。
- pts：mp4 + h264 baseline（无 B 帧）素材的 pts 严格递增（纳秒，`gst::Buffer::pts()`）；HEVC10 素材首帧 pts = 66666666 ns。
- 同一文件两次打开解码，第 1 帧像素完全一致（`tests/proto.rs::decoding_is_deterministic`）。
- 测试耗时：`cargo test -p playback-proto` 约 0.2 s（除 `slint_path_isolated` 外，全部用 320x240 / 2 s 素材）。

## 7. 已知噪声与局限

- 测量期间机器有 1.4 核被 qemu VM 占用、桌面会话在跑，load average ≈ 3.3。4K 的 memcpy 出现过 1.5 ms 与 6.3 ms 两态（33 MB：22 GB/s vs 5.2 GB/s），同一进程内、同一份代码；同一轮里"纯 memcpy 复用缓冲"同步变慢，说明是系统级内存带宽/缺页抖动而不是代码分支差异。隔离测量（`slint_path_isolated`）重复 3 次结果稳定在 1.50–1.72 ms，故 §2.3 表用隔离数据为主。
- dev profile（workspace 设 `opt-level = 1`）与 release 差距明显：NV12→RGBA 1080p 11.0 ms vs 7.1 ms，4K 42.6 ms vs 28.1 ms，Slint `new`+copy 1080p 2.19 ms vs 1.13 ms。引用数字时务必带构建类型。
- `NV12→RGBA` 的标量实现未做 SIMD/多线程优化，也未按 caps 的 `colorimetry`/range 选矩阵（`>= 720` 行硬编码 BT.709 limited range，SD 用 BT.601）。颜色正确性只由单元测试覆盖（黑/白/stride padding），没有和 `videoconvert` 做像素级对比。
- 没有验证任何开窗/渲染路径（本机无可用显示服务器，Slint 也没有编译任何 backend）。
- 解码器探测依赖 `deep-element-added`：`uridecodebin3` / `decodebin3` 自身不在该信号里；HTTP URL 路径未实测（本机有 `souphttpsrc`，但没跑过网络用例）。
