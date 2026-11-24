// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 参数解析与帮助文本。
//!
//! 手写而不是引入 clap：本 crate 的命令很少、选项都是"一次性的长选项"，
//! 而 workspace 已经有统一的依赖清单，为一个 CLI 再加一棵依赖树不划算。
//! 解析规则刻意保持简单：`--opt value`、`--opt=value`、`--flag`，未知选项直接报错，
//! 不做前缀匹配（避免 `--serv` 之类拼写错误被悄悄接受成别的东西）。

use nova_core::PlayMethod;

use crate::error::CliError;

/// 子命令。
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `nova help [命令]` / `nova [命令] --help`
    Help(Option<String>),
    /// `nova --version`
    Version,
    /// `nova login ...`
    Login(LoginArgs),
    /// `nova servers [--json]`
    Servers {
        /// 输出 JSON。
        json: bool,
    },
    /// `nova libraries ...`
    Libraries {
        /// 输出 JSON。
        json: bool,
        /// 指定服务器 id。
        server: Option<String>,
    },
    /// `nova items ...`
    Items {
        /// 输出 JSON。
        json: bool,
        /// 指定服务器 id。
        server: Option<String>,
        /// 媒体库 id（必填）。
        library: String,
        /// 搜索关键字。
        search: Option<String>,
        /// 最多返回多少条。
        limit: Option<u32>,
        /// 只读本地缓存（不联网）。
        cached: bool,
    },
    /// `nova info ...`
    Info {
        /// 输出 JSON。
        json: bool,
        /// 指定服务器 id。
        server: Option<String>,
        /// 条目 id。
        item: String,
    },
    /// `nova play ...`
    Play {
        /// 指定服务器 id。
        server: Option<String>,
        /// 条目 id。
        item: String,
        /// 发一次 Range 请求验证地址。
        verify: bool,
        /// 打印带令牌的真实地址（会先警告）。
        show_url: bool,
    },
    /// `nova progress ...`
    Progress {
        /// 指定服务器 id。
        server: Option<String>,
        /// 条目 id。
        item: String,
        /// 播放位置（秒，允许小数）。
        position: f64,
        /// 是否处于暂停状态。
        paused: bool,
        /// 播放方式。
        method: PlayMethod,
    },
}

/// `login` 的选项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginArgs {
    /// 服务器地址（含 http:// 或 https://）。
    pub url: String,
    /// 用户名。
    pub user: String,
    /// 从标准输入读密码（不带该开关时从终端读一行）。
    pub password_stdin: bool,
    /// 允许把密码发往明文 http:// 连接。
    pub insecure_plaintext: bool,
}

