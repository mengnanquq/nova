// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 本地调试通道（`--debug-socket <path>`）。
//!
//! 目的：让"驱动界面"可脚本化，而不是只能靠人手点。行分隔 JSON，一问一答：
//!
//! ```text
//! $ socat - UNIX-CONNECT:/tmp/nova.sock
//! {"cmd":"route","value":"detail","item":"196390"}
//! {"ok":true,"route":"detail"}
//! {"cmd":"screenshot","path":"/tmp/x.png"}
//! {"ok":true,"path":"/tmp/x.png"}
//! ```
//!
//! 设计取舍：
//! - **只用标准库的 Unix socket**，不引入 IPC 依赖；命令表刻意小（够驱动与取证即可）。
//! - 命令在 **UI 线程**执行（`invoke_from_event_loop`），所以能安全碰 Slint 与播放器状态；
//!   调试线程用 oneshot 等结果，天然串行，不会两个命令同时改状态。
//! - 不监听网络，只监听本地文件；socket 权限跟随用户目录，**没有鉴权也没有加密**——
//!   它只该在开发机上开启。
//! - 不注入鼠标/键盘事件：跨平台注入是另一个坑，而"直接切路由 + 自截"已经能覆盖
//!   绝大多数自动化需求。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;

/// 一条调试命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// 当前视图摘要。
    View,
    /// 切路由：`home` / `library` / `detail`（需要 `item`）/ `player`（需要 `item`）。
    Route { value: String, item: Option<String> },
    /// 打开条目详情。
    Open { item: String },
    /// 切换媒体库标签（`library` 为空串表示首页）。
    Tab { library: String },
    /// 用本地缓存过滤首页。
    Search { term: String },
    /// 自截窗口到 PNG（回读自己的 back buffer）。
    Screenshot { path: PathBuf },
    /// 退出进程。
    Quit,
}

impl Command {
    fn parse(line: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| format!("不是合法 JSON：{error}"))?;
        let cmd = value
            .get("cmd")
            .and_then(|cmd| cmd.as_str())
            .ok_or_else(|| "缺少 cmd 字段".to_owned())?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        match cmd {
            "view" => Ok(Self::View),
            "route" => Ok(Self::Route {
                value: text("value").ok_or_else(|| "route 需要 value".to_owned())?,
                item: text("item"),
            }),
            "open" => Ok(Self::Open {
                item: text("item").ok_or_else(|| "open 需要 item".to_owned())?,
            }),
            "tab" => Ok(Self::Tab {
                library: text("library").unwrap_or_default(),
            }),
            "search" => Ok(Self::Search {
                term: text("term").unwrap_or_default(),
            }),
            "screenshot" => Ok(Self::Screenshot {
                path: PathBuf::from(text("path").ok_or_else(|| "screenshot 需要 path".to_owned())?),
            }),
            "quit" => Ok(Self::Quit),
            other => Err(format!("未知命令 `{other}`")),
        }
    }
}

/// 在后台线程上监听 `path`。
///
/// 每条连接读一行、执行、回一行 JSON，然后关闭（用 `socat`/`nc -U` 一条命令一条连接最省心）。
pub(crate) fn serve(path: PathBuf, jobs: mpsc::Sender<Job>) {
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "调试通道绑定失败");
            return;
        }
    };
    tracing::info!(path = %path.display(), "调试通道已就绪（行分隔 JSON）");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let jobs = jobs.clone();
            // 一条连接可能在同一个线程里连续发多条命令，逐条处理。
            std::thread::spawn(move || handle(stream, jobs));
        }
    });
}

/// 一条待执行的命令 + 回信通道（在 UI 线程上执行）。
pub(crate) type Job = (Command, mpsc::Sender<serde_json::Value>);

fn handle(stream: UnixStream, jobs: mpsc::Sender<Job>) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(error) => {
            tracing::debug!(%error, "调试连接复制失败");
            return;
        }
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match Command::parse(&line) {
            Ok(command) => run(command, &jobs),
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        };
        if writeln!(writer, "{reply}").is_err() {
            break;
        }
        let _ = writer.flush();
        if reply.get("quit").and_then(|quit| quit.as_bool()) == Some(true) {
            break;
        }
    }
}

/// 把命令交给 UI 线程执行，并等结果。
fn run(command: Command, jobs: &mpsc::Sender<Job>) -> serde_json::Value {
    let (tx, rx) = mpsc::channel();
    if jobs.send((command, tx)).is_err() {
        return serde_json::json!({ "ok": false, "error": "界面已关闭" });
    }
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(reply) => reply,
        Err(_) => serde_json::json!({ "ok": false, "error": "命令执行超时" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_commands() {
        assert_eq!(Command::parse(r#"{"cmd":"view"}"#), Ok(Command::View));
        assert_eq!(
            Command::parse(r#"{"cmd":"open","item":"42"}"#),
            Ok(Command::Open {
                item: "42".to_owned()
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"route","value":"detail","item":"42"}"#),
            Ok(Command::Route {
                value: "detail".to_owned(),
                item: Some("42".to_owned())
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"tab","library":"abc"}"#),
            Ok(Command::Tab {
                library: "abc".to_owned()
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"screenshot","path":"/tmp/a.png"}"#),
            Ok(Command::Screenshot {
                path: std::path::PathBuf::from("/tmp/a.png")
            })
        );
        assert_eq!(Command::parse(r#"{"cmd":"quit"}"#), Ok(Command::Quit));
    }

    #[test]
    fn rejects_bad_input_with_a_usable_message() {
        assert!(Command::parse("not json").unwrap_err().contains("JSON"));
        assert!(
            Command::parse(r#"{"cmd":"open"}"#)
                .unwrap_err()
                .contains("item")
        );
        assert!(
            Command::parse(r#"{"cmd":"nope"}"#)
                .unwrap_err()
                .contains("未知命令")
        );
    }
}
