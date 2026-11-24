// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 命令行的端到端测试，全部对**真实** Jellyfin 跑，`NOVA_JELLYFIN_URL` 门控。
//!
//! 三个用例：
//!
//! 1. [`cli_flow_against_live_server`]：同一个进程里连续跑各条命令（等价于"每条命令
//!    一个进程"，只是凭证存储固定为进程内 `MemoryStore` 以便在一个测试里跑完）。
//! 2. [`two_processes_share_one_login`]：真的用 `std::process::Command` 起两次
//!    `nova` 可执行文件——先 `login`，再 `items`——断言第二条命令没有重新登录也能成功。
//!    这一条依赖系统密钥环（跨进程共享令牌的唯一途径），没有密钥环时跳过。
//! 3. [`cached_items_survive_offline`]：同步一次后把库里的服务器地址改成一个关闭的
//!    端口，`--cached` 仍然能列出条目（并且联网路径确实失败，证明"断网"是真的）。
//!
//! 三个用例共享一把锁：同一个服务器 + 同一个 `DeviceId` 重复登录会吊销旧令牌
//! （Jellyfin 的"每个设备一个会话"），并行跑会互相踩。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use nova_cli::{App, CredentialMode, Env, LoginArgs, run_in};
use nova_core::{ItemId, ServerInfo};
use nova_provider::{Flavor, ItemQuery, MediaProvider, Provider, ProviderConfig};
use nova_secrets::SecretString;
use nova_storage::Database;
use serde_json::Value;

/// 集成测试里可以直接拿到正在构建的 `nova` 可执行文件。
const BIN: &str = env!("CARGO_BIN_EXE_nova");

/// 同一服务器重复登录会吊销旧令牌，所以真机用例串行跑。
async fn live_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

fn env_var(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("输出不是 JSON（{error}）：\n{text}"))
}

/// 状态目录里含 `needle` 的文件（含 `nova.db` / `-wal` / `-shm`）。
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

fn live_env(dir: &Path) -> Env {
    Env {
        data_dir: dir.to_path_buf(),
        credentials: CredentialMode::Memory,
    }
}

/// 用 CLI 的登录命令登录，返回它的 JSON 结果。
async fn login(env: &Env, base: &str, user: &str, password: &str) -> Value {
    let app = App::open(env).expect("打开本地状态");
    let output = app
        .login_with_password(
            LoginArgs {
                url: base.to_owned(),
                user: user.to_owned(),
                password_stdin: false,
                insecure_plaintext: true,
            },
            SecretString::new(password),
        )
        .await
        .expect("登录应当成功");
    assert!(output.text().contains("已登录"), "{}", output.text());
    output.json().clone()
}

/// 用 CLI 存下的服务器信息 + 令牌构造一个 provider，供测试自己发现数据
/// （不写任何缓存，所以不会干扰"缓存命中/未命中"的断言）。
fn provider_for(app: &App) -> Provider {
    let mut servers = app.database().servers().expect("读服务器");
    let server = servers.remove(0);
    let mut config = ProviderConfig::new(server.info.id.clone(), server.info.base_url.clone());
    if let Some(user_id) = server.info.user_id.clone().filter(|id| !id.is_empty()) {
        config = config.with_user_id(user_id);
    }
    let flavor = match server.info.kind {
        nova_core::ServerKind::Emby => Flavor::Emby,
        nova_core::ServerKind::Jellyfin => Flavor::Jellyfin,
    };
    Provider::new(flavor, config, app.credential_store().clone()).expect("构造 provider")
}

