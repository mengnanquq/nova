# Nova

[![CI](https://github.com/mengnanquq/nova/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/mengnanquq/nova/actions/workflows/ci.yml)

Nova 是一个开源的 Linux 桌面媒体中心，用来浏览和播放你自己媒体服务器上的电影与剧集。
用 Rust 和 Slint 编写，播放基于 libmpv。

兼容 Emby 与 Jellyfin 服务端（同一套适配，两种实现分支）。

> **状态：预发布。** 功能已经可以日常使用，但还没有正式版本，安装需要自己从源码构建。
> 目前只支持 Linux 的 Wayland 会话，详见下文「已知限制」。

## 功能

- **浏览**：首页（继续观看、最近添加、各媒体库）、媒体库网格（四种排序）、搜索、详情页。
  大媒体库（几千部）也能流畅滚动。
- **播放**：硬件解码优先，自动回落软解；进度条拖动、±15 秒 / ±60 秒、倍速、全屏。
- **音轨与字幕**：多音轨、多字幕切换，字幕延迟调整；控制栏出现时字幕自动上移，不被遮挡。
- **续播**：播放进度定期同步到服务器，退出时补交；网络失败会落盘，恢复后补发。
- **我的列表**：在详情页把影片加入本地列表。
- **离线浏览**：媒体库索引和海报缓存在本地，连不上服务器时仍可浏览和搜索已缓存的内容；横幅上的「重试」可强制重新连接。
- **键盘 / 遥控器**：所有页面都能只用键盘走完，焦点有清晰的描边提示。

## 隐私与安全

- 没有账号体系，没有遥测，不向第三方发送任何数据。
- 访问令牌只存进系统密钥环（Secret Service），本地数据库里只记录键名；密码不落盘。
- HTTPS 始终校验证书，不提供"跳过证书校验"的开关。
- 本地数据位于 `~/.local/share/nova`（可用 `NOVA_DATA_DIR` 改），删掉它只会丢缓存，
  下次连接服务器会重新同步。

## 从源码构建

需要 Rust stable（版本由 `rust-toolchain.toml` 固定）和下列系统库。

Arch Linux：

```bash
sudo pacman -S --needed base-devel clang mpv fontconfig libxkbcommon wayland dbus \
  gstreamer gst-plugins-base noto-fonts-cjk
```

Debian / Ubuntu：

```bash
sudo apt install build-essential pkg-config libclang-dev libmpv-dev libdbus-1-dev \
  libfontconfig1-dev libxkbcommon-dev libwayland-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev fonts-noto-cjk
```

（GStreamer 只被仓库里的一个引擎选型原型 `spikes/playback-proto` 用到，Nova 本身不依赖它；
因为它在同一个 Cargo workspace 里，构建整个 workspace 时需要装上。）

构建并运行：

```bash
cargo build --release -p nova-app
./target/release/nova-desktop
```

打包成单文件 AppImage：`packaging/appimage/build.sh`，产物在 `dist/`，细节见
[`docs/packaging.md`](docs/packaging.md)。

## 使用

第一次打开时首页是空的，点「连接服务器」（或按 `Ctrl+N`），填服务器地址、用户名和密码。
连接成功后会在后台同步各媒体库。

常用按键：

| 场景 | 按键 | 作用 |
|---|---|---|
| 浏览 | 方向键 / `Enter` / `Esc` | 移动焦点 / 打开 / 返回 |
| 浏览 | 在第一行按 `↑` | 进入首页大图按钮和顶栏（切换媒体库、搜索） |
| 任意页面 | `Ctrl+F` | 搜索 |
| 浏览页面 | `Ctrl+N` | 连接服务器 |
| 播放 | `Space` / `K` | 播放 / 暂停 |
| 播放 | `←` `→`（加 `Shift` 为 60 秒） | 后退 / 前进 15 秒 |
| 播放 | `↑` `↓` / `M` | 音量 / 静音 |
| 播放 | `S` / `A` | 字幕菜单 / 音轨菜单 |
| 播放 | `F` / `Esc` | 全屏 / 退出播放 |

完整的按键说明见 [`docs/ui-architecture.md`](docs/ui-architecture.md)。

仓库里还有一个命令行工具 `nova`（`cargo build --release -p nova-cli`），可以登录、
列出媒体库、查看条目、检查播放地址，主要用于调试，用法见 `nova help`。

## 已知限制

- **只支持 Wayland**。在 X11 会话下启动播放会崩溃，这条路径不打算修。
- **AppImage 的兼容范围取决于构建机器**：在较新的发行版（例如 Arch）上打出的包，
  无法在 Debian stable / Ubuntu LTS 这类 glibc 较旧的系统上运行。
- 没有 Windows / macOS 版本；还没有下一集自动播放、选集面板、投屏。

## 项目结构

```
crates/     Rust crate（见下表）
docs/       架构、测试、打包、界面规范、决策记录（ADR）
packaging/  AppImage 构建脚本与桌面文件
spikes/     早期引擎选型原型，不进入发布产物
scripts/    开发辅助脚本
```

| crate | 职责 |
|---|---|
| `nova-core` | 媒体模型、轨道选择、播放状态机（不做 I/O） |
| `nova-secrets` | 凭证存储（系统密钥环） |
| `nova-provider` | 服务端 API 适配、请求重试 |
| `nova-storage` | SQLite 本地缓存 |
| `nova-images` | 海报获取、解码与磁盘缓存 |
| `nova-playback` | 与引擎无关的播放核心（会话、进度上报） |
| `nova-playback-mpv` | libmpv 播放引擎（OpenGL 渲染） |
| `nova-ui` | Slint 界面与视图模型 |
| `nova-app` | 桌面应用（`nova-desktop`） |
| `nova-cli` | 命令行工具（`nova`） |

## 参与开发

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

提交前请确保这三条都通过（CI 会检查同样的内容）。界面调试有一个本地调试通道
（`nova-desktop --debug-socket <path>`），可以读取界面状态、模拟点击和截图，见
[`docs/debugging.md`](docs/debugging.md)；测试约定见 [`docs/testing.md`](docs/testing.md)。

## 许可证

[GPL-3.0-or-later](LICENSE)。界面库 Slint 的开源版采用 GPLv3，Nova 因此使用同一许可证，
理由见 [`docs/adr/0002-license.md`](docs/adr/0002-license.md)。
