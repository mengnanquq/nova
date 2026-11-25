# Nova

面向个人媒体库与家庭媒体服务器的开源桌面媒体中心（Rust + Slint）。

本地优先、零遥测、可自建、无账号体系；凭证存入操作系统凭证存储，SQLite 只缓存索引。

## 特性

- 媒体库浏览：首页 / 媒体库 / 搜索 / 详情页，含继续观看与最近添加。
- 播放：libmpv OpenGL 零拷贝路径，硬解断言与帧 watchdog。
- 进度与续播：上报节奏、去重、失败重试与落盘补发；详情页续播位置与按钮文案。
- 轨道选择：多音轨 / 多字幕，手动选择优先于用户偏好。
- 本地缓存与离线可读：SQLite 索引 + 图片磁盘缓存；服务端不可达时仍可浏览。
- 服务器管理：多服务器；访问令牌只经操作系统凭证存储，SQLite 仅存键名。
- 分发：单文件 AppImage（`dist/Nova-x86_64.AppImage`）。

兼容 Emby 与 Jellyfin 服务端（同一套适配，两种实现分支）。

## 技术栈

| 层 | 选型 |
|---|---|
| 语言 | Rust（edition 2024，rust-version 1.85） |
| 界面 | Slint |
| 异步运行时 | Tokio |
| HTTP | reqwest（rustls，默认校验证书） |
| 本地存储 | SQLite（rusqlite，bundled） |
| 序列化 | serde |
| 日志 | tracing |
| 播放引擎 | libmpv（OpenGL 零拷贝） |
| 打包 | AppImage |

## 仓库结构

```
crates/     10 个 crate（见下表）
docs/       架构、路线图、测试、UI 架构、打包、ADR
spikes/     一次性原型，可整体删除，不参与产品构建
packaging/  AppImage 构建脚本与桌面文件
scripts/    开发辅助（Jellyfin 开发实例）
```

| crate | 职责 |
|---|---|
| `nova-core` | 统一媒体模型、轨道选择、播放状态机（无 I/O） |
| `nova-secrets` | 凭证存储门面（`SecretString` + 平台凭证存储） |
| `nova-provider` | Provider trait、Emby/Jellyfin 统一映射、请求重试 |
| `nova-storage` | SQLite：服务器 / 条目 / 资源 / 偏好 / 图片缓存索引 |
| `nova-cli` | 命令行验证工具（登录、媒体库、播放地址、进度） |
| `nova-playback` | 引擎无关播放核心（会话、进度上报、轨道选择） |
| `nova-playback-mpv` | libmpv 引擎实现 |
| `nova-images` | 图片获取、解码、磁盘缓存 |
| `nova-ui` | Slint 界面与纯 Rust 视图模型 |
| `nova-app` | 桌面应用装配（缓存 → 视图模型 → Slint） |

## 快速开始

前置：Rust stable（见 `rust-toolchain.toml`）、libmpv、Wayland 会话，以及构建所需系统包（见 `docs/packaging.md`）。

```bash
cargo build --release -p nova-app
./target/release/nova-desktop            # 打开窗口
nova login --url <URL> --user <NAME>    # 添加服务器（凭证明文经 stdin 可用 --password-stdin）
```

常用命令行（完整选项见 `--help`）：

```bash
nova servers [--json]
nova libraries [--json] [--server <id>]
nova items --library <id> [--search <s>] [--limit N] [--cached] [--json] [--server <id>]
nova info <item-id> [--json] [--server <id>]
nova play <item-id> [--verify] [--show-url] [--server <id>]
nova progress <item-id> --position <秒> [--paused] [--method <方式>] [--server <id>]
```

应用也支持无头模式：`nova-desktop --print-view`、`nova-desktop --snapshot out.png`（详见 `--help`）。

## 开发

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p nova-ui --test snapshots   # 生成/校验 Slint 快照
```

当前 workspace 测试：523 个通过（以本地运行为准）。

## 设计原则

1. 服务端是唯一事实来源：本地只缓存索引，删库可重建。
2. 凭证不进模型、不进日志：令牌存操作系统凭证存储。
3. 不提供"跳过证书校验"开关：自签名服务器走导入根证书。
4. 默认零遥测：任何遥测都必须显式告知、默认关闭、可关闭。
5. 错配比缺失更糟：语言 / 字幕匹配宁可失败也不猜。

## 许可证

**GPL-3.0-or-later**（见 `LICENSE` 与 `docs/adr/0002-license.md`）。Slint 开源版为 GPLv3，故项目采用 GPLv3；源文件带 `SPDX-License-Identifier: GPL-3.0-or-later` 头。

## 状态

阶段 0–4 已完成，阶段 5（发布）进行中：AppImage 已能产出 `dist/Nova-x86_64.AppImage`
（约 90.6 MiB，170 个捆绑库），Wayland 会话可用。当前限制：

- **仅支持 Wayland**：X11 下 `mpv_render_context_create()` 会 SIGSEGV，已明确不在支持范围。
- **glibc 下限偏高**：包内最高引用 `GLIBC_2.44` / `GLIBCXX_3.4.36`，在较老的发行版上会
  报 `version not found`。需要在老基线容器里构建才能跨发行版分发，这一项未完成。
- 无 Windows / macOS 构建；崩溃日志导出、AppImage 签名与 zsync 更新通道未做。

各阶段出口条件与未完成项见 `docs/roadmap.md`。
