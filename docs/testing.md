# 测试与开发服务端

## 分层

| 层 | 跑什么 | 需要什么 |
|---|---|---|
| 单元测试 | `nova-core` 的模型与轨道选择、各层映射函数 | 无 |
| 集成测试（mock） | `nova-provider` 用 wiremock 验证请求形状、认证头、分页、重试 | 无 |
| 集成测试（真实） | 对真实 Jellyfin 走完整流程：登录 → 媒体库 → 条目 → 资源 → 播放地址 | docker |
| 真机渲染测试 | `spikes/slint-mpv-gl` 的 Windows/GPU 用例 | GPU + 显示会话 |

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
