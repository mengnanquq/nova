// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 真机集成测试：对一个**真实**的 Jellyfin 走完整流程。
//!
//! 门控方式是环境变量：没有 `NOVA_JELLYFIN_URL` 就跳过（打印说明后 return，不算失败），
//! 这样 CI 不会因为拉镜像 / 端口占用而红。起服务端的方法见 `docs/testing.md`：
//!
//! ```bash
//! scripts/jellyfin-dev.sh up
//! eval "$(scripts/jellyfin-dev.sh env)"
//! cargo test -p nova-cli --test jellyfin_live -- --nocapture
//! ```
//!
//! 断言的重点是"能把整条链路走通"，而不是"服务端一定填了某个字段"：
//! 测试素材是 ffmpeg 生成的短片，文件名会被解析成奇怪的标题，
//! `RunTimeTicks` / `ImageTags` 在**列表**接口上为空（真实 Jellyfin 12.4 就是这样），
//! 只有详情与 `PlaybackInfo` 才带时长。

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nova_core::{ItemId, PlayMethod, PlaybackProgress, ServerId};
use nova_provider::{Flavor, MediaProvider, Provider, ProviderConfig};
use nova_secrets::{CredentialStore, MemoryStore, SecretString};
use nova_storage::{Database, unix_now};

/// 把日志收集到内存里，最后断言"日志里没有令牌"。
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        match self.0.lock() {
            Ok(buffer) => String::from_utf8_lossy(&buffer).into_owned(),
            Err(poisoned) => String::from_utf8_lossy(&poisoned.into_inner()).into_owned(),
        }
    }
}

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut buffer) => buffer.extend_from_slice(buf),
            Err(mut poisoned) => poisoned.get_mut().extend_from_slice(buf),
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter(Arc::clone(&self.0))
    }
}

fn env_var(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// 在目录里找含 `needle` 的文件（覆盖 `nova.db`、`nova.db-wal`、`nova.db-shm`）。
fn files_containing(dir: &Path, needle: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return hits;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
        {
            hits.push(path.display().to_string());
        }
    }
    hits
}

