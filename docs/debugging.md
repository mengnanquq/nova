# 调试接口

界面是"点出来的"，所以诊断问题时最缺的是**驱动能力**。这里有三件工具，都不需要截屏协议、
不抢焦点、不碰用户屏幕。

## 1. `--debug-socket <path>`：本地命令通道

```bash
nova-desktop --debug-socket /tmp/nova.sock
```

Unix socket，**行分隔 JSON**，一问一答。用 `socat`/`nc -U` 每次连一条命令最省心：

```bash
printf '{"cmd":"view"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```

| 命令 | 作用 |
|---|---|
| `{"cmd":"view"}` | 当前路由、连接状态、首页/库/详情摘要（含标签列表与数量） |
| `{"cmd":"open","item":"<id>"}` | 打开条目详情（走界面导航，会切页并拉数据） |
| `{"cmd":"route","value":"home"\|"library"}` | 切到首页 / 第一个媒体库 |
| `{"cmd":"route","value":"detail","item":"<id>"}` | 同上，直达详情 |
| `{"cmd":"tab","library":"<id>"}` | 切媒体库标签（空串=首页） |
| `{"cmd":"search","term":"..."}` | 用本地缓存过滤首页 |
| `{"cmd":"screenshot","path":"/tmp/x.png"}` | 窗口自截（见下），**异步**：等 `view` 里看不到 pending 或直接等文件出现 |
| `{"cmd":"quit"}` | 退出 |

设计取舍：

- 命令在 **UI 线程**执行（线程只负责收发），所以能安全改 Slint 与播放器状态；串行执行，
  不会两条命令同时改状态。
- 只监听本地文件、无鉴权、无加密——**只在开发机上开**。
- 不注入鼠标/键盘：跨平台事件注入是另一个坑，而"直接切路由 + 自截"覆盖了绝大多数自动化需求。

## 2. `--screenshot`：窗口自截（只含自己的窗口）

```bash
nova-desktop --screenshot /tmp/out.png --screenshot-delay 8000
```

走 `glReadPixels` 回读自己的 back buffer（与播放捕获同一条路径），所以：

- **不依赖 screencopy 协议**（这台机器的 KDE 没有，`grim` 直接失败）
- **不会抓到别人的窗口**（`spectacle -a` 抓的是"活动窗口"，会截到用户正在播的视频）
- **不抢焦点、不碰用户屏幕**

`--screenshot-delay` 是必需的：库标签要等后台刷新完成才出现，1.5s 时只能看到「首页 + 全部」，
看完整标签给 8000。

## 3. `--print-view`：不开窗口打印视图摘要

```bash
nova-desktop --print-view [--refresh] [--search foo]
```

无头、不发网络（除 `--refresh`），适合 CI 与"缓存里到底有什么"的取证。

## 4. 日志

```bash
NOVA_LOG=debug nova-desktop      # 或 info / warn（默认）
```

播放相关诊断走 `nova_playback` / `nova_playback_mpv` 两个 target，可以只开它们：

```bash
NOVA_LOG=warn,nova_playback_mpv=debug nova-desktop
```

## 典型用法

**"首页为什么是黑的"**（真实踩过的坑）：

```bash
nova-desktop --debug-socket /tmp/nova.sock &
printf '{"cmd":"view"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock      # 数据在不在？
printf '{"cmd":"screenshot","path":"/tmp/a.png"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```

数据在但画面黑 → 渲染层问题（当时是 mpv 无条件画到窗口上盖掉了界面）。

**验证某个页面**：

```bash
printf '{"cmd":"open","item":"196390"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
sleep 1
printf '{"cmd":"screenshot","path":"/tmp/detail.png"}\n' | socat - UNIX-CONNECT:/tmp/nova.sock
```