// ---------------------------------------------------------------------------
// 1. 同一进程内连续跑各条命令
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cli_flow_against_live_server() {
    let Ok(base) = std::env::var("NOVA_JELLYFIN_URL") else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL（见 docs/testing.md）");
        return;
    };
    let _guard = live_lock().await;
    let user = env_var("NOVA_JELLYFIN_USER", "nova");
    let password = env_var("NOVA_JELLYFIN_PASS", "nova-dev-pass");

    let dir = tempfile::tempdir().expect("临时状态目录");
    let env = live_env(dir.path());

    // 空目录下的 `servers` 应当提示先登录，而不是报错崩掉。
    let text = run_in(&env, argv(&["servers"]))
        .await
        .expect("空目录下 servers 应当成功");
    assert!(text.contains("还没有保存任何服务器"), "{text}");

    let error = run_in(&env, argv(&["libraries"]))
        .await
        .expect_err("没有服务器时 libraries 应当报错");
    assert!(
        error.to_string().contains("还没有保存任何服务器"),
        "{error}"
    );
    assert!(error.hint().is_some(), "错误应当带下一步指引");

    // ---------------- login ----------------
    let login_json = login(&env, &base, &user, &password).await;
    assert_eq!(login_json["server"]["kind"], "Jellyfin");
    assert!(
        login_json["server"]["plaintext"].as_bool().unwrap_or(false),
        "测试服务器是 http://"
    );
    assert!(
        login_json["server"]["version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "login 应当带上服务端版本：{login_json}"
    );
    let libraries = login_json["libraries"]
        .as_array()
        .expect("login 应当列出媒体库（collection_type 信息）");
    assert!(!libraries.is_empty(), "真实服务端上应当有媒体库");
    assert_eq!(
        libraries[0]["collection_type"], "movies",
        "login 应当打印 collection_type：{login_json}"
    );
    let library_id = libraries[0]["id"]
        .as_str()
        .expect("媒体库应当有 id")
        .to_owned();

    // `user_id` 必须真的落库：下一条命令（新进程）靠它省掉 `/Users/Me`。
    let app = App::open(&env).expect("打开本地状态");
    let servers = app.database().servers().expect("读服务器");
    assert_eq!(servers.len(), 1);
    assert_eq!(
        servers[0].info.user_id.as_deref(),
        login_json["server"]["user_id"].as_str(),
        "login 之后 user_id 必须写进 SQLite"
    );
    let token = app
        .credential_store()
        .get("jellyfin/srv-46a2f2cd89876282/token")
        .ok()
        .flatten()
        .or_else(|| {
            // 服务器 id 由地址派生，测试里不写死；从 auth 状态里取键名。
            match &servers[0].info.auth {
                nova_core::AuthState::Authorized { credential_key, .. } => app
                    .credential_store()
                    .get(credential_key)
                    .expect("读凭证存储"),
                _ => None,
            }
        })
        .expect("令牌应当在凭证存储里");
    let token_text = token.expose().to_owned();
    assert!(
        files_containing(dir.path(), &token_text).is_empty(),
        "令牌不能写进状态目录里的任何文件"
    );

    // ---------------- items --cached（缓存为空） ----------------
    let empty_cache = json_of(
        &run_in(
            &env,
            argv(&["items", "--library", &library_id, "--cached", "--json"]),
        )
        .await
        .expect("空缓存下 items --cached 应当成功"),
    );
    assert_eq!(empty_cache["data_source"], "cache");
    assert_eq!(empty_cache["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(empty_cache["cached_total"], 0);

    // ---------------- items（联网，写入缓存） ----------------
    // 注意：`--limit` 数的是服务端原始记录，库里混着 Folder（`map_items` 会跳过），
    // 所以拿到的条目数可能少于 limit。这里只断言"非空且不超过 limit"。
    let page = json_of(
        &run_in(
            &env,
            argv(&["items", "--library", &library_id, "--limit", "5", "--json"]),
        )
        .await
        .expect("items 应当成功"),
    );
    assert_eq!(page["data_source"], "server");
    let page_items = page["items"].as_array().expect("items 应当是数组");
    assert!(
        !page_items.is_empty() && page_items.len() <= 5,
        "`--limit 5` 应当返回 1..=5 条（文件夹会被过滤掉）：{page}"
    );
    let cached_id = page_items[0]["id"]
        .as_str()
        .expect("条目应当有 id")
        .to_owned();
    let cached_title = page_items[0]["title"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // ---------------- items --cached（命中） ----------------
    let cached = json_of(
        &run_in(
            &env,
            argv(&["items", "--library", &library_id, "--cached", "--json"]),
        )
        .await
        .expect("items --cached 应当成功"),
    );
    assert_eq!(cached["data_source"], "cache");
    let cached_items = cached["items"].as_array().expect("items 应当是数组");
    assert!(
        !cached_items.is_empty(),
        "刚同步过的条目应当能从本地缓存列出来：{cached}"
    );
    assert!(
        cached_items
            .iter()
            .any(|item| item["id"].as_str() == Some(cached_id.as_str())),
        "缓存里应当有刚同步的条目：{cached}"
    );
    assert_eq!(
        cached["cached_total"],
        page_items.len() as u64,
        "缓存里的条目数应当与刚同步的那一页一致"
    );

    // 本地缓存上的搜索也必须能用（不发网络请求）。
    let searched = json_of(
        &run_in(
            &env,
            argv(&[
                "items",
                "--cached",
                "--library",
                &library_id,
                "--search",
                &cached_title,
                "--json",
            ]),
        )
        .await
        .expect("items --cached --search 应当成功"),
    );
    assert!(
        !searched["items"]
            .as_array()
            .unwrap_or(&Vec::new())
            .is_empty(),
        "本地搜索应当命中刚缓存的条目：{searched}"
    );
    let no_hit = json_of(
        &run_in(
            &env,
            argv(&[
                "items",
                "--cached",
                "--library",
                &library_id,
                "--search",
                "绝对不存在的标题zzz",
                "--json",
            ]),
        )
        .await
        .expect("items --cached --search 应当成功"),
    );
    assert_eq!(no_hit["items"].as_array().map(Vec::len), Some(0));

    // ---------------- info：缓存未命中 → 网络；命中 → 本地 ----------------
    // 用 provider 自己发现一个"不在缓存里"的条目（不写缓存）。
    let app = App::open(&env).expect("打开本地状态");
    let provider = provider_for(&app);
    let full_page = provider
        .items(ItemQuery::in_parent(ItemId::new(library_id.clone())))
        .await
        .expect("provider.items 应当成功");
    let cached_ids: Vec<&str> = page_items
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    let uncached = full_page
        .items
        .iter()
        .find(|item| !cached_ids.contains(&item.id.as_str()));
    if let Some(uncached) = uncached {
        let uncached_id = uncached.id.as_str().to_owned();
        let first = json_of(
            &run_in(&env, argv(&["info", &uncached_id, "--json"]))
                .await
                .expect("info 应当成功"),
        );
        assert_eq!(first["item_source"], "server", "首次 info 应当来自服务端");
        assert_eq!(first["item"]["id"], uncached_id.as_str());
        let sources = first["sources"].as_array().expect("info 应当带资源");
        if uncached.kind == nova_core::MediaKind::Movie {
            assert!(!sources.is_empty(), "电影应当有资源：{first}");
        }

        let second = json_of(
            &run_in(&env, argv(&["info", &uncached_id, "--json"]))
                .await
                .expect("info 应当成功"),
        );
        assert_eq!(second["item_source"], "cache", "再次 info 应当读本地缓存");
    } else {
        eprintln!("提示：这一页已经覆盖了全部条目，跳过 info 的\"网络未命中\"断言");
    }

    let cached_info = json_of(
        &run_in(&env, argv(&["info", &cached_id, "--json"]))
            .await
            .expect("info 应当成功"),
    );
    assert_eq!(cached_info["item_source"], "cache");
    assert_eq!(
        cached_info["item_source"], "cache",
        "已缓存的条目不应该再走网络"
    );
    assert!(cached_info["sources_source"].as_str().is_some());

    // ---------------- play（含 --verify 的 Range 请求） ----------------
    let app = App::open(&env).expect("打开本地状态");
    let play_output = app
        .play(None, &cached_id, true)
        .await
        .expect("play --verify 应当成功");
    let play = play_output.json().clone();
    let display_url = play["display_url"].as_str().expect("应当有脱敏地址");
    assert!(display_url.contains("<redacted>"), "{display_url}");
    assert!(!display_url.contains(&token_text), "脱敏地址泄漏令牌");
    let probe = &play["probe"];
    let status = probe["status"].as_u64().expect("应当有状态码");
    assert!(status == 200 || status == 206, "Range 状态码：{status}");
    let content_type = probe["content_type"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("video/") || content_type.starts_with("application/octet-stream"),
        "Content-Type：{content_type}"
    );
    assert!(
        probe["bytes"].as_u64().unwrap_or(0) > 0,
        "Range 应当拿到字节"
    );
    let real_url = play["url"].as_str().expect("应当有真实地址");
    assert!(real_url.contains(&token_text), "真实地址里应当有令牌");
    assert!(
        !play_output.text().contains(&token_text),
        "play 的文本输出泄漏令牌"
    );

    let play_text = run_in(&env, argv(&["play", &cached_id]))
        .await
        .expect("play 应当成功");
    assert!(play_text.contains("<redacted>"), "{play_text}");
    assert!(
        !play_text.contains(&token_text),
        "play 的 stdout 泄漏令牌：{play_text}"
    );
    let shown = run_in(&env, argv(&["play", &cached_id, "--show-url"]))
        .await
        .expect("play --show-url 应当成功");
    assert!(!shown.contains(&token_text), "stdout 不该出现真实地址");

    // ---------------- progress（暂停 + 转码） ----------------
    let app = App::open(&env).expect("打开本地状态");
    let progress_output = app
        .progress(
            None,
            &cached_id,
            1.0,
            true,
            nova_core::PlayMethod::Transcode,
        )
        .await
        .expect("progress 应当成功");
    let progress = progress_output.json().clone();
    assert_eq!(progress["ok"], true);
    assert_eq!(progress["item"], cached_id.as_str());
    assert_eq!(progress["position_secs"], 1);
    assert_eq!(progress["paused"], true);
    assert_eq!(progress["method"], "transcode");
    assert_eq!(progress["played"], false, "暂停的进度不该同时算看完");
    assert!(progress_output.text().contains("已上报进度"));
    assert!(
        progress_output.text().contains("已暂停"),
        "{}",
        progress_output.text()
    );
    assert!(progress_output.text().contains("transcode"));

    let progress_text = run_in(
        &env,
        argv(&[
            "progress",
            &cached_id,
            "--position",
            "1",
            "--paused",
            "--method",
            "direct-stream",
        ]),
    )
    .await
    .expect("progress 命令行路径应当成功");
    assert!(progress_text.contains("已上报进度"), "{progress_text}");
    assert!(progress_text.contains("direct-stream"), "{progress_text}");

    // 跑完这么多命令，SQLite 里依然没有令牌。
    assert!(
        files_containing(dir.path(), &token_text).is_empty(),
        "跑完整条流程后令牌出现在了状态目录"
    );

    eprintln!(
        "CLI 流程通过：{} → 库 {}（{}）→ 缓存 {} 条（如 {} / {}）→ info 来源切换 → play Range {} {} → progress（暂停+转码）",
        base,
        library_id,
        libraries[0]["name"].as_str().unwrap_or("-"),
        cached["cached_total"],
        cached_id,
        cached_title,
        status,
        content_type
    );
}

// ---------------------------------------------------------------------------
// 2. 两个独立进程共享一次登录
// ---------------------------------------------------------------------------

/// 跑一次 `nova` 可执行文件。
fn run_bin(data_dir: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    let mut command = Command::new(BIN);
    command
        .env("NOVA_DATA_DIR", data_dir)
        // 测试进程里的 NOVA_CREDENTIALS 不该漏给子进程：这里要走真实的自动选择。
        .env_remove("NOVA_CREDENTIALS")
        .env("NOVA_LOG", "warn")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("启动 nova 可执行文件失败");
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().expect("stdin 已经设为 piped");
        pipe.write_all(input.as_bytes()).expect("写密码到 stdin");
    }
    child.wait_with_output().expect("等待 nova 退出失败")
}

fn library_id_from_login(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter(|line| line.contains("collection_type="))
        .find_map(|line| line.split("id=").nth(1))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

#[tokio::test]
async fn two_processes_share_one_login() {
    let Ok(base) = std::env::var("NOVA_JELLYFIN_URL") else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL（见 docs/testing.md）");
        return;
    };
    let _guard = live_lock().await;
    let user = env_var("NOVA_JELLYFIN_USER", "nova");
    let password = env_var("NOVA_JELLYFIN_PASS", "nova-dev-pass");

    let dir = tempfile::tempdir().expect("临时状态目录");

    // 进程 1：登录（密码走 stdin）。
    let login = run_bin(
        dir.path(),
        &[
            "login",
            "--url",
            &base,
            "--user",
            &user,
            "--password-stdin",
            "--insecure-plaintext",
        ],
        Some(&password),
    );
    assert!(
        login.status.success(),
        "进程 1 `nova login` 失败：{}",
        String::from_utf8_lossy(&login.stderr)
    );
    let login_stdout = String::from_utf8_lossy(&login.stdout).into_owned();
    let login_stderr = String::from_utf8_lossy(&login.stderr).into_owned();
    assert!(login_stdout.contains("已登录"), "{login_stdout}");

    // 跨进程共享令牌只能靠密钥环；没有密钥环时这条测试没有意义（但登录本身仍然被验证过）。
    if login_stderr.contains("退化为进程内存储") || login_stderr.contains("密钥环不可用")
    {
        eprintln!(
            "跳过跨进程断言：本机没有可用的密钥环。\n登录仍然成功：\n{login_stdout}\n警告：{login_stderr}"
        );
        return;
    }

    // 进程 2：`servers --json` 必须看到令牌还在（不带任何环境变量）。
    let servers = run_bin(dir.path(), &["servers", "--json"], None);
    assert!(
        servers.status.success(),
        "进程 2 `servers` 失败：{}",
        String::from_utf8_lossy(&servers.stderr)
    );
    let servers_json = json_of(&String::from_utf8_lossy(&servers.stdout));
    assert_eq!(servers_json["credentials_persistent"], true);
    assert_eq!(servers_json["servers"][0]["token_available"], true);
    let server_id = servers_json["servers"][0]["id"]
        .as_str()
        .expect("服务器 id")
        .to_owned();

    // 进程 3：`items`——**没有重新登录**也必须成功。
    let library_id = library_id_from_login(&login_stdout)
        .expect("login 的输出里应当带媒体库 id（collection_type 行）");
    let items = run_bin(
        dir.path(),
        &["items", "--library", &library_id, "--limit", "3", "--json"],
        None,
    );
    let items_stdout = String::from_utf8_lossy(&items.stdout).into_owned();
    let items_stderr = String::from_utf8_lossy(&items.stderr).into_owned();
    assert!(
        items.status.success(),
        "进程 3 `items` 失败（跨进程会话没恢复？）：{items_stderr}"
    );
    assert!(
        !items_stderr.contains("请重新运行") && !items_stderr.contains("没有可用的会话"),
        "第二条命令不该要求重新登录：{items_stderr}"
    );
    let items_json = json_of(&items_stdout);
    assert_eq!(items_json["server"], server_id.as_str());
    assert_eq!(items_json["data_source"], "server");
    assert!(
        items_json["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "第二个进程应当能列出条目：{items_json}"
    );

    // 进程 4：`--cached` 读的是进程 3 刚写进 SQLite 的缓存。
    let cached = run_bin(
        dir.path(),
        &["items", "--library", &library_id, "--cached", "--json"],
        None,
    );
    assert!(
        cached.status.success(),
        "进程 4 `items --cached` 失败：{}",
        String::from_utf8_lossy(&cached.stderr)
    );
    let cached_json = json_of(&String::from_utf8_lossy(&cached.stdout));
    assert_eq!(cached_json["data_source"], "cache");
    assert!(
        cached_json["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "缓存应当能跨进程读到：{cached_json}"
    );

    eprintln!(
        "跨进程通过：login（进程 1）→ servers/items（进程 2/3，未重新登录）→ items --cached（进程 4）\n\
         服务器 {} 库 {} 条目 {} 条",
        server_id, library_id, items_json["total"]
    );
}

// ---------------------------------------------------------------------------
// 3. 断网下的 --cached
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cached_items_survive_offline() {
    let Ok(base) = std::env::var("NOVA_JELLYFIN_URL") else {
        eprintln!("跳过：没有设置 NOVA_JELLYFIN_URL（见 docs/testing.md）");
        return;
    };
    let _guard = live_lock().await;
    let user = env_var("NOVA_JELLYFIN_USER", "nova");
    let password = env_var("NOVA_JELLYFIN_PASS", "nova-dev-pass");

    let dir = tempfile::tempdir().expect("临时状态目录");
    let env = live_env(dir.path());

    let login_json = login(&env, &base, &user, &password).await;
    let library_id = login_json["libraries"][0]["id"]
        .as_str()
        .expect("媒体库 id")
        .to_owned();

    // 联网同步一次（条目 + 资源都进缓存）。
    let page = json_of(
        &run_in(&env, argv(&["items", "--library", &library_id, "--json"]))
            .await
            .expect("items 应当成功"),
    );
    let item_id = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| matches!(item["kind"].as_str(), Some("movie") | Some("episode")))
        .and_then(|item| item["id"].as_str())
        .expect("媒体库里应当有可播放条目")
        .to_owned();
    let item_title = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"].as_str() == Some(item_id.as_str()))
        .and_then(|item| item["title"].as_str())
        .unwrap_or_default()
        .to_owned();
    // `info` 会把资源也缓存下来（`play` 离线要用）。
    let info = json_of(
        &run_in(&env, argv(&["info", &item_id, "--json"]))
            .await
            .expect("info 应当成功"),
    );
    assert_eq!(info["sources_source"], "server");
    assert!(
        !info["sources"].as_array().unwrap_or(&Vec::new()).is_empty(),
        "电影应当有资源：{info}"
    );

    // 让服务器"断网"：把状态目录里记的地址改成一个关闭的端口。
    // 命令行只从 SQLite 读地址（不读 NOVA_JELLYFIN_URL），所以这就是断网。
    let db_path = dir.path().join("nova.db");
    let db = Database::open(&db_path).expect("打开 SQLite");
    let mut servers = db.servers().expect("读服务器");
    let mut server = servers.remove(0);
    server.info = ServerInfo {
        base_url: "http://127.0.0.1:1".to_owned(),
        ..server.info
    };
    db.upsert_server(&server.info, 0).expect("改地址");

    // 联网路径必须真的失败（否则下面的断言证明不了"离线可用"）。
    let offline = run_in(&env, argv(&["items", "--library", &library_id]))
        .await
        .expect_err("地址指向关闭的端口时联网必然失败");
    assert!(
        offline.to_string().contains("网络错误") || offline.to_string().contains("无法连接"),
        "断网错误信息应当可读：{offline}"
    );

    // `--cached`：一个网络请求都不发。
    let cached = json_of(
        &run_in(
            &env,
            argv(&["items", "--library", &library_id, "--cached", "--json"]),
        )
        .await
        .expect("断网时 items --cached 仍然应当成功"),
    );
    assert_eq!(cached["data_source"], "cache");
    let cached_items = cached["items"].as_array().expect("items");
    assert!(
        !cached_items.is_empty(),
        "断网时缓存应当还能列出条目：{cached}"
    );
    assert!(
        cached_items
            .iter()
            .any(|item| item["id"].as_str() == Some(item_id.as_str())),
        "缓存里应当有刚同步的条目：{cached}"
    );
    let cached_search = json_of(
        &run_in(
            &env,
            argv(&[
                "items",
                "--cached",
                "--library",
                &library_id,
                "--search",
                &item_title,
                "--json",
            ]),
        )
        .await
        .expect("断网时本地搜索仍然应当成功"),
    );
    assert!(
        !cached_search["items"]
            .as_array()
            .unwrap_or(&Vec::new())
            .is_empty(),
        "断网时本地搜索应当命中：{cached_search}"
    );

    // 断网时 `info` 也能给出本地缓存（条目 + 资源都在）。
    let offline_info = json_of(
        &run_in(&env, argv(&["info", &item_id, "--json"]))
            .await
            .expect("断网时 info 应当回落到本地缓存"),
    );
    assert_eq!(offline_info["item_source"], "cache");
    assert_eq!(offline_info["sources_source"], "cache");
    assert!(
        !offline_info["sources"]
            .as_array()
            .unwrap_or(&Vec::new())
            .is_empty(),
        "资源也应当来自缓存：{offline_info}"
    );
    // 条目与资源都在缓存里时，`info` 根本不需要联网，因此不会产生"离线"提示；
    // 这条断言保证我们不会靠提示文本来判断数据来源。
    assert_eq!(offline_info["item_source"], "cache");

    eprintln!(
        "断网缓存通过：{} → 缓存 {} 条（条目 {} / {}）→ 联网失败（{}）→ --cached 仍可列出 {} 条",
        base,
        cached["cached_total"],
        item_id,
        item_title,
        offline,
        cached_items.len()
    );
}
