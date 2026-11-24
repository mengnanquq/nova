# 打包（AppImage）

目前只做 AppImage。理由：一份文件、无需安装、不需要 root，是自建媒体中心用户最省事的形态；
deb/rpm/Flatpak 等发行版渠道等发布流程稳定后再补。

产物：`dist/Nova-x86_64.AppImage`（**94 988 792 字节 ≈ 90.6 MiB**，本机实测）。

## 构建

```bash
packaging/appimage/build.sh                     # 编译 release + 打包
NOVA_SKIP_BUILD=1 packaging/appimage/build.sh   # 复用 target/release/nova-desktop
```

脚本 `set -euo pipefail`，幂等（每次先 `rm -rf` AppDir）。依赖 `cargo`、`mksquashfs`
（squashfs-tools，appimagetool 内部用）、`curl`（首次下载 appimagetool）。
**不需要 sudo**：appimagetool 从 GitHub `AppImage/appimagetool` 的 continuous 构建下载后
`--appimage-extract` 解包，用 `squashfs-root/AppRun` 调用，避免依赖 FUSE。

中间目录 `.dev/appimage/`（AppDir）与 `.dev/cache/`（appimagetool）都在 `.gitignore` 里，
仓库只提交 `packaging/appimage/{build.sh,nova.desktop,nova.svg,nova.png}`；
`dist/`、`/squashfs-root/` 也已忽略。

## 目录结构

```
packaging/appimage/
├── build.sh      # 打包脚本
├── nova.desktop  # Name=Nova / Exec=nova-desktop / Categories=AudioVideo;Video;Player; / StartupWMClass=Nova
├── nova.svg      # 品牌图标：深色圆角方块 + 琥珀色 "N"，纯 path/rect，不依赖字体
└── nova.png      # nova.svg 导出的 256×256 PNG（rsvg-convert 生成）

AppDir/
├── AppRun                                   # 设 LD_LIBRARY_PATH 后 exec，参数原样透传
├── nova.desktop / nova.svg / nova.png / .DirIcon
└── usr/{bin/nova-desktop, lib/*（170 个库）, share/{applications,icons}}
```

图标用 `rsvg-convert` 生成（SVG 里刻意没有 `<text>`，`N` 是一条 `path`，避免某些工具链不出字）：

```bash
rsvg-convert -w 256 -h 256 -o packaging/appimage/nova.png packaging/appimage/nova.svg
```

## 自证（无显示环境）

```bash
$ ./dist/Nova-x86_64.AppImage --print-view --no-refresh
status: 未连接服务器
hint: 先在终端运行 nova login；本地缓存仍可浏览
hero: -
tabs:
continue: 0
recent: 0
```

不需要窗口、不需要合成器，适合 CI 与"装完先看看能不能跑"。`--snapshot out.png` 走 Slint
testing backend 的软件光栅器，同样不需要合成器。`--help` 也正常：

```bash
$ ./dist/Nova-x86_64.AppImage --help | head -5
nova-desktop — Nova 桌面应用（SQLite 缓存 + provider + 图片缓存 + Slint 界面）

用法：
  nova-desktop                          打开真实窗口
  nova-desktop --snapshot <path.png>    无头渲染一张快照后退出（不需要合成器）
```

确认真正用的是包内库（而非宿主同名库）：

```bash
LD_DEBUG=libs ./dist/Nova-x86_64.AppImage --print-view --no-refresh 2>&1 \
  | grep "calling init: .*\(libmpv\|libavcodec\|libfontconfig\|libGL\.so\|libstdc++\)"
# libmpv / libavcodec 来自 /tmp/.mount_Nova-*/usr/lib/
# libfontconfig / libGL.so.1 / libstdc++ 来自 /usr/lib/
```

## 捆绑边界（这是 AppImage 最容易做错的地方）

