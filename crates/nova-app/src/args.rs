// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 命令行参数。纯解析，不碰 I/O，可直接单测。
//!
//! 三种模式互斥：
//!
//! - 默认：打开真实窗口（winit + femtovg）；
//! - `--snapshot <path.png>`：用 Slint testing backend 渲染一张快照后退出；
//! - `--print-view`：不建窗口，把当前 [`HomeView`](nova_ui::HomeView) 摘要打到 stdout。

use std::path::PathBuf;

use crate::error::AppError;

/// 帮助文本。
pub const USAGE: &str = "\
nova-desktop — Nova 桌面应用（SQLite 缓存 + provider + 图片缓存 + Slint 界面）

用法：
  nova-desktop                          打开真实窗口
  nova-desktop --snapshot <path.png>    无头渲染一张快照后退出（不需要合成器）
  nova-desktop --print-view             不建窗口，打印当前首页摘要
  nova-desktop --play <item-id>         直达播放页并起播（真实窗口）
  nova-desktop --play <item-id> --capture <path.png> --seconds 5
                                        起播后播 5 秒，回读窗口像素存 PNG 并打印验收指标

选项：
  --route home|library|detail|player    指定快照/打印的起始页面（默认 home）
  --item <id>                           指定详情/播放页的条目 id
  --source <id>                         --play 时指定播放版本（默认第一个）
  --capture <path.png>                  --play 的捕获模式（退出码 0 通过 / 3 不通过）
  --screenshot <path.png>               窗口自截：回读自己的 back buffer 写 PNG 后退出
  --screenshot-delay <ms>               自截前等多久（默认 1500；要看刷新后的标签就给 8000）
  --seconds <n>                         捕获前播放多少秒（默认 5）
  --server <id>                         本机有多台服务器时指定一台
  --search <text>                        只用本地缓存过滤标题（不联网）
  --refresh                             print-view 也先做一次联网刷新
  --no-refresh                          snapshot 不做联网刷新（默认会做一次）
  -h, --help                            显示本帮助
  -V, --version                         显示版本

数据目录：`$NOVA_DATA_DIR` > `$XDG_DATA_HOME/nova` > `$HOME/.local/share/nova`；
与 `nova`（CLI）共用同一个 nova.db 与 `<data>/images` 图片缓存。
";

/// 快照 / 打印的起始页面。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RouteArg {
    /// 首页。
    #[default]
    Home,
    /// 媒体库。
    Library,
    /// 详情页。
    Detail,
    /// 播放外壳。
    Player,
}

impl RouteArg {
    /// 从 `--route` 的取值解析。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "home" => Some(Self::Home),
            "library" => Some(Self::Library),
            "detail" => Some(Self::Detail),
            "player" => Some(Self::Player),
            _ => None,
        }
    }
}

/// 启动模式。
#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    /// 打开真实窗口。
    Window,
    /// 渲染一张 PNG 后退出。
    Snapshot(PathBuf),
    /// 打印首页摘要后退出。
    PrintView,
    /// 打印帮助后退出。
    Help,
    /// 打印版本后退出。
    Version,
    /// 直达播放页并起播（真实窗口）。
    Play(Box<PlayArgs>),
}

/// `--play` 的参数。
#[derive(Clone, Debug, PartialEq)]
pub struct PlayArgs {
    /// 要播放的条目 id。
    pub item: String,
    /// 指定的播放版本；`None` 用第一个。
    pub source: Option<String>,
    /// 捕获模式：PNG 落盘路径。
    pub capture: Option<PathBuf>,
    /// 捕获前播放多少秒。
    pub seconds: f64,
    /// 首帧预算（秒），默认 1.5。真实 WAN 上的 4K HDR 需要放宽。
    pub max_first_frame: f64,
}

