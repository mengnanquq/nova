# Nova 架构

Nova 是面向个人媒体库与家庭媒体服务器的开源跨平台桌面媒体中心。
它参考 Infuse 的浏览与播放体验，但有自己的品牌、架构与社区路线。

## 分层

```
┌──────────────────────────────────────────────────────┐
│ 桌面界面层        Slint：页面 · 导航 · 动画 · 交互    │
├──────────────────────────────────────────────────────┤
│ 应用核心层        媒体库 · 搜索 · 配置 · 播放会话 · 缓存 │
├──────────────────────────────────────────────────────┤
│ 媒体服务层        Emby Provider / Jellyfin Provider   │
│ 媒体处理          Media Engine（字幕 / 元数据）        │
├──────────────────────────────────────────────────────┤
│ 基础设施层        SQLite · HTTP · 本地缓存 · 日志 · 系统适配 │
└──────────────────────────────────────────────────────┘
```

## Crate 划分与依赖方向

依赖只能向下，不能向上或横穿：

```
nova-app ──┬─> nova-ui
           ├─> nova-playback ──> nova-playback-mpv
           ├─> nova-provider
           ├─> nova-images
           ├─> nova-storage
           └─> nova-secrets
                      所有层 ──> nova-core
```

| crate | 职责 | 状态 |
|---|---|---|
| `nova-core` | 统一媒体数据模型、轨道选择、播放状态机。**无 I/O、无 async、无 UI** | 已建立 |
| `nova-secrets` | 凭证存储门面：`SecretString`（不可序列化、Debug 脱敏）+ `CredentialStore`（Keychain / Credential Manager / Secret Service） | 已建立 |
| `nova-provider` | Provider trait、请求重试、地址校验、Emby/Jellyfin 统一映射 | 已建立 |
| `nova-storage` | SQLite：服务器/条目/资源/偏好/同步状态/图片缓存 | 已建立 |
| `nova-cli` | 阶段 2 的验证入口：登录、媒体库、条目、播放地址、进度上报 | 已建立 |
| `nova-playback` | 播放会话、轨道切换、进度上报、`FrameSource` 抽象（引擎无关） | 已建立 |
| `nova-playback-mpv` | libmpv 引擎实现（ADR-0001 结论，OpenGL 零拷贝） | 已建立 |
| `nova-ui` | Slint 界面（18 个 `.slint`）与纯 Rust 视图模型；不碰网络/DB | 已建立 |
| `nova-images` | 图片获取、解码、磁盘缓存（sha256(url+tag)、LRU、并发去重） | 已建立 |
| `nova-app` | 装配：DB 缓存 → 视图模型 → Slint；tokio 后台刷新；`--snapshot` 无头出图 | 已建立 |

`nova-provider` 是一个 crate、两套适配：Emby 与 Jellyfin 的 HTTP API 是同一份代码的两个分支
（字段差异很小，差异点集中在 `Flavor` 与少数 DTO 上）。分两个 crate 会让约 90% 的映射代码重复，
所以这里是"独立的适配模块 + 共用映射核心"，而不是两个 crate。新增第三个服务端（Plex 等）
才需要新的 crate——它的 API 形状完全不同。

## 依赖方向（实测后的版本）

```
nova-cli ─┬─> nova-provider ─┬─> nova-core
          │                  └─> nova-secrets
          ├─> nova-storage ─────> nova-core
          └─> nova-secrets
```

`nova-provider` 不依赖 `nova-storage`：provider 只负责把服务端数据变成 `nova-core` 类型，
落库由调用方决定。这样 provider 的集成测试不需要数据库，storage 的测试不需要网络。

`spikes/` 下是一次性原型，可随时整体删除，不参与产品构建。

## 关键设计决策

### 1. 统一模型在 core，映射在各 provider

Emby 与 Jellyfin 的字段差异（`RunTimeTicks`、`IndexNumber`、`MediaSources` 结构、字幕索引含义）
全部在各自 provider 内消化。上层只认 `nova-core`。这样新增一个服务端不需要改界面。

### 2. core 不做 I/O

`nova-core` 只依赖 `serde`。轨道选择、进度判定这类最容易出错的逻辑因此可以用纯单元测试覆盖，
不需要起 HTTP mock，也就不会随着网络层重构而腐化。

### 3. 播放引擎被 `FrameSource` 隔开

界面层只拿到 `VideoFrame`（RGBA + 时间戳）。引擎是 GStreamer / libmpv / libVLC 对界面无差别。
选择依据见 `docs/adr/0001-playback-engine.md`。

### 4. 凭证不进模型、不进日志

`ServerInfo` 只保存凭证存储里的 **键名**（`AuthState::Authorized::credential_key`），
令牌本体由操作系统凭证存储保管（macOS Keychain / Windows Credential Manager / Linux Secret Service）。
结构体整体 `Debug` 打印是安全的。日志中禁止输出 `Authorization` 头与查询串里的 `api_key`。