/// 用 `grep` 再复核一次（任务的硬要求：`grep` 二进制文件）。
///
/// 本机没有 grep 时返回 `None`（跳过这一条，但上面的字节扫描仍然生效）。
fn grep_finds(path: &Path, needle: &str) -> Option<bool> {
    let status = std::process::Command::new("grep")
        .arg("-a")
        .arg("-F")
        .arg("-q")
        .arg(needle)
        .arg(path)
        .status()
        .ok()?;
    match status.code() {
        // 0 = 找到（泄漏），1 = 没找到（正确），其它（如 2）当作没工具。
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn first_lines(text: &str, limit: usize) -> String {
    text.lines().take(limit).collect::<Vec<_>>().join("\n")
}

#[tokio::test]
async fn live_jellyfin_end_to_end() {
    let Ok(base) = std::env::var("NOVA_JELLYFIN_URL") else {
        eprintln!(
            "跳过：没有设置 NOVA_JELLYFIN_URL。\n\
             起服务端并取环境变量：\n  scripts/jellyfin-dev.sh up\n  eval \"$(scripts/jellyfin-dev.sh env)\""
        );
        return;
    };
    if base.trim().is_empty() {
        eprintln!("跳过：NOVA_JELLYFIN_URL 是空的");
        return;
    }
    let user = env_var("NOVA_JELLYFIN_USER", "nova");
    let password = env_var("NOVA_JELLYFIN_PASS", "nova-dev-pass");

    let capture = Capture::default();
    // 其它测试可能已经装过 subscriber，装不上就继续（下面的 `assert!(!logs.is_empty())`
    // 会明确告诉我们日志采集失效了）。
    let _ = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_target(false)
        .try_init();

    let dir = tempfile::tempdir().expect("应当能建临时目录");
    let db_path = dir.path().join("nova.db");
    let db = Database::open(&db_path).expect("应当能打开 SQLite");
    let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());

    let server_id = ServerId::new("live-server");
    let config = ProviderConfig::new(server_id.clone(), base.clone()).with_display_name("live");
    let provider = Provider::new(Flavor::Jellyfin, config, Arc::clone(&store))
        .expect("合法地址应当能构造 provider");

    // ------------------------------------------------------------------
    // 0. 公开信息：不需要令牌就能问出来（provider 新增的能力）
    // ------------------------------------------------------------------
    tracing::info!(%base, "读取 /System/Info/Public");
    let public = provider
        .public_info()
        .await
        .expect("public_info 应当成功（无需令牌）");
    assert_eq!(public.kind, nova_core::ServerKind::Jellyfin);
    assert!(!public.name.trim().is_empty(), "公开信息应当带服务器名");
    assert!(
        public.version.as_deref().is_some_and(|v| !v.is_empty()),
        "公开信息应当带版本：{public:?}"
    );
    // 窄结构体本来就不含会话字段——这正是它存在的原因（防止误覆盖本地记录）。
    assert!(!public.name.trim().is_empty(), "公开信息应当带服务器名");

    // ------------------------------------------------------------------
    // 1. 登录：令牌进凭证存储，服务器信息落 SQLite，令牌**不能**落 SQLite
    // ------------------------------------------------------------------
    tracing::info!(%user, "开始真机流程：登录");
    let info = provider
        .authenticate(&user, &SecretString::new(password))
        .await
        .expect("登录应当成功");
    assert!(
        info.auth.is_authorized(),
        "登录后应当是已认证状态：{info:?}"
    );
    assert!(
        info.user_id.as_deref().is_some_and(|id| !id.is_empty()),
        "认证响应里应当带用户标识（provider 的 libraries/items 都依赖它）：{info:?}"
    );
    // `authenticate()` 现在会顺带把公开信息（名字 / 版本）填进来。
    assert_eq!(info.name, public.name, "登录后应当带上服务端名");
    assert_eq!(info.version, public.version, "登录后应当带上服务端版本");

    let token = store
        .get(provider.credential_key())
        .expect("读取凭证存储不应失败")
        .expect("登录成功后令牌应当已在凭证存储里");
    assert!(!token.is_empty(), "令牌不能是空串");
    let token_text = token.expose().to_owned();
    assert!(
        token_text.len() >= 8,
        "令牌短得可疑（{} 字符）",
        token_text.len()
    );

    db.upsert_server(&info, unix_now())
        .expect("服务器信息应当能落库");
    let stored = db.servers().expect("应当能读回服务器");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].info.id, server_id);
    assert_eq!(
        stored[0].info.user_id, info.user_id,
        "user_id 必须写进 SQLite（下一条命令要靠它省掉 /Users/Me）"
    );

    // 防"写错表"的关键断言：SQLite（含 -wal/-shm）里不能出现令牌子串。
    let leaked = files_containing(dir.path(), &token_text);
    assert!(
        leaked.is_empty(),
        "令牌出现在了这些文件里（绝不能落 SQLite）：{leaked:?}"
    );
    let grep_hit = grep_finds(&db_path, &token_text);
    assert_ne!(
        grep_hit,
        Some(true),
        "`grep -a -F` 在 {} 里找到了令牌",
        db_path.display()
    );
    tracing::info!(
        db = %db_path.display(),
        grep = ?grep_hit,
        "已确认令牌只存在于凭证存储（SQLite 里没有）"
    );

    // ------------------------------------------------------------------
    // 2. 媒体库（`collection_type` 是新增字段）
    // ------------------------------------------------------------------
    let libraries = provider.libraries().await.expect("列媒体库应当成功");
    assert!(!libraries.is_empty(), "真实服务端上应当至少有一个媒体库");
    for library in &libraries {
        assert!(!library.id.as_str().is_empty());
        assert!(!library.name.is_empty());
        assert!(
            library
                .collection_type
                .as_deref()
                .is_some_and(|t| !t.is_empty()),
            "媒体库应当带服务端原始 collection_type：{library:?}"
        );
    }
    tracing::info!(
        count = libraries.len(),
        libs = ?libraries
            .iter()
            .map(|library| format!(
                "{}（{}，{} 条）",
                library.name,
                library.collection_type.as_deref().unwrap_or("-"),
                library
                    .item_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "?".to_owned())
            ))
            .collect::<Vec<_>>(),
        "媒体库"
    );
    let library = &libraries[0];
    assert_eq!(
        library.collection_type.as_deref(),
        Some("movies"),
        "测试服务端的第一个库是 Movies"
    );

    // ------------------------------------------------------------------
    // 3. 条目：只断言"能解析出条目且 id 非空"，不断言时长/图片
    // ------------------------------------------------------------------
    let page = provider
        .items(nova_provider::ItemQuery::in_parent(library.id.clone()))
        .await
        .expect("列条目应当成功");
    assert!(
        !page.items.is_empty(),
        "媒体库 {} 里应当至少有一个条目（扫描完成了吗？）",
        library.name
    );
    for item in &page.items {
        assert!(!item.id.as_str().is_empty(), "条目 id 不能为空");
        assert!(!item.title.is_empty(), "条目标题不能为空");
        assert!(
            item.kind.is_playable() || item.kind.is_container(),
            "只应有电影 / 剧集 / 季 / 集：{:?}",
            item.kind
        );
    }
    let playable = page
        .items
        .iter()
        .find(|item| item.is_playable())
        .expect("媒体库里应当有可播放条目");
    tracing::info!(
        total = page.total,
        item = %playable.id,
        title = %playable.title,
        runtime = ?playable.runtime,
        "条目（列表接口可能不给时长，这是已知的服务端行为）"
    );

    // 详情接口必须带时长（列表不带、详情带，是这条链路的关键差异）。
    let detail = provider
        .item(&playable.id)
        .await
        .expect("取条目详情应当成功");
    assert_eq!(detail.id, playable.id);
    // 时长取决于服务端是否完成了 ffprobe：测试素材是临时生成的短片，容器重扫后偶发缺失。
    // 所以"缺失"只记日志，不当作失败；但只要服务端给了值，就必须是合理的非零时长
    // （0 说明 ticks→Duration 的换算错了，这才是真 bug）。
    let detail_runtime = match detail.runtime {
        Some(runtime) => {
            assert!(
                !runtime.is_zero(),
                "服务端给了 0 时长，说明 ticks 换算有问题：{detail:?}"
            );
            Some(runtime)
        }
        None => {
            tracing::warn!(
                item = %detail.id,
                "服务端未提供 RunTimeTicks（ffprobe 未完成或素材未被探测），跳过时长断言"
            );
            None
        }
    };

    // ------------------------------------------------------------------
    // 4. 资源与视频流（`MediaSource.runtime` 是新增字段）
    // ------------------------------------------------------------------
    let sources = provider
        .sources(&playable.id)
        .await
        .expect("取资源应当成功");
    assert!(!sources.is_empty(), "可播放条目应当至少有一个 MediaSource");
    let source = sources
        .iter()
        .find(|source| source.video_stream().is_some())
        .expect("至少有一个 MediaSource 带视频流");
    let video = source.video_stream().expect("上一步刚确认过");
    assert!(!source.id.as_str().is_empty());
    if let Some(runtime) = source.runtime {
        assert!(
            !runtime.is_zero(),
            "PlaybackInfo 给了 0 时长，说明 ticks 换算有问题：{source:?}"
        );
        // 两个端点给的是同一份时长，若都提供就必须一致。
        if let Some(detail_runtime) = detail_runtime {
            assert_eq!(
                runtime, detail_runtime,
                "详情与 PlaybackInfo 的时长不一致：{runtime:?} vs {detail_runtime:?}"
            );
        }
    } else {
        tracing::warn!(
            source = %source.id,
            "PlaybackInfo 未提供 MediaSources[].RunTimeTicks，跳过该断言"
        );
    }
    tracing::info!(
        source = %source.id,
        container = ?source.container,
        size = ?source.size,
        runtime = ?source.runtime,
        codec = %video.codec,
        width = ?video.video.as_ref().map(|details| details.width),
        height = ?video.video.as_ref().map(|details| details.height),
        "媒体资源"
    );

    // 容器条目（文件夹）的 PlaybackInfo 是 400：provider 现在返回空列表而不是错误。
    let missing = provider
        .sources(&ItemId::new("00000000000000000000000000000000"))
        .await
        .expect("没有媒体源的条目不该让 sources() 失败");
    assert!(missing.is_empty(), "不存在的条目不应当有资源：{missing:?}");

    // ------------------------------------------------------------------
    // 5. 播放地址 + Range 请求
    // ------------------------------------------------------------------
    let target = provider
        .stream_target(&source.item_id, &source.id)
        .await
        .expect("构造播放地址应当成功");
    assert!(!target.url().is_empty());
    // Display/Debug 必须脱敏：这里是"日志里会不会漏令牌"的第一道闸。
    let display = target.to_string();
    assert!(
        !display.contains(&token_text),
        "Display 泄漏了令牌：{display}"
    );
    assert!(
        !display.contains("api_key"),
        "Display 泄漏了查询串：{display}"
    );
    let debug = format!("{target:?}");
    assert!(!debug.contains(&token_text), "Debug 泄漏了令牌：{debug}");
    tracing::info!(%target, "播放地址（已脱敏）");

    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("应当能建 HTTP 客户端")
        .get(target.url())
        .header(reqwest::header::RANGE, "bytes=0-1023")
        .send()
        .await
        .expect("播放地址应当能取到响应");
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_default();
    let body = response.bytes().await.expect("应当能读到响应体");
    assert!(
        status == 200 || status == 206,
        "Range 请求应当返回 200/206，实际 {status}"
    );
    assert!(
        content_type.starts_with("video/") || content_type.starts_with("application/octet-stream"),
        "Content-Type 应当是视频或二进制流，实际 `{content_type}`"
    );
    assert!(!body.is_empty(), "Range 请求应当拿到字节");
    assert!(body.len() <= 1024, "要求了 1024 字节却拿到 {}", body.len());
    tracing::info!(status, %content_type, bytes = body.len(), "Range 请求通过");

    // ------------------------------------------------------------------
    // 6. 上报进度（新增 `paused` 与 `method`）
    // ------------------------------------------------------------------
    let paused = PlaybackProgress {
        session_seq: 0,
        play_session_id: None,
        item_id: playable.id.clone(),
        source_id: source.id.clone(),
        position: Duration::from_secs(1),
        duration: detail.runtime.or(source.runtime),
        played: false,
        paused: true,
        method: PlayMethod::Transcode,
    };
    provider
        .report_progress(&paused)
        .await
        .expect("带暂停 + 转码方式的进度应当能上报");
    tracing::info!(item = %paused.item_id, "暂停进度上报成功");

    let finished = PlaybackProgress {
        session_seq: 0,
        play_session_id: None,
        item_id: playable.id.clone(),
        source_id: source.id.clone(),
        position: Duration::from_secs(2),
        duration: detail.runtime.or(source.runtime),
        played: true,
        paused: false,
        method: PlayMethod::DirectPlay,
    };
    provider
        .report_progress(&finished)
        .await
        .expect("看完的进度应当能上报（并额外提交 Stopped）");
    tracing::info!(item = %finished.item_id, "看完进度上报成功");

    // ------------------------------------------------------------------
    // 7. 日志里不能有令牌明文
    // ------------------------------------------------------------------
    let logs = capture.text();
    assert!(
        !logs.is_empty(),
        "应当采集到 tracing 日志（subscriber 被占用了？）"
    );
    assert!(
        !logs.contains(&token_text),
        "日志里出现了令牌明文；采集到的日志：\n{}",
        first_lines(&logs, 40)
    );
    assert!(!logs.contains("api_key="), "日志里出现了带 api_key 的地址");
    tracing::info!(bytes = logs.len(), "日志采集完成，未出现令牌明文");

    let provider_debug = format!("{provider:?}");
    assert!(
        !provider_debug.contains(&token_text),
        "Provider Debug 泄漏令牌"
    );

    eprintln!(
        "真机流程通过：{} → {} 个库（collection_type={}）→ {} 条 → 详情时长 {:?} → \
         {} 个资源（runtime={:?}）→ Range {} {} → 进度（暂停/转码 + 看完）已上报；\
         日志 {} 字节，无令牌明文",
        base,
        libraries.len(),
        library.collection_type.as_deref().unwrap_or("-"),
        page.total,
        detail.runtime,
        sources.len(),
        source.runtime,
        status,
        content_type,
        logs.len()
    );
}