/// 解析完整命令行（不含程序名）。
pub fn parse(argv: &[String]) -> Result<Command, CliError> {
    let mut args = normalize(argv);
    let Some(name) = args.first().cloned() else {
        return Ok(Command::Help(None));
    };
    args.remove(0);

    if matches!(name.as_str(), "-h" | "--help" | "help") {
        return Ok(Command::Help(args.first().cloned()));
    }
    if matches!(name.as_str(), "-V" | "--version" | "version") {
        return Ok(Command::Version);
    }

    // 任何子命令都接受 `--help`，且优先于其它参数校验。
    if take_flag(&mut args, "--help") | take_flag(&mut args, "-h") {
        return Ok(Command::Help(Some(name)));
    }

    match name.as_str() {
        "login" => parse_login(args),
        "servers" => {
            let json = take_flag(&mut args, "--json");
            expect_no_args(&args, &name)?;
            Ok(Command::Servers { json })
        }
        "libraries" => {
            let json = take_flag(&mut args, "--json");
            let server = take_value(&mut args, "--server")?;
            expect_no_args(&args, &name)?;
            Ok(Command::Libraries { json, server })
        }
        "items" => parse_items(args),
        "info" => {
            let json = take_flag(&mut args, "--json");
            let server = take_value(&mut args, "--server")?;
            let item = one_positional(&args, "item-id")?;
            Ok(Command::Info { json, server, item })
        }
        "play" => {
            let server = take_value(&mut args, "--server")?;
            let verify = take_flag(&mut args, "--verify");
            let show_url = take_flag(&mut args, "--show-url");
            let item = one_positional(&args, "item-id")?;
            Ok(Command::Play {
                server,
                item,
                verify,
                show_url,
            })
        }
        "progress" => {
            let server = take_value(&mut args, "--server")?;
            let position = take_value(&mut args, "--position")?
                .ok_or_else(|| CliError::Usage("progress 需要 `--position <秒>`".to_owned()))?;
            let position: f64 = position.parse().map_err(|_| {
                CliError::Usage(format!("`--position` 需要数字（秒），收到 `{position}`"))
            })?;
            if !position.is_finite() || position < 0.0 {
                return Err(CliError::Usage("`--position` 必须是非负的秒数".to_owned()));
            }
            if position > 1_000_000_000.0 {
                return Err(CliError::Usage(
                    "`--position` 太大了（上限 10 亿秒）".to_owned(),
                ));
            }
            let paused = take_flag(&mut args, "--paused");
            let method = match take_value(&mut args, "--method")?.as_deref() {
                None | Some("direct-play") => PlayMethod::DirectPlay,
                Some("direct-stream") => PlayMethod::DirectStream,
                Some("transcode") => PlayMethod::Transcode,
                Some(other) => {
                    return Err(CliError::Usage(format!(
                        "`--method` 只能是 direct-play / direct-stream / transcode，收到 `{other}`"
                    )));
                }
            };
            let item = one_positional(&args, "item-id")?;
            Ok(Command::Progress {
                server,
                item,
                position,
                paused,
                method,
            })
        }
        other => Err(CliError::Usage(format!(
            "未知命令 `{other}`（可用：login / servers / libraries / items / info / play / progress）"
        ))),
    }
}

fn parse_login(mut args: Vec<String>) -> Result<Command, CliError> {
    let url = take_value(&mut args, "--url")?
        .ok_or_else(|| CliError::Usage("login 需要 `--url <URL>`".to_owned()))?;
    let user = take_value(&mut args, "--user")?
        .ok_or_else(|| CliError::Usage("login 需要 `--user <NAME>`".to_owned()))?;
    let password_stdin = take_flag(&mut args, "--password-stdin");
    let insecure_plaintext = take_flag(&mut args, "--insecure-plaintext");
    expect_no_args(&args, "login")?;
    if user.trim().is_empty() {
        return Err(CliError::Usage("`--user` 不能为空".to_owned()));
    }
    Ok(Command::Login(LoginArgs {
        url,
        user,
        password_stdin,
        insecure_plaintext,
    }))
}

fn parse_items(mut args: Vec<String>) -> Result<Command, CliError> {
    let json = take_flag(&mut args, "--json");
    let server = take_value(&mut args, "--server")?;
    let library = take_value(&mut args, "--library")?.ok_or_else(|| {
        CliError::Usage("items 需要 `--library <id>`（先运行 `nova libraries`）".to_owned())
    })?;
    let search = take_value(&mut args, "--search")?;
    let cached = take_flag(&mut args, "--cached");
    let limit = match take_value(&mut args, "--limit")? {
        None => None,
        Some(raw) => Some(
            raw.parse::<u32>()
                .map_err(|_| CliError::Usage(format!("`--limit` 需要正整数，收到 `{raw}`")))?,
        ),
    };
    expect_no_args(&args, "items")?;
    Ok(Command::Items {
        json,
        server,
        library,
        search,
        limit,
        cached,
    })
}

/// `--key=value` 拆成两个元素，其余原样保留。
fn normalize(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    for arg in argv {
        match arg.split_once('=') {
            Some((key, value)) if key.starts_with("--") && arg.starts_with("--") => {
                out.push(key.to_owned());
                out.push(value.to_owned());
            }
            _ => out.push(arg.clone()),
        }
    }
    out
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|arg| arg == name) {
        Some(pos) => {
            args.remove(pos);
            true
        }
        None => false,
    }
}

