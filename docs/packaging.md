# 打包（AppImage）

目前只做 AppImage。理由：一份文件、无需安装、不需要 root，是自建媒体中心用户最省事的形态；
deb/rpm/Flatpak 等发行版渠道等发布流程稳定后再补。

## 构建

```bash
packaging/appimage/build.sh                     # 编译 release + 打包
NOVA_SKIP_BUILD=1 packaging/appimage/build.sh   # 复用 target/release/nova-desktop
```

产物：`dist/Nova-x86_64.AppImage`（**91 MiB**，实测）。

中间目录 `.dev/appimage/`（AppDir）与 `.dev/cache/`（appimagetool）都在 `.gitignore` 里，
仓库只提交 `packaging/appimage/{build.sh,nova.desktop,nova.svg,nova.png}`。

## 自证（无显示环境）

```bash
./dist/Nova-x86_64.AppImage --print-view --no-refresh
# status: 未连接服务器
# hint: 先在终端运行 nova login；本地缓存仍可浏览
```

不需要窗口、不需要合成器，适合 CI 与"装完先看看能不能跑"。

## 捆绑边界（这是 AppImage 最容易做错的地方）

捆绑约 **180 个库**，核心是 `libmpv` 与它那一整套 ffmpeg 依赖：`libavcodec`、`libavformat`、
`libavfilter`、`libavdevice`、`libavutil`、`libswscale`、`libswresample`，以及 `libass`、
`libplacebo`、`libdav1d`、`libx264`/`libx265`、`libdvdnav`/`libdvdread`、`libbluray` 等。

**刻意不捆**（必须用宿主自己的）：

| 类别 | 库 | 捆了会怎样 |
|---|---|---|
| 动态链接器 / libc ABI | `ld-linux*`、`libc`、`libm`、`libpthread`、`libdl`、`librt`、`libgcc_s`、`libstdc++` | 直接起不来或随机段错误 |
| 显卡驱动 / GL | `libGL*`、`libEGL*`、`libGLX*`、`libGLdispatch`、`libdrm*`、`libgbm*`、`libvulkan*` | 黑屏、拿不到硬件解码、驱动与 Mesa 版本冲突 |
| 显示服务器 | `libwayland-*`、`libX11`、`libxcb*`、`libxkbcommon*` | 建不出窗口或输入失效 |
| 字体栈 | `libfontconfig*`、`libfreetype*` | 字体缓存路径错乱，中文变豆腐块 |
| 音频服务 | `libasound`、`libpulse`、`libpipewire` | AO 初始化失败，或与宿主 daemon 版本冲突 |
| 系统服务 | `libdbus-1`、`libsystemd`、`libudev` | 会话总线连接异常 |

`AppRun` 只做一件事：把 `$APPDIR/usr/lib` 前置到 `LD_LIBRARY_PATH`，然后 `exec` 二进制并透传参数。

## 已知限制

1. 宿主需要 **OpenGL 3.2+**（Slint 的 femtovg 后端）。Mesa 软件渲染能起来但 4K 播放会很慢。
2. 硬解走宿主驱动：VA-API 需要宿主装了 iHD/radeonsi 等驱动；NVIDIA 需要宿主 `libcuda`/`nvidia-vaapi`。
   我们只捆 `libva*` 这层 API，不捆驱动。
3. 运行需要 FUSE；没有 FUSE 的环境用 `--appimage-extract-and-run`（或在构建时选 type-2 的 fallback）。
4. 图标只提供 SVG（`nova.svg` + `.DirIcon`）。个别老桌面环境可能不显示，需要时再加 256×256 PNG。
5. 未做签名与更新通道（`zsync`）；发布前要补。