TLS：`reqwest` 使用 `rustls-tls-native-roots`，默认校验证书与主机名。**不提供**"跳过证书校验"开关；
自签名服务器走"导入自定义根证书"路径。

### 5. 默认零遥测

产品默认不发送任何行为数据。未来若增加，必须满足：显式告知、默认关闭、可随时关闭且不影响功能。

## 在线 / 离线状态模型

"在线"在三层各有一个表示，含义与职责都不同，**不要混用**：

| 层 | 类型 | 取值 | 是否落库 |
|---|---|---|---|
| 持久化认证状态 | `nova_core::AuthState`（`ServerInfo::auth`，`crates/nova-core/src/server.rs`） | `Unauthorized` / `Authorized { user_name, credential_key }` / `Expired { user_name }` | 是（SQLite `servers`） |
| 请求错误 | `nova_provider::ProviderError`（`crates/nova-provider/src/error.rs`） | `NoToken` / `Unauthorized` / `Forbidden` / `NotFound` / `Http { status }` / `Network` / `Decode` / … | 否 |
| 界面状态 | `nova_app::status::Status`（`crates/nova-app/src/status.rs`） | `Connected { server, base_url }` / `NoServer` / `AmbiguousServer { count }` / `MissingToken { url, note }` / `Expired { url }` / `RefreshFailed { message }` | 否（内存） |

三个容易混的 `ProviderError` 变体（`Unauthorized` 只是 401，不等于"所有认证失败"）：

- `NoToken`：本机凭证存储里没有令牌，请求**根本没发出去**。上层保持原状态，既不 `mark_expired`
  也不改写成 `RefreshFailed`——否则会把"本地还没登录"误记成"服务器判定会话过期"并写进本地库。
- `Unauthorized`：HTTP 401。`nova-provider` 的 `error_from_response` 先 `mark_expired()`
  （把 provider 内存里的 `ServerInfo::auth` 置为 `AuthState::Expired`）再返回错误；
  上层再 `Local::mark_expired()` 写回 SQLite 并广播 `Status::Expired`。
  **只有这一种错误会让会话变成过期。**
- `Forbidden`：HTTP 403，权限不足，**不是**会话过期——重新登录也不会变好。不动 `AuthState`、
  不 `mark_expired`，按普通失败处理（横幅仍可重试）。

转换关系：

- **启动**：`Local::open` 只要凭证存储里能取到非空令牌就**乐观**置为 `Status::Connected`
  （库里存着 `AuthState::Expired` 也一样），交给后台刷新去验证；取不到令牌时，库里是
  `AuthState::Expired` 就给 `Status::Expired`，否则 `Status::MissingToken`；
  `build_provider` 失败则 `Status::RefreshFailed`。
- **刷新成功**：刷新（`refresh::refresh` → 模块内的 `run`）调 `Local::persist_info(provider)` 把 `AuthState::Authorized`
  写回本地库（凭令牌恢复的会话 `user_name` 是空串，不会覆盖登录时存下的名字），
  并把 `Local::status()` 与界面一起置为 `Status::Connected`。
- **刷新失败**（网络 / 5xx / 响应不合规）：刷新模块内的 `failure_status` 把
  `Status::RefreshFailed` 回写 `Local`。`Status::is_connected()` 只有 `Connected` 为真，
  所以 `ui::online_provider` 之后不再给搜索 / 详情 / 图片发请求，一律只读缓存
  （搜索失败页也不会再覆盖本地命中结果）。
- **回到在线**：横幅上的「重试」调 `refresh::refresh_forced`（`force = true`，当前不是
  `Connected` 也照发）；下次启动也会重新刷新。
- **广播**：`Local::set_status` 写完状态就调注册的 `status_listener`（窗口模式在 `ui::run_window`
  里注册，投一条 `UiMessage::Status`），所以后台任务里的 `mark_expired` 也能立刻更新横幅，
  不必等下一次刷新或重启。
- **离线 → 在线**：`Shell::apply_status` 用 `recovered_session(previous, next)`
  （`!previous.is_connected() && next.is_connected()`）判断，为真时调用
  `ImageService::clear_failures` 并重排图片预取。反向（在线 → 离线）**不清**，否则一次断网
  就把真正的坏图（404）记录洗掉，恢复后又去重试它们。

## 图片

- 按**可见窗口**请求（只拉当前视口需要的图）。
- `FetchError::Unavailable` / `FetchError::Transient` **不**进入失败名单（不把短暂/不可用记成永久失败）。

## 本地数据

| 数据 | 位置 | 说明 |
|---|---|---|
| 配置与偏好 | SQLite `preferences` | 见 `UserPreferences` |
| 媒体索引缓存 | SQLite `items` / `sources` / `streams` | 可整表重建，服务端是唯一事实来源 |
| 图片缓存 | 文件系统，按内容哈希命名 | 有容量上限与 LRU 淘汰 |
| 凭证 | 系统凭证存储 | 不落 SQLite，不落日志 |