fn take_value(args: &mut Vec<String>, name: &str) -> Result<Option<String>, CliError> {
    let Some(pos) = args.iter().position(|arg| arg == name) else {
        return Ok(None);
    };
    if pos + 1 >= args.len() {
        return Err(CliError::Usage(format!("`{name}` 需要一个值")));
    }
    let value = args.remove(pos + 1);
    args.remove(pos);
    Ok(Some(value))
}

/// 取唯一的位置参数；剩余参数一律视为错误。
fn one_positional(args: &[String], what: &str) -> Result<String, CliError> {
    let mut positional: Vec<&String> = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            return Err(CliError::Usage(format!("未知选项 `{arg}`")));
        }
        positional.push(arg);
    }
    match positional.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(CliError::Usage(format!("缺少参数 <{what}>"))),
        _ => Err(CliError::Usage(format!(
            "只接受一个 <{what}>，多出来的：{}",
            positional[1..]
                .iter()
                .map(|arg| arg.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ))),
    }
}

fn expect_no_args(args: &[String], command: &str) -> Result<(), CliError> {
    match args.first() {
        None => Ok(()),
        Some(arg) if arg.starts_with('-') => Err(CliError::Usage(format!("未知选项 `{arg}`"))),
        Some(arg) => Err(CliError::Usage(format!(
            "`{command}` 不接受位置参数（多余的是 `{arg}`）"
        ))),
    }
}

/// 帮助文本。`topic` 为子命令名时只打印那一节。
pub fn help(topic: Option<&str>) -> String {
    let topic = topic.map(str::trim).filter(|topic| !topic.is_empty());
    match topic {
        None => OVERVIEW.to_owned(),
        Some("login") => LOGIN.to_owned(),
        Some("servers") => SERVERS.to_owned(),
        Some("libraries") => LIBRARIES.to_owned(),
        Some("items") => ITEMS.to_owned(),
        Some("info") => INFO.to_owned(),
        Some("play") => PLAY.to_owned(),
        Some("progress") => PROGRESS.to_owned(),
        Some(other) => format!("没有 `{other}` 这个命令的帮助。\n\n{OVERVIEW}"),
    }
}

const OVERVIEW: &str = "\
nova — Nova 命令行（阶段 2 出口条件的验证工具）

用法：
  nova login --url <URL> --user <NAME> [--password-stdin] [--insecure-plaintext]
  nova servers [--json]
  nova libraries [--json] [--server <id>]
  nova items --library <id> [--search <s>] [--limit N] [--cached] [--json] [--server <id>]
  nova info <item-id> [--json] [--server <id>]
  nova play <item-id> [--verify] [--show-url] [--server <id>]
  nova progress <item-id> --position <秒> [--paused] [--method <方式>] [--server <id>]
  nova help [命令]

环境变量：
  NOVA_DATA_DIR       状态目录（默认 $XDG_DATA_HOME/nova，即 ~/.local/share/nova）
  NOVA_CREDENTIALS    auto（默认）| keyring | memory；决定令牌存在哪里
  NOVA_LOG            日志过滤（默认 warn，例如 NOVA_LOG=debug）

安全约定：
  * 令牌只写入系统凭证存储（Keychain / Credential Manager / Secret Service），
    SQLite 里只有凭证的键名。密钥环不可用时会明确警告并退化为进程内存储，
    那种情况下每条新命令都需要重新登录。
  * 播放地址里带令牌，默认只打印脱敏形式；`play --show-url` 才会打印真实地址，
    且会先打印一行警告。
  * 本工具**不提供**跳过 TLS 证书校验的选项。`--insecure-plaintext` 只表示
    \"允许把密码发往明文 http:// 连接\"，与证书校验无关。

用 `nova help <命令>` 看单个命令的细节。";

const LOGIN: &str = "\
nova login — 配对一台 Jellyfin / Emby 服务器

用法：
  nova login --url <URL> --user <NAME> [--password-stdin] [--insecure-plaintext]

  --url <URL>              服务器地址，例如 http://192.168.1.10:8096
  --user <NAME>            用户名
  --password-stdin         从标准输入读密码（推荐，密码不会出现在 shell 历史里）
  --insecure-plaintext     允许把密码发往明文 http:// 连接

