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
//! - 交互是通过 Slint 自己的 `Window::dispatch_event` 注入的（`PointerMoved` /
//!   `PointerPressed` / `PointerReleased` / `PointerScrolled` / `KeyPressed`），
//!   不需要操作系统级工具，也不会干扰用户桌面。目标清单由界面在 `ui/*.slint` 里
//!   登记（`Target` / `TargetBus`）。
//! - `targets` / `sweep` / `dead` 与按 id 的 `click` / `hover` / `scroll` 需要"界面把
//!   目标重新登记一遍"：命令先排队等一拍事件循环（`TargetBus.gen` 推进后 Slint 才会
//!   重新求值），见 `ui.rs` 的调试定时器。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;

/// 一条调试命令。
///
/// 没有 `Eq`：`click` 的坐标是 `f32`（Slint 的逻辑像素）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    /// 当前视图摘要。
    View,
    /// 切路由：`home` / `library` / `detail`（需要 `item`）/ `player`（需要 `item`）。
    Route {
        value: String,
        item: Option<String>,
    },
    /// 打开条目详情。
    Open {
        item: String,
    },
    /// 切换媒体库标签（`library` 为空串表示首页）。
    Tab {
        library: String,
    },
    /// 搜索：默认只用本地缓存过滤首页；`page` 为真时进搜索页并填好查询词。
    Search {
        /// 查询词。
        term: String,
        /// 是否进独立搜索页（`true`）还是沿用旧的"过滤首页"（`false`）。
        page: bool,
    },
    /// 自截窗口到 PNG（回读自己的 back buffer）。
    /// 调整窗口逻辑尺寸（等价于用户拖拽窗口边框；Wayland 下由窗口系统下发 configure）。
    Resize {
        /// 宽。
        width: u32,
        /// 高。
        height: u32,
    },
    Screenshot {
        path: PathBuf,
    },
    /// 当前页面的可交互目标清单。
    Targets,
    /// 点一个目标：`target` 按名字，或者 `x`/`y` 按窗口坐标。
    Click {
        target: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
    },
    /// 只发 `PointerMoved`（验证悬停才出现的东西：音量滑杆、卡片放大、行箭头）。
    Hover {
        target: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
    },
    /// 滚轮：把指针移到目标（或坐标）再发 `PointerScrolled`
    /// （`dx` / `dy` 是逻辑像素，语义同 winit：正 `dy` = 向上滚，负 `dy` = 向下滚）。
    Scroll {
        target: Option<String>,
        x: Option<f32>,
        y: Option<f32>,
        /// 横向滚动量。
        dx: f32,
        /// 纵向滚动量。
        dy: f32,
    },
    /// 一个按键（命名键或单字符）。
    Key {
        key: String,
        ctrl: bool,
        shift: bool,
    },
    /// 逐字符输入文本。
    Type {
        text: String,
    },
    /// 在 UI 线程上不阻塞地等一段时间（动画 / 异步加载结算）。
    Wait {
        ms: u64,
    },
    /// 交互扫描：对当前页每个目标"悬停 → 点击"，记录状态摘要是否变化。
    Sweep,
    /// 只返回"点击后状态没有任何变化"的目标（`sweep` 的过滤版）。
    Dead,
    /// 退出进程。
    Quit,
}

impl Command {
    /// 是否需要先让界面把目标重新登记一遍（推进 `TargetBus.gen` 并等一拍）。
    ///
    /// 按 id 操作的命令（`click` / `hover` / `scroll` 给了 `target`）也要：
    /// 清单里的坐标来自上一次登记，页面刚换过 / 刚滚过时是**旧位置**，
    /// 直接派发会落到空处（甚至报 `unknown target`，实测媒体库滚一屏后就能重现）。
    /// 按 `x`/`y` 给坐标的不需要，省掉这 100ms。
    pub(crate) const fn needs_settle(&self) -> bool {
        match self {
            Self::Targets | Self::Sweep | Self::Dead => true,
            Self::Click { target, .. }
            | Self::Hover { target, .. }
            | Self::Scroll { target, .. } => target.is_some(),
            _ => false,
        }
    }

