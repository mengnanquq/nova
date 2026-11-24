# playback-proto — 第一阶段播放引擎原型

目标只有一个：**用可复现的数字，回答"哪个播放引擎能接进 Slint、成本多大"**。
不是播放器，不做 UI，不做进度保存。

## 环境事实（本机实测，2026-11）

| 项 | 状态 |
|---|---|
| GStreamer | 1.28.9，`gstreamer-*-1.0.pc` 齐全 |
| GStreamer 插件 | `appsink` / `glimagesink` / `decodebin3` / `avdec_h264` / `vp8dec` 有；**`vaapi` / `vah264dec` / `vah265dec` / `nvdec` 没有** |
| mpv / libmpv | 2.6.0 dev 头文件可用 |
| libVLC | 3.0.24 dev 头文件可用 |
| ffmpeg | n9.1（用于生成测试素材） |
| 显示服务器 | **无**（无 X11 / Wayland / Xvfb） |
| GPU | `/dev/dri/card0` + `renderD128` 存在 |

因为没有显示服务器，本次原型**完全不验证开窗**。要验证的是"帧怎么来、每帧成本多少、
Slint 那边怎么接"，这四件事都是 headless 可测的。

## 必须交付

### 1. 库 API

```rust
/// 一帧解码后的图像，RGB 分量在内存里连续排列。
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: usize,          // 每行字节数，可能 > width*4
    pub data: Vec<u8>,          // RGBA8
    pub pts_ns: Option<u64>,    // 显示时间戳
}

/// 帧来源抽象：GStreamer 现在是唯一实现，mpv/libVLC 将来可插进来。
pub trait FrameSource {
    fn next_frame(&mut self) -> Result<Option<VideoFrame>, Error>;
    fn description(&self) -> SourceDescription; // 实际选中的解码器、是否硬解、协商后的 caps
}
```

`description()` 必须如实反映**运行时实际选择**，不能写死预期值。

### 2. 命令行

```
cargo run -p playback-proto -- probe <media>            # 打印解码器/硬解/caps/流信息
cargo run -p playback-proto -- dump  <media> -n 5 -o out # 导出前 N 帧为 PNG
cargo run -p playback-proto -- bench <media> [-n 300]   # 解码 N 帧，报 p50/p95 每帧耗时
```

`<media>` 支持本地路径；HTTP URL 若实现成本低也可支持（用 `playbin3` 或 `souphttpsrc`）。

### 3. 硬解探测（重要）

`probe` 必须回答"这台机器能不能硬解 HEVC 10bit"，而**不是**"理论上能"。做法：
检查 `decodebin3` 实际插入了哪个 `GstElement`（`pad-added` 后的 `child-added` / `deep-element-added` 信号），
打印元素名与 factory 属性，并明确区分：

- `vaapi` / `nvdec` / `vtdec` / `d3d11` 等 → 硬解
- `avdec_*` / `vpxdec` / `openh264dec` → 软解

同时打印 `GST_PLUGIN_FEATURE_RANK` 相关诊断与缺失的插件提示，方便跨平台排查。

### 4. Slint 集成实测（关键结论）

在一个 feature（`slint`）后面加入 Slint 依赖，**不建窗口**，只测这条路：

```text
VideoFrame(RGBA Vec<u8>) -> slint::SharedPixelBuffer<Rgba8Pixel> -> slint::Image
```

要求给出：

- 每帧从 `Vec<u8>` 到 `slint::Image` 的实际耗时（1080p 与 4K 各测一次），
- 是否每次都必须整帧拷贝（读 Slint 版本源码或文档确认），
- 若 `Image::size()` 等操作在没有 backend 时会 panic，记录下来并说明原因，
- 结论：1080p 60fps 时这套拷贝每帧预算够不够（给出占 16.6ms 的百分比）。

如果 Slint 在无 backend 时无法构造 `Image`，退化为"只测 `SharedPixelBuffer` 的内存写入成本 +
记录调用链"，并在报告里写明这一限制。

### 5. 测试

`cargo test -p playback-proto` 必须自包含：

- 用 ffmpeg 生成测试素材（`lavfi` 的 `testsrc2` + 正弦音频，h264/aac，mp4）到 `target/testdata/`，
  ffmpeg 不存在则 `eprintln!` 跳过而不是失败；
- 断言：能解出帧、尺寸正确、`data.len() == stride * height`、连续帧的 pts 严格递增；
- 断言：`dump` 写出的 PNG 文件头正确且字节数 > 1KB；
- 断言：同一文件两次解码得到的第 1 帧像素完全一致（解码确定性）。

### 6. 结论文档

在 `spikes/playback-proto/FINDINGS.md` 里写实测结果：解码器选择、软/硬解耗时、
Slint 拷贝成本、每帧总预算、已知风险。**只要数字和事实，不要写选型建议**（选型由 ADR 负责）。

## 约束

- 不要改动 `crates/` 下任何代码；本项目不依赖 `nova-core`（原型要能被独立删除）。
- 不使用 `unsafe`，除非必要且附注释说明为什么。
- 不建窗口、不连显示服务器、不引入 GUI 依赖（Slint 只作为库用于图片路径测试）。
- 提交前 `cargo fmt` + `cargo clippy -p playback-proto -- -D warnings` 通过。