/// 解析后的命令行。
#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    /// 启动模式。
    pub mode: Mode,
    /// 起始页面（窗口模式忽略，始终从首页开始）。
    pub route: RouteArg,
    /// 指定的条目 id。
    pub item: Option<String>,
    /// 指定的服务器 id。
    pub server: Option<String>,
    /// 本地缓存里的标题过滤词。
    pub search: Option<String>,
    /// `--refresh`（`print-view` 用）。
    pub refresh: bool,
    /// `--no-refresh`（`snapshot` 用）。
    pub no_refresh: bool,
    /// `--play`（直达播放页）。
    pub play: Option<String>,
    /// `--source`（`--play` 时的版本）。
    pub source: Option<String>,
    /// `--capture`（`--play` 时的捕获路径）。
    pub capture: Option<PathBuf>,
    /// `--seconds`（`--play --capture` 的播放时长）。
    pub seconds: f64,
    /// `--max-first-frame`（首帧预算，秒）。
    pub max_first_frame: f64,
    /// `--screenshot <path>`：窗口自截（glReadPixels 回读自己的 back buffer）。
    pub screenshot: Option<PathBuf>,
    /// `--screenshot-delay <ms>`：等待多久再截（等首帧/数据加载）。
    pub screenshot_delay_ms: u64,
}

impl Cli {
    /// 解析参数；未知参数、缺值、模式冲突都返回 [`AppError::Usage`]。
    pub fn parse(argv: &[String]) -> Result<Self, AppError> {
        let mut cli = Self {
            mode: Mode::Window,
            route: RouteArg::default(),
            item: None,
            server: None,
            search: None,
            refresh: false,
            no_refresh: false,
            play: None,
            source: None,
            capture: None,
            seconds: 5.0,
            max_first_frame: 1.5,
            screenshot: None,
            screenshot_delay_ms: 1500,
        };
        let mut snapshot: Option<PathBuf> = None;
        let mut print_view = false;

        let mut index = 0;
        while index < argv.len() {
            let arg = argv[index].as_str();
            match arg {
                "-h" | "--help" => {
                    cli.mode = Mode::Help;
                    return Ok(cli);
                }
                "-V" | "--version" => {
                    cli.mode = Mode::Version;
                    return Ok(cli);
                }
                "--print-view" => print_view = true,
                "--refresh" => cli.refresh = true,
                "--no-refresh" => cli.no_refresh = true,
                "--snapshot" => {
                    let value = next_value(argv, index, "--snapshot")?;
                    snapshot = Some(PathBuf::from(value));
                    index += 1;
                }
                "--route" => {
                    let value = next_value(argv, index, "--route")?;
                    cli.route = RouteArg::parse(value).ok_or_else(|| {
                        AppError::Usage(format!(
                            "--route 只接受 home / library / detail / player，收到 `{value}`"
                        ))
                    })?;
                    index += 1;
                }
                "--item" => {
                    cli.item = Some(next_value(argv, index, "--item")?.to_owned());
                    index += 1;
                }
                "--server" => {
                    cli.server = Some(next_value(argv, index, "--server")?.to_owned());
                    index += 1;
                }
                "--search" => {
                    cli.search = Some(next_value(argv, index, "--search")?.to_owned());
                    index += 1;
                }
                "--play" => {
                    cli.play = Some(next_value(argv, index, "--play")?.to_owned());
                    index += 1;
                }
                "--source" => {
                    cli.source = Some(next_value(argv, index, "--source")?.to_owned());
                    index += 1;
                }
                "--capture" => {
                    cli.capture = Some(PathBuf::from(next_value(argv, index, "--capture")?));
                    index += 1;
                }
                "--screenshot" => {
                    cli.screenshot = Some(PathBuf::from(next_value(argv, index, "--screenshot")?));
                    index += 1;
                }
                "--screenshot-delay" => {
                    let raw = next_value(argv, index, "--screenshot-delay")?;
                    cli.screenshot_delay_ms = raw
                        .parse::<u64>()
                        .ok()
                        .filter(|ms| *ms > 0)
                        .ok_or_else(|| {
                            AppError::Usage(format!(
                                "--screenshot-delay 需要正整数毫秒，收到 `{raw}`"
                            ))
                        })?;
                    index += 1;
                }
                "--max-first-frame" => {
                    let raw = next_value(argv, index, "--max-first-frame")?;
                    cli.max_first_frame = raw
                        .parse::<f64>()
                        .ok()
                        .filter(|value| value.is_finite() && *value > 0.0)
                        .ok_or_else(|| {
                            AppError::Usage(format!("--max-first-frame 需要正数，收到 `{raw}`"))
                        })?;
                    index += 1;
                }
                "--seconds" => {
                    let raw = next_value(argv, index, "--seconds")?;
                    cli.seconds = raw.parse::<f64>().map_err(|_| {
                        AppError::Usage(format!("--seconds 需要一个秒数，收到 `{raw}`"))
                    })?;
                    if !cli.seconds.is_finite() || cli.seconds <= 0.0 {
                        return Err(AppError::Usage(
                            "--seconds 必须是正数（单位：秒）".to_owned(),
                        ));
                    }
                    index += 1;
                }
                other if other.starts_with('-') => {
                    return Err(AppError::Usage(format!("未知选项 `{other}`")));
                }
                other => {
                    return Err(AppError::Usage(format!(
                        "不接受位置参数 `{other}`；用 --snapshot / --print-view 指定模式"
                    )));
                }
            }
            index += 1;
        }

        if cli.mode == Mode::Help || cli.mode == Mode::Version {
            return Ok(cli);
        }
        if let Some(item) = cli.play.take() {
            if snapshot.is_some() || print_view {
                return Err(AppError::Usage(
                    "--play 不能与 --snapshot / --print-view 同时使用".to_owned(),
                ));
            }
            if item.trim().is_empty() {
                return Err(AppError::Usage("--play 需要一个条目 id".to_owned()));
            }
            cli.mode = Mode::Play(Box::new(PlayArgs {
                item: item.trim().to_owned(),
                source: cli.source.take().map(|value| value.trim().to_owned()),
                capture: cli.capture.take(),
                seconds: cli.seconds,
                max_first_frame: cli.max_first_frame,
            }));
            return Ok(cli);
        }
        if cli.capture.is_some() || cli.source.is_some() || cli.seconds != 5.0 {
            return Err(AppError::Usage(
                "--capture / --source / --seconds 只能在 --play 下使用".to_owned(),
            ));
        }
        cli.mode = match (snapshot, print_view) {
            (Some(_), true) => {
                return Err(AppError::Usage(
                    "--snapshot 与 --print-view 不能同时使用".to_owned(),
                ));
            }
            (Some(path), false) => Mode::Snapshot(path),
            (None, true) => Mode::PrintView,
            (None, false) => Mode::Window,
        };
        Ok(cli)
    }

