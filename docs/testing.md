# 测试与开发服务端

## 分层

| 层 | 跑什么 | 需要什么 |
|---|---|---|
| 单元测试 | `nova-core` 的模型与轨道选择、各层映射函数 | 无 |
| 集成测试（mock） | `nova-provider` 用 wiremock 验证请求形状、认证头、分页、重试 | 无 |
| 集成测试（真实） | 对真实 Jellyfin 走完整流程：登录 → 媒体库 → 条目 → 资源 → 播放地址 | docker |
| 真机渲染测试 | `spikes/slint-mpv-gl` 的 Windows/GPU 用例 | GPU + 显示会话 |

## 交互目标与"死按钮"测试

这两组测试对应一个真实事故：媒体库的「排序：最近添加 ▼」**点了没反应**——回调注册了，
实现是空的（`tracing::debug!("…阶段 3 不提供切换")`），而当时没有任何测试能发现它。
现在"界面能点哪些东西"是可断言的：

| 文件 | 作用 |
|---|---|
| `crates/nova-ui/tests/interaction_targets.rs` | 目标清单本身：每页非空、id 唯一、`kind` 合法、矩形非零且在窗口内；**`TouchArea` 覆盖率**（见下）；点击后确实能产出新目标（排序/版本下拉） |
| `crates/nova-app/tests/interaction_sweep.rs` | 行为：用真实 SQLite 缓存 + 无头后端，对每个目标"悬停 → 点击"，断言有足够多的目标产生状态变化，且"没反应"的目标必须出现在 `KNOWN_DEAD` 里 |
| `crates/nova-ui/tests/sweep_restore.rs` | 恢复器：快照与当前值一致时**不**重写视图模型（不重建网格 / 不清图片窗口）；首页每行的行内横向滚动在扫描后写回快照位置（否则旧坐标会点到邻卡），没被扫描点过的行也不能被整份数组写回拖回行首 |

运行：

```bash
cargo test -p nova-ui --test interaction_targets
cargo test -p nova-ui --test sweep_restore
cargo test -p nova-app --test interaction_sweep
```

### `TouchArea` 覆盖率（不留白名单漏洞）

`interaction_targets.rs` 会**扫描 `ui/**/*.slint`** 里所有 `TouchArea` 声明，逐个要求登记：

```rust
const TOUCH_AREAS: &[Case] = &[
    Case { file: "library.slint", component: "LibraryPage", entry: "sort-area",
           pages: &["library"], coverage: Coverage::Targets(&["library.sort"]) },
    Case { file: "components/add_server.slint", component: "AddServerDialog",
           entry: "<anonymous#1>", pages: &[],
           coverage: Coverage::NoTarget("面板自身的事件拦截层：没有点击行为") },
    …
];
```

- 新增 `TouchArea` 而不登记 → `touch_areas_are_classified` 失败（列出所有未登记的项）；
- `NoTarget` **必须写理由**（空理由会失败）；理由要能解释"为什么它不是可枚举控件"；
- 登记表里列了但代码里已经不存在的项也会失败（改名/删除后要同步删掉）。

### `KNOWN_DEAD` 的维护约定

`interaction_sweep.rs` 里的 `KNOWN_DEAD` 是"**点击后状态没有任何变化**且已确认可以接受"的
目标清单，每条带一行原因：

```rust
const KNOWN_DEAD: &[(&str, &str, &str)] = &[
    ("library", "library.nav.tab.0", "点已经选中的「电影」标签：重建出同一份视图，同页空操作"),
];
```

- `dead` **不是失败**：有的按钮只改局部状态（悬停态），有的是"点当前项"的空操作。
- 但测试会双向核对：**出现新的 dead → 红灯**（多半是新按钮忘了接行为），
  **清单里的项又活了 → 也红灯**（清单过时，删掉它）。
- 加条目之前先问一句"这真的是设计如此，还是功能没写完？"——上面那个排序按钮当初
  就是"回调空实现"，它应该被修掉，而不是写进白名单。
- 真机上的完整清单用 `--debug-socket` 的 `sweep` / `dead` 命令拿（见 `docs/debugging.md`）。

## 真机测试用真实数据

真机（Wayland 窗口 + 调试通道）验证一律用真实数据目录 `NOVA_DATA_DIR=$HOME/.local/share/nova`
与真实服务器，不用副本——副本会掩盖真实数据才有的问题（库规模、归属、凭证状态）。约束：

- 同一时刻只跑**一个**写真实目录的实例：统一用 `flock /tmp/nova-real.lock <命令>` 启动，后来者自动排队；
  用完 `{"cmd":"quit"}` 退出，只结束自己启动的进程；
- 真实目录只能用默认凭证模式；`NOVA_CREDENTIALS=memory` 会让 GUI 在没有令牌时运行，只允许用在副本上；
- 不执行 `nova login`、不写密钥环、不手改 `servers.auth_state`；不使用 `--show-url`；
- 播放会向服务端提交进度：只用少数指定条目，开始前记录基线、结束后列出改动过的续播点；
- 会改变 schema 或批量删写缓存的改动，测试前先 `sqlite3 nova.db ".backup /tmp/…"` 备份；
  断网、过期、无令牌这类破坏性场景只在副本上做（`.backup` 数据库 + 复制 `images/`）。