脚本对 `target/release/nova-desktop` 跑一次 `ldd`（已是完整传递闭包），逐条判断：
命中白名单的跳过；其余**只要落在系统库目录**（`/usr/lib`、`/lib`、`lib64`）就 `cp -L` 进
`usr/lib/`（解引用符号链接，文件名保留 soname）。`/opt`、`$HOME` 等 `LD_LIBRARY_PATH`
泄漏进来的私有运行时一律不捆（例如本机 Intel oneAPI 的 `libOpenCL/libsvml/libimf`），
否则等于把某台机器的编译器运行时塞进包里。

实测捆绑 **170 个库**（AppDir 未压缩 241 MiB：`usr/lib` 204 MiB + 二进制 38 MiB），
按白名单跳过 **39 个宿主库**。核心是 `libmpv` 与它那一整套 ffmpeg 依赖：
`libavcodec`、`libavformat`、`libavfilter`、`libavdevice`、`libavutil`、`libswscale`、
`libswresample`，以及 `libass`、`libplacebo`、`libdav1d`、`libx264`/`libx265`、
`libvpx`/`libaom`/`libSvtAv1Enc`/`librav1e`、`libdvdnav`/`libdvdread`、`libbluray`、
`libcurl`、`libgnutls`/`libssl`、`libicu`、`libharfbuzz`/`libfribidi` 等。

列出全部捆绑库：

```bash
./dist/Nova-x86_64.AppImage --appimage-extract >/dev/null
ls squashfs-root/usr/lib | wc -l && ls squashfs-root/usr/lib
```

**刻意不捆**（必须用宿主自己的；脚本里是 `HOST_LIBS_ERE`，并在每轮结束时自查
`usr/lib` 里没有白名单库，有就 `exit 1`）：

| 类别 | 库 | 捆了会怎样 |
|---|---|---|
| 动态链接器 / libc ABI | `ld-linux*`、`libc`、`libm`、`libpthread`、`libdl`、`librt`、`libgcc_s`、`libstdc++` | 直接起不来或随机段错误；一个进程只能有一份 libc；`libstdc++` 会和宿主 C++ 库的 `GLIBCXX_*` 版本打架 |
| 显卡驱动 / GL | `libGL*`、`libEGL*`、`libGLX*`、`libGLdispatch`、`libOpenGL.so`、`libOpenCL`、`libdrm*`、`libgbm*`、`libvulkan*` | GL 入口点由（Mesa/nvidia）驱动导出；捆了 Mesa 的 `libGL` 会绕开宿主驱动 → 黑屏 / `EGL_BAD_DISPLAY`；DRM/GBM/Vulkan 与内核驱动配对，Vulkan loader 还要读宿主 ICD |
| 显示服务器 | `libwayland-*`、`libX11`、`libxcb*`、`libxkbcommon*` | 要和正在跑的 compositor / X server 协议一致，捆了建不出窗口或输入失效 |
| 字体栈 | `libfontconfig*`、`libfreetype*` | 必须读宿主 `/etc/fonts` 与字体缓存，捆了中文变豆腐块 |
| 音频服务 | `libasound`、`libpulse`（含 `libpulsecommon-*`）、`libpipewire` | 要和宿主 ALSA/PulseAudio/PipeWire daemon 通信，服务端 ABI 不匹配 → 没声音甚至挂起 |
| 系统服务 | `libdbus-1`、`libsystemd`、`libudev` | 要连宿主 session bus 与 udev 数据库 |

白名单按题目要求只排除 `libX11*`/`libxcb*`/`libwayland-*`；
`libXext/libXfixes/libXrandr/libXrender/libXss/libXv/libXpresent`
这些 **X 客户端扩展库仍会被捆绑**——ABI 长期稳定、只是薄封装，风险可接受。

手工复核（期望无输出）：

```bash
./dist/Nova-x86_64.AppImage --appimage-extract
ls squashfs-root/usr/lib | grep -E '^(libGL|libEGL|libwayland-|libxkbcommon|libfontconfig|libfreetype|libc\.so|libstdc\+\+|libX11|libxcb)'
```