    /// 本次启动是否应当在构建视图前做一次联网刷新。
    ///
    /// 窗口模式总是后台刷新；`snapshot` 默认刷新一次（便于端到端验证），
    /// `print-view` 默认只读缓存（脚本/CI 要确定性）。
    pub fn should_refresh(&self) -> bool {
        match self.mode {
            Mode::Window => true,
            Mode::Snapshot(_) => !self.no_refresh,
            Mode::PrintView => self.refresh,
            Mode::Play(_) | Mode::Help | Mode::Version => false,
        }
    }

    /// 搜索词去空白；空串按"没有过滤"处理。
    pub fn search_term(&self) -> Option<&str> {
        self.search
            .as_deref()
            .map(str::trim)
            .filter(|term| !term.is_empty())
    }
}

fn next_value<'a>(argv: &'a [String], index: usize, flag: &str) -> Result<&'a str, AppError> {
    argv.get(index + 1)
        .map(String::as_str)
        .ok_or_else(|| AppError::Usage(format!("{flag} 缺少取值")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn defaults_to_window_and_home() {
        let cli = Cli::parse(&[]).expect("空参数应当合法");
        assert_eq!(cli.mode, Mode::Window);
        assert_eq!(cli.route, RouteArg::Home);
        assert!(cli.should_refresh());
        assert_eq!(cli.search_term(), None);
    }

    #[test]
    fn parses_snapshot_with_route_and_item() {
        let cli = Cli::parse(&argv(&[
            "--snapshot",
            "/tmp/x.png",
            "--route",
            "detail",
            "--item",
            "abc",
            "--no-refresh",
        ]))
        .expect("参数应当合法");
        assert_eq!(cli.mode, Mode::Snapshot(PathBuf::from("/tmp/x.png")));
        assert_eq!(cli.route, RouteArg::Detail);
        assert_eq!(cli.item.as_deref(), Some("abc"));
        assert!(!cli.should_refresh());
    }

    #[test]
    fn print_view_does_not_refresh_by_default() {
        let cli = Cli::parse(&argv(&["--print-view"])).expect("参数应当合法");
        assert_eq!(cli.mode, Mode::PrintView);
        assert!(!cli.should_refresh());
        let cli = Cli::parse(&argv(&["--print-view", "--refresh"])).expect("参数应当合法");
        assert!(cli.should_refresh());
    }

    #[test]
    fn rejects_conflicts_and_unknowns() {
        assert!(matches!(
            Cli::parse(&argv(&["--snapshot", "a.png", "--print-view"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--snapshot"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--route", "nowhere"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--wat"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["stray"])),
            Err(AppError::Usage(_))
        ));
    }

    #[test]
    fn parses_play_with_capture() {
        let cli = Cli::parse(&argv(&[
            "--play",
            "abc",
            "--capture",
            "/tmp/play.png",
            "--seconds",
            "5",
            "--source",
            "src-1",
        ]))
        .expect("参数应当合法");
        match cli.mode {
            Mode::Play(ref play) => {
                assert_eq!(play.item, "abc");
                assert_eq!(play.source.as_deref(), Some("src-1"));
                assert_eq!(play.capture, Some(PathBuf::from("/tmp/play.png")));
                assert_eq!(play.seconds, 5.0);
            }
            other => panic!("应当是 --play 模式：{other:?}"),
        }
        assert!(!cli.should_refresh(), "播放模式不做首页刷新");
    }

    #[test]
    fn play_defaults_to_five_seconds_without_capture() {
        let cli = Cli::parse(&argv(&["--play", "abc"])).expect("参数应当合法");
        match cli.mode {
            Mode::Play(play) => {
                assert!(play.capture.is_none());
                assert_eq!(play.seconds, 5.0);
            }
            other => panic!("应当是 --play 模式：{other:?}"),
        }
    }

    #[test]
    fn play_conflicts_are_rejected() {
        assert!(matches!(
            Cli::parse(&argv(&["--play", "abc", "--snapshot", "x.png"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--capture", "/tmp/x.png"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--play", "abc", "--seconds", "0"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--play", "abc", "--seconds", "abc"])),
            Err(AppError::Usage(_))
        ));
        assert!(matches!(
            Cli::parse(&argv(&["--play"])),
            Err(AppError::Usage(_))
        ));
    }

    #[test]
    fn help_and_version_short_circuit() {
        let cli = Cli::parse(&argv(&["--help", "--snapshot"])).expect("帮助优先");
        assert_eq!(cli.mode, Mode::Help);
        let cli = Cli::parse(&argv(&["-V"])).expect("版本优先");
        assert_eq!(cli.mode, Mode::Version);
    }

    #[test]
    fn search_term_ignores_blank() {
        let cli = Cli::parse(&argv(&["--print-view", "--search", "  "])).expect("参数应当合法");
        assert_eq!(cli.search_term(), None);
        let cli = Cli::parse(&argv(&["--print-view", "--search", " 沙丘 "])).expect("参数应当合法");
        assert_eq!(cli.search_term(), Some("沙丘"));
    }
}