## 并行开发：一个任务一个分支

多个人（或多个代理）同时改代码时，每个任务独占一个 worktree，互不共享工作区：

```bash
git worktree add ../nova-wt/<任务> -b fix/<任务> master
cd ../nova-wt/<任务>
export CARGO_TARGET_DIR=$PWD/../../nova/target/w-<任务>   # 独立构建目录，避免互相锁
```

- 只在自己的 worktree 里修改、测试、提交；不推送、不动 master；
- 审查通过后合并进 master，在 master 上跑全量测试、clippy、fmt，再推送；
- **任务结束立即清理**：`git worktree remove ../nova-wt/<任务>`、`git branch -d fix/<任务>`、
  `rm -rf target/w-<任务>`，以及该任务在 `/tmp` 下留下的截图、日志、socket 与副本。
  每个构建目录动辄几十 GB，不清理会很快吃满磁盘。

## 起一个真实 Jellyfin

```bash
scripts/jellyfin-dev.sh up      # 首次会拉镜像；建 Movies 库并扫描 target/testdata
scripts/jellyfin-dev.sh env     # 打印环境变量
scripts/jellyfin-dev.sh down    # 停（保留数据）
scripts/jellyfin-dev.sh reset   # 清库（下次 up 重新初始化）
```

它做的事，以及**为什么必须按这个顺序**（踩过）：

1. 容器以当前 uid/gid 运行，否则 `.dev/` 下会出现 root 所有、宿主删不掉的配置文件。
2. `GET /Startup/User` 才会触发 `InitializeAsync()` 创建首个用户；`POST /Startup/User`
   只是给它改名设密码，没有用户时返回 **404**（Jellyfin 12 的行为，和 10.x 不同）。
3. 先 `POST /Startup/Complete` 再建用户，会导致向导关闭后所有 `/Startup/*` 变 401，只能 `reset`。
4. 就绪判断看日志里的 `Startup complete`，不要看 `/System/Info/Public`——那个接口在真正就绪前就会应答。

## 真实集成测试的已知环境特性

- 测试素材是 ffmpeg 生成的短片，文件名会被 Jellyfin 解析成奇怪的标题（`hevc10_1080p_3s.mp4` → `hevc10`），
  且没有联网元数据源，所以 `RunTimeTicks`、`ImageTags` 可能为空。
  **provider 的映射必须把"字段缺失"当成正常输入**，不能假设服务端一定填了。
- 直链播放：`GET /Videos/{id}/stream?static=true&api_key=<token>` 支持 Range，
  实测返回 `206 / video/mp4`。注意 token 出现在 URL 里，所以这个 URL 的 `Debug` 必须脱敏。
- `POST /Items/{id}/PlaybackInfo` 返回 `PlaySessionId`、`SupportsDirectPlay`、`MediaSources`。

## Jellyfin 12.4 的 API 事实（实测，写代码前先看）

| 事实 | 影响 |
|---|---|
| `GET /Items?Fields=MediaSources` **有效**，会带回 `MediaSources` | 列表可以直接带资源 |
| `RunTimeTicks` **不是** `ItemFields` 的合法值，加了会被静默忽略 | 列表拿不到时长；详情与 `PlaybackInfo` 才有 |
| 列表响应**不含 `ParentId`** | 条目无法在列表路径上确定父项 |
| `Limit` 数的是服务端原始记录（含 `Folder`） | `--limit 5` 可能只返回 3 个可播放条目 |
| `Folder` 条目调 `PlaybackInfo` 返回 **400** | `sources()` 把 400/404 当作"没有资源" |
| `MediaSources.Container` 在不同端点取值不同（`mp4` vs `mov,mp4,m4a,...`） | 不能当扩展名用 |
| 同一 `DeviceId` 再次登录会**吊销旧令牌** | 集成测试必须串行，否则互相踢下线 |
| `/Users/{id}/Views` 带 `ChildCount` | 媒体库条目数有值 |

## 验证续播必须用长素材

Jellyfin 的 `UpdatePlayState` 对**片长不足 300 秒**的条目直接标记"已看完"并把位置清零
（见 `docs/roadmap.md` 里的三道闸门表）。用几秒的测试片验证进度回写，会得到假阴性，
并且这个"已看完"状态不会因为重新播放而自动恢复，需要 `DELETE /UserPlayedItems/{id}` 复位。

## 环境变量

真实集成测试通过环境变量找服务端（没有设置就跳过，不失败）：

```bash
export NOVA_JELLYFIN_URL=http://127.0.0.1:18096
export NOVA_JELLYFIN_USER=nova
export NOVA_JELLYFIN_PASS=nova-dev-pass
```

## CI 策略

CI 只跑不依赖外部服务的测试（单元 + wiremock）。真实 Jellyfin 测试在本地或专门的 pipeline 里跑，
避免 CI 因为拉镜像、端口、扫描时序而随机红。