`AppRun` 只做一件事：把 `$APPDIR/usr/lib` 前置到 `LD_LIBRARY_PATH`，然后 `exec` 二进制并透传参数。

```sh
export LD_LIBRARY_PATH="$APPDIR/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$APPDIR/usr/bin/nova-desktop" "$@"
```

## 已知限制

1. **glibc / libstdc++ 下限由构建机决定（最大的坑）**。本机（Arch，2026-11）打出来的包，
   捆绑库最高引用 `GLIBC_2.44`，二进制本身要 `GLIBC_2.43`；C++ 库要
   `GLIBCXX_3.4.36` / `CXXABI_1.3.15`（`libstdc++` 按白名单取自宿主）。
   也就是说这个包**只能在 glibc ≥ 2.44、GCC 15/16 级别的发行版上跑**
   （Arch 滚动、最新 Fedora 等），Debian stable / Ubuntu LTS 会报
   `version 'GLIBC_2.44' not found`。要广谱兼容，必须在更老的基线环境构建
   （例如 Debian/Ubuntu 容器里跑同一份 `build.sh`，或走 CI 容器构建）。
   核对：

   ```bash
   for f in squashfs-root/usr/lib/* squashfs-root/usr/bin/*; do
     objdump -p "$f" 2>/dev/null | grep -o 'GLIBC_[0-9.]*'; done | sort -uV | tail -1
   ```

2. 宿主需要 **OpenGL 3.2+**（Slint 的 femtovg 后端）。Mesa 软件渲染能起来但 4K 播放会很慢。
   无 GL 的环境只能跑 `--print-view` / `--snapshot`。
3. 硬解走宿主驱动：VA-API 需要宿主装了 iHD/radeonsi 等驱动；NVIDIA 需要宿主 `libcuda`/`nvidia-vaapi`。
   我们只捆 `libva*` / `libvdpau*` / `libvpl` 这层 API，不捆驱动。没驱动时自动回落软解。
4. **只支持 Wayland（X11 不在支持范围）**：项目决定不做 X11 后端。在只有 `DISPLAY` 的环境里
   启动会在 `mpv_render_context_create()` 里 SIGSEGV（`MpvEngine::attach_gl` 首次渲染），
   这是已知的、**不再修**的路径。若要强制 Wayland 后端，可设 `WAYLAND_DISPLAY`；
   X11 会话用户请使用 Wayland 会话。
5. 运行需要 FUSE；没有 FUSE 的环境（容器、精简系统）用：

   ```bash
   ./dist/Nova-x86_64.AppImage --appimage-extract-and-run --print-view --no-refresh
   ./dist/Nova-x86_64.AppImage --appimage-extract && ./squashfs-root/AppRun --print-view --no-refresh
   ```

6. 图标同时提供 256×256 PNG（`nova.png`）与 SVG，二者都放进 `usr/share/icons/hicolor/`，
   根目录也有 `nova.png`/`nova.svg`/`.DirIcon`；个别老桌面环境只认固定尺寸 PNG。
7. 字体看宿主（`libfontconfig`/`libfreetype` 用宿主的），建议装 Noto Sans CJK 之类。
8. 体积 90.6 MiB 主要来自 ffmpeg 全家桶（`libx265`、`libaom`、`libSvtAv1Enc`、`libicudata`…）。
   要瘦身可在 `build.sh` 里额外排除编码器（`libx265/libaom/librav1e/libSvtAv1Enc/libvpx/libx264`），
   代价是这些格式只能软解/无法编码。
9. 未做签名与更新通道（`zsync`）、未带 `.desktop` 安装器（需要 `AppImageLauncher`
   或手工装到 `~/.local/share/applications`）；发布前要补。

## 待办

- 引入老基线容器构建，把 glibc 下限压到 2.28~2.31 量级，否则跨发行版分发名不副实。