说明：
  * 不带 --password-stdin 时从标准输入读一行；终端是否回显由终端决定，
    所以脚本与共享机器上一律建议用 --password-stdin。
  * 默认拒绝把密码发往 http:// 的服务器。--insecure-plaintext 只是承认
    \"这条连接是明文的\"，**与证书校验无关**：https:// 的自签名证书仍然会失败，
    Nova 不提供跳过证书校验的开关（请把根证书导入系统信任库）。
  * 成功后：令牌写入凭证存储，服务器信息写入 SQLite（nova.db），
    并打印服务器名、版本、类型以及连接是否明文。";

const SERVERS: &str = "\
nova servers — 列出本机已保存的服务器

用法：
  nova servers [--json]

  --json    输出 JSON

每行包含服务器 id（`--server` 用的就是它）、名称、类型、地址、用户与认证状态。";

const LIBRARIES: &str = "\
nova libraries — 列出服务器上的媒体库

用法：
  nova libraries [--json] [--server <id>]

  --json          输出 JSON
  --server <id>   多台服务器时指定用哪一台（`nova servers` 里的第一列）";

const ITEMS: &str = "\
nova items — 列出媒体库里的条目

用法：
  nova items --library <id> [--search <s>] [--limit N] [--cached] [--json] [--server <id>]

  --library <id>  媒体库 id（`nova libraries` 输出里的第一列）
  --search <s>    按标题搜索（`--cached` 时在本地缓存里过滤）
  --limit N       最多返回 N 条（默认由服务端决定）
  --cached        只读本地 SQLite 缓存，一个网络请求都不发（断网可用）；
                  缓存里没有时先不带 `--cached` 跑一次同步
  --json          输出 JSON
  --server <id>   多台服务器时指定用哪一台

联网拉到的条目会写进 nova.db（`upsert_items`），`--cached` 读的就是这份数据。";

const INFO: &str = "\
nova info — 查看单个条目及其可播放资源

用法：
  nova info <item-id> [--json] [--server <id>]

条目的时长、资源（MediaSource）与媒体流都从服务端的详情 / PlaybackInfo 读取。
列表接口可能不返回时长（真实的 Jellyfin 就是这样），详情接口通常有。";

const PLAY: &str = "\
nova play — 取条目的可播放直链

用法：
  nova play <item-id> [--verify] [--show-url] [--server <id>]

  --verify     对真实地址发一次 Range 请求（bytes=0-1023），
               打印状态码、Content-Type 与拿到的字节数
  --show-url   打印带令牌的真实地址。会先在标准错误打印一行警告：
               这条地址等同于凭证，不要贴进日志、issue 或截图

默认只打印脱敏后的地址（查询串被替换为 `?<redacted>`）。";

const PROGRESS: &str = "\
nova progress — 上报播放进度

用法：
  nova progress <item-id> --position <秒> [--paused] [--method <方式>] [--server <id>]

  --paused            上报为\"已暂停\"（服务端的会话状态跟着变）
  --method <方式>     direct-play（默认）/ direct-stream / transcode，
                      服务端用它统计哪些设备在被迫转码