    /// 命令自身要求的等待时间（`wait` 用；其余命令不需要等）。
    pub(crate) const fn wait_ms(&self) -> u64 {
        match self {
            Self::Wait { ms } => *ms,
            _ => 0,
        }
    }
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
        let number = |key: &str| value.get(key).and_then(|value| value.as_f64());
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
                // 可选参数：缺省保持旧行为（只过滤首页），`"page":true` 进搜索页。
                page: value
                    .get("page")
                    .and_then(|page| page.as_bool())
                    .unwrap_or(false),
            }),
            "resize" => {
                let width = number("width").ok_or_else(|| "resize 需要 width".to_owned())?;
                let height = number("height").ok_or_else(|| "resize 需要 height".to_owned())?;
                Ok(Self::Resize {
                    width: width.max(320.0) as u32,
                    height: height.max(240.0) as u32,
                })
            }
            "screenshot" => Ok(Self::Screenshot {
                path: PathBuf::from(text("path").ok_or_else(|| "screenshot 需要 path".to_owned())?),
            }),
            "targets" => Ok(Self::Targets),
            "click" | "hover" => {
                let target = text("target");
                // 坐标按 f32 用（Slint 的逻辑像素），先在这里收窄，后续不再转换。
                let (x, y) = match (number("x"), number("y")) {
                    (Some(x), Some(y)) => (Some(x as f32), Some(y as f32)),
                    _ => (None, None),
                };
                if target.is_none() && x.is_none() {
                    return Err(format!("{cmd} 需要 target，或同时给 x 与 y"));
                }
                if cmd == "click" {
                    Ok(Self::Click { target, x, y })
                } else {
                    Ok(Self::Hover { target, x, y })
                }
            }
            "scroll" => {
                let target = text("target");
                let (x, y) = match (number("x"), number("y")) {
                    (Some(x), Some(y)) => (Some(x as f32), Some(y as f32)),
                    _ => (None, None),
                };
                if target.is_none() && x.is_none() {
                    return Err("scroll 需要 target，或同时给 x 与 y".to_owned());
                }
                // 滚动量：`dy` 必备（只写 `{"cmd":"scroll",...}` 是最常见的误用），`dx` 缺省 0。
                let dy = number("dy").ok_or_else(|| "scroll 需要 dy".to_owned())? as f32;
                let dx = number("dx").unwrap_or(0.0) as f32;
                if dy == 0.0 && dx == 0.0 {
                    return Err("scroll 的 dx 与 dy 不能同时为 0".to_owned());
                }
                Ok(Self::Scroll {
                    target,
                    x,
                    y,
                    dx,
                    dy,
                })
            }
            "key" => Ok(Self::Key {
                key: text("key").ok_or_else(|| "key 需要 key".to_owned())?,
                // 修饰键：`"ctrl":true` / `"shift":true`。只有全局快捷键需要它们
                // （Ctrl+F 聚焦搜索、Ctrl+N 打开添加服务器）。
                ctrl: value
                    .get("ctrl")
                    .and_then(|flag| flag.as_bool())
                    .unwrap_or(false),
                shift: value
                    .get("shift")
                    .and_then(|flag| flag.as_bool())
                    .unwrap_or(false),
            }),
            "type" => Ok(Self::Type {
                text: text("text").ok_or_else(|| "type 需要 text".to_owned())?,
            }),
            "wait" => {
                let ms = number("ms").ok_or_else(|| "wait 需要 ms".to_owned())?;
                if ms < 0.0 {
                    return Err("wait 的 ms 不能为负".to_owned());
                }
                // 调试线程等回信的超时是 10s，等太久会先超时；上限取 9s。
                Ok(Self::Wait {
                    ms: ms.min(9_000.0) as u64,
                })
            }
            "sweep" => Ok(Self::Sweep),
            "dead" => Ok(Self::Dead),
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
            Command::parse(r#"{"cmd":"search","term":"沙丘"}"#),
            Ok(Command::Search {
                term: "沙丘".to_owned(),
                page: false
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"search","term":"沙丘","page":true}"#),
            Ok(Command::Search {
                term: "沙丘".to_owned(),
                page: true
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"screenshot","path":"/tmp/a.png"}"#),
            Ok(Command::Screenshot {
                path: std::path::PathBuf::from("/tmp/a.png")
            })
        );
        assert_eq!(Command::parse(r#"{"cmd":"quit"}"#), Ok(Command::Quit));
        assert_eq!(Command::parse(r#"{"cmd":"targets"}"#), Ok(Command::Targets));
        assert_eq!(
            Command::parse(r#"{"cmd":"click","target":"library.sort"}"#),
            Ok(Command::Click {
                target: Some("library.sort".to_owned()),
                x: None,
                y: None
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"click","x":123,"y":45}"#),
            Ok(Command::Click {
                target: None,
                x: Some(123.0),
                y: Some(45.0)
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"hover","target":"home.row.continue.card.0"}"#),
            Ok(Command::Hover {
                target: Some("home.row.continue.card.0".to_owned()),
                x: None,
                y: None
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"scroll","x":400,"y":300,"dy":-360}"#),
            Ok(Command::Scroll {
                target: None,
                x: Some(400.0),
                y: Some(300.0),
                dx: 0.0,
                dy: -360.0
            })
        );
        // 按目标滚动：`dx` / `dy` 都可给；`dx` 缺省 0。
        assert_eq!(
            Command::parse(r#"{"cmd":"scroll","target":"library.grid","dy":360,"dx":12}"#),
            Ok(Command::Scroll {
                target: Some("library.grid".to_owned()),
                x: None,
                y: None,
                dx: 12.0,
                dy: 360.0
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"key","key":"Enter"}"#),
            Ok(Command::Key {
                key: "Enter".to_owned(),
                ctrl: false,
                shift: false,
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"key","key":"n","ctrl":true}"#),
            Ok(Command::Key {
                key: "n".to_owned(),
                ctrl: true,
                shift: false,
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"type","text":"沙丘"}"#),
            Ok(Command::Type {
                text: "沙丘".to_owned()
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"wait","ms":400}"#),
            Ok(Command::Wait { ms: 400 })
        );
        assert_eq!(Command::parse(r#"{"cmd":"sweep"}"#), Ok(Command::Sweep));
        assert_eq!(Command::parse(r#"{"cmd":"dead"}"#), Ok(Command::Dead));
    }

    #[test]
    fn settle_and_wait_flags_match_the_command() {
        assert!(Command::Targets.needs_settle());
        assert!(Command::Sweep.needs_settle());
        assert!(Command::Dead.needs_settle());
        // 按 id 操作也要等一拍：清单位置可能已经过期。
        assert!(
            Command::Hover {
                target: Some("library.card.0".to_owned()),
                x: None,
                y: None
            }
            .needs_settle()
        );
        assert!(
            Command::Click {
                target: Some("library.sort".to_owned()),
                x: None,
                y: None
            }
            .needs_settle()
        );
        assert!(
            Command::Scroll {
                target: Some("library.grid".to_owned()),
                x: None,
                y: None,
                dx: 0.0,
                dy: -360.0
            }
            .needs_settle()
        );
        // 纯坐标的不需要（省 100ms）。
        assert!(
            !Command::Click {
                target: None,
                x: Some(1.0),
                y: Some(2.0)
            }
            .needs_settle()
        );
        assert!(!Command::View.needs_settle());
        assert_eq!(Command::Wait { ms: 400 }.wait_ms(), 400);
        assert_eq!(Command::View.wait_ms(), 0);
        // 上限 9s：调试线程等回信只有 10s。
        assert_eq!(Command::Wait { ms: 60_000 }.wait_ms(), 60_000);
        assert_eq!(
            Command::parse(r#"{"cmd":"wait","ms":60000}"#),
            Ok(Command::Wait { ms: 9_000 })
        );
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
        assert!(
            Command::parse(r#"{"cmd":"click"}"#)
                .unwrap_err()
                .contains("target")
        );
        assert!(
            Command::parse(r#"{"cmd":"click","x":1}"#)
                .unwrap_err()
                .contains("x")
        );
        // `scroll`：位置和滚动量缺一不可（只给位置等于什么都没滚）。
        assert!(
            Command::parse(r#"{"cmd":"scroll","x":1,"y":2}"#)
                .unwrap_err()
                .contains("dy")
        );
        assert!(
            Command::parse(r#"{"cmd":"scroll","dy":100}"#)
                .unwrap_err()
                .contains("target")
        );
        assert!(
            Command::parse(r#"{"cmd":"scroll","x":1,"y":2,"dy":0}"#)
                .unwrap_err()
                .contains("0")
        );
        assert!(
            Command::parse(r#"{"cmd":"key"}"#)
                .unwrap_err()
                .contains("key")
        );
        assert!(
            Command::parse(r#"{"cmd":"wait","ms":-1}"#)
                .unwrap_err()
                .contains("负")
        );
    }
}
