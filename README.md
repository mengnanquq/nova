# Nova

面向个人媒体库与家庭媒体服务器的开源跨平台桌面媒体中心。

浏览与播放体验向 Infuse 看齐，但 Nova 有自己的品牌、自己的架构、自己的社区路线：
本地优先、零遥测、可自建、无账号体系。

**状态：阶段 1 原型完成，引擎选型待定稿（见 ADR-0001 Gate 1.5）。尚无可用界面。**

## 技术栈

| 层 | 选型 |
|---|---|
| 核心语言 | Rust |
| 界面 | Slint |
| 异步运行时 | Tokio |
| HTTP | reqwest（rustls，默认校验证书） |
| 本地存储 | SQLite |
| 序列化 | serde |
| 日志 | tracing |
| 构建 / CI | Cargo / GitHub Actions |

播放引擎初步结论：**libmpv（OpenGL 零拷贝）为主，GStreamer 备选，libVLC 排除**。
CPU 回读路径实测在 4K 上不达标（21–70 ms/帧），所以零拷贝是硬要求而不是优化项。
实机验证尚未做——本机没有显示服务器。详见 `docs/adr/0001-playback-engine.md`。

## 仓库结构

```
crates/
  nova-core/          统一媒体模型、轨道选择、播放状态机（纯逻辑，无 I/O）
spikes/
  playback-proto/     GStreamer → RGBA 帧 → Slint 图片路径的可行性实测
  libmpv-proto/       libmpv SW 渲染路径的对照实测
docs/
  architecture.md     分层与依赖规则
  roadmap.md          阶段与出口条件
  adr/                架构决策记录
```

`spikes/` 是可整体删除的一次性原型，不进入产品构建。

## 构建与测试

需要 Rust stable（见 `rust-toolchain.toml`）。原型还需要系统里的 GStreamer / libmpv 开发包。

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## 设计原则

1. **服务端是唯一事实来源**：本地只缓存索引，删掉数据库能整库重建。
2. **凭证不进模型、不进日志**：令牌存操作系统凭证存储，结构体可以随手打印。
3. **不提供"跳过证书校验"开关**：自签名服务器走导入根证书。
4. **默认零遥测**：任何未来的遥测都必须显式告知、默认关闭、可关闭。
5. **错配比缺失更糟**：语言/字幕匹配宁可失败也不猜（`nova-core` 有对应测试）。

## 许可证

**GPL-3.0-or-later**（见 `docs/adr/0002-license.md` 与根目录 `LICENSE`）。

选它的两个原因：Slint 开源版本身就是 GPLv3，选更宽松的许可并不能改变这一点；
而 GPL 与 GStreamer、libmpv、libVLC 全部兼容，不会在引擎定案后被迫返工。

Source files carry `SPDX-License-Identifier: GPL-3.0-or-later` headers.