位置允许小数秒。是否\"已看完\"由本地偏好里的 finished_threshold 判断：
位置落在片尾阈值内时，会额外向服务端提交一次\"停止播放\"（`--paused` 时不算看完）。";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_argv(argv: &[&str]) -> Result<Command, CliError> {
        parse(&argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn empty_argv_prints_help() {
        assert_eq!(parse_argv(&[]).unwrap(), Command::Help(None));
        assert_eq!(parse_argv(&["help"]).unwrap(), Command::Help(None));
        assert_eq!(parse_argv(&["--help"]).unwrap(), Command::Help(None));
        assert_eq!(parse_argv(&["-h"]).unwrap(), Command::Help(None));
        assert_eq!(
            parse_argv(&["play", "--help"]).unwrap(),
            Command::Help(Some("play".to_owned()))
        );
        assert_eq!(parse_argv(&["--version"]).unwrap(), Command::Version);
    }

    #[test]
    fn login_parses_all_switches() {
        let command = parse_argv(&[
            "login",
            "--url",
            "http://127.0.0.1:8096",
            "--user",
            "nova",
            "--password-stdin",
            "--insecure-plaintext",
        ])
        .unwrap();
        assert_eq!(
            command,
            Command::Login(LoginArgs {
                url: "http://127.0.0.1:8096".to_owned(),
                user: "nova".to_owned(),
                password_stdin: true,
                insecure_plaintext: true,
            })
        );
    }

    #[test]
    fn equals_syntax_is_accepted() {
        let command = parse_argv(&["items", "--library=lib-1", "--limit=5"]).unwrap();
        assert_eq!(
            command,
            Command::Items {
                json: false,
                server: None,
                library: "lib-1".to_owned(),
                search: None,
                limit: Some(5),
                cached: false,
            }
        );
    }

    #[test]
    fn play_and_progress_parse() {
        assert_eq!(
            parse_argv(&["play", "abc", "--verify", "--show-url"]).unwrap(),
            Command::Play {
                server: None,
                item: "abc".to_owned(),
                verify: true,
                show_url: true,
            }
        );
        assert_eq!(
            parse_argv(&["progress", "abc", "--position=12.5"]).unwrap(),
            Command::Progress {
                server: None,
                item: "abc".to_owned(),
                position: 12.5,
                paused: false,
                method: PlayMethod::DirectPlay,
            }
        );
        assert_eq!(
            parse_argv(&[
                "progress",
                "abc",
                "--position",
                "3",
                "--paused",
                "--method",
                "transcode",
                "--server",
                "s1"
            ])
            .unwrap(),
            Command::Progress {
                server: Some("s1".to_owned()),
                item: "abc".to_owned(),
                position: 3.0,
                paused: true,
                method: PlayMethod::Transcode,
            }
        );
        assert_eq!(
            parse_argv(&[
                "progress",
                "abc",
                "--position",
                "1",
                "--method",
                "direct-stream"
            ])
            .unwrap(),
            Command::Progress {
                server: None,
                item: "abc".to_owned(),
                position: 1.0,
                paused: false,
                method: PlayMethod::DirectStream,
            }
        );
        assert_eq!(
            parse_argv(&["items", "--library", "l", "--cached"]).unwrap(),
            Command::Items {
                json: false,
                server: None,
                library: "l".to_owned(),
                search: None,
                limit: None,
                cached: true,
            }
        );
    }

    #[test]
    fn missing_and_unknown_arguments_are_usage_errors() {
        for argv in [
            vec!["login", "--user", "nova"], // 缺 --url
            vec!["login", "--url", "u"],     // 缺 --user
            vec!["items"],                   // 缺 --library
            vec!["items", "--library", "l", "--bogus"],
            vec!["info"],           // 缺位置参数
            vec!["play", "a", "b"], // 多个位置参数
            vec!["progress", "a"],  // 缺 --position
            vec!["progress", "a", "--position", "-1"],
            vec!["progress", "a", "--position", "x"],
            vec!["progress", "a", "--position", "1", "--method", "copy"],
            vec!["servers", "extra"],
            vec!["nonsense"],
        ] {
            let error = parse_argv(&argv).expect_err(&format!("{argv:?} 应当报用法错误"));
            assert!(matches!(error, CliError::Usage(_)), "{argv:?} → {error:?}");
        }
    }

    #[test]
    fn help_texts_document_the_security_rules() {
        let overview = help(None);
        assert!(overview.contains("NOVA_DATA_DIR"));
        assert!(overview.contains("不提供"));
        assert!(overview.contains("TLS 证书校验"));

        let login = help(Some("login"));
        assert!(login.contains("--insecure-plaintext"));
        assert!(login.contains("与证书校验无关"), "{login}");

        let items = help(Some("items"));
        assert!(items.contains("--cached"), "{items}");
        assert!(items.contains("断网可用"), "{items}");

        let progress = help(Some("progress"));
        assert!(progress.contains("--paused"), "{progress}");
        assert!(progress.contains("transcode"), "{progress}");

        let play = help(Some("play"));
        assert!(play.contains("<redacted>"));
        assert!(play.contains("--show-url"));

        assert!(help(Some("nope")).contains("没有 `nope` 这个命令"));
    }
}
