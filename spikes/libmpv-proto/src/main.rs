// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 命令行入口：`probe` / `bench` / `dump`。

use std::path::PathBuf;

use libmpv_proto::bench::{self, BenchOptions};
use libmpv_proto::player::SessionOptions;
use libmpv_proto::{SurfaceFormat, dump, parse_format, probe};

const USAGE: &str = "\
libmpv-proto —— libmpv software 渲染 API（headless）原型

用法:
  libmpv-proto probe <file>                 打印容器/轨道信息、hwdec-current、色彩参数、HDR 元数据
  libmpv-proto bench <file> [-n 300]        用 SW 渲染出 N 帧并报告每帧耗时 p50/p95
  libmpv-proto dump  <file> [-n 5] [-o out] 导出前 N 帧为 PNG（默认写到 ./out）

通用参数:
  --format rgb0|bgr0   SW 渲染目标格式（默认 rgb0，即内存顺序 R,G,B,X）
  --hwdec <mode>       mpv 的 hwdec 值（默认 auto-safe）
  --repeat <n>         bench 结束时在同一帧上重复渲染的次数（默认 30）
  -v, --verbose        打开 mpv 自己的日志（排查解码/时序问题用）
  --size <WxH>         渲染目标尺寸（默认=视频尺寸；其他尺寸会被 mpv 缩放并加黑边）
  --speed <n>          bench 用的播放倍率（默认 100，mpv 上限；用于抵消播放时钟节流）
  --opt <name>=<value> 透传一个 mpv option（可重复，例如 --opt hwdec=no）
  -h, --help           显示本帮助
";

fn main() {
    if let Err(err) = run() {
        eprintln!("错误: {err}");
        std::process::exit(1);
    }
}

struct Args {
    file: PathBuf,
    frames: Option<usize>,
    out: Option<PathBuf>,
    format: SurfaceFormat,
    hwdec: String,
    repeat: usize,
    verbose: bool,
    speed: f64,
    size: Option<(u32, u32)>,
    opts: Vec<(String, String)>,
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = argv.first() else {
        print!("{USAGE}");
        return Err("缺少子命令".to_string());
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        print!("{USAGE}");
        return Ok(());
    }
    let command = command.clone();
    let args = parse_args(&argv[1..])?;
    // mpv option 的取值要活到 Session 用完，所以先放在 args 外面。
    let mut option_storage: Vec<(String, String)> = Vec::new();
    if args.verbose {
        option_storage.push(("terminal".into(), "yes".into()));
        option_storage.push(("msg-level".into(), "all=info".into()));
    }
    option_storage.extend(args.opts.iter().cloned());
    let extra: Vec<(&str, &str)> = option_storage
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let session = SessionOptions {
        hwdec: &args.hwdec,
        extra: &extra,
    };

    match command.as_str() {
        "probe" => {
            probe::run(&args.file, &session)?;
            Ok(())
        }
        "bench" => {
            let opts = BenchOptions {
                frames: args.frames.unwrap_or(300),
                format: args.format,
                repeat: args.repeat,
                speed: args.speed,
                size: args.size,
                ..BenchOptions::default()
            };
            let report = bench::run(&args.file, &opts, &session)?;
            bench::print_report(&args.file, &report, args.format);
            Ok(())
        }
        "dump" => {
            let out_dir = args.out.unwrap_or_else(|| PathBuf::from("out"));
            let n = args.frames.unwrap_or(5);
            println!(
                "导出 {n} 帧到 {}（格式 {}）",
                out_dir.display(),
                args.format.label()
            );
            dump::run(&args.file, &out_dir, n, args.format, &session)?;
            Ok(())
        }
        other => Err(format!("未知子命令 `{other}`\n\n{USAGE}")),
    }
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut file: Option<PathBuf> = None;
    let mut frames: Option<usize> = None;
    let mut out: Option<PathBuf> = None;
    let mut format = SurfaceFormat::Rgb0;
    let mut hwdec = "auto-safe".to_string();
    let mut repeat = 30usize;
    let mut verbose = false;
    let mut opts: Vec<(String, String)> = Vec::new();
    let mut speed = 100.0f64;
    let mut size: Option<(u32, u32)> = None;

    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        let mut next_value = |what: &str| -> Result<String, String> {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("`{what}` 缺少取值"))
        };
        match arg.as_str() {
            "-n" | "--frames" => {
                let v = next_value("-n")?;
                frames = Some(v.parse().map_err(|e| format!("`-n {v}` 不是正整数: {e}"))?);
            }
            "-o" | "--out" => out = Some(PathBuf::from(next_value("-o")?)),
            "--format" => format = parse_format(&next_value("--format")?)?,
            "--hwdec" => hwdec = next_value("--hwdec")?,
            "--size" => {
                let v = next_value("--size")?;
                let (w, h) = v
                    .split_once(['x', 'X'])
                    .ok_or_else(|| format!("`--size {v}` 需要 WxH 形式"))?;
                let w: u32 = w.parse().map_err(|e| format!("宽度 `{w}`: {e}"))?;
                let h: u32 = h.parse().map_err(|e| format!("高度 `{h}`: {e}"))?;
                size = Some((w, h));
            }
            "--speed" => {
                let v = next_value("--speed")?;
                speed = v
                    .parse()
                    .map_err(|e| format!("`--speed {v}` 不是数字: {e}"))?;
            }
            "--repeat" => {
                let v = next_value("--repeat")?;
                repeat = v
                    .parse()
                    .map_err(|e| format!("`--repeat {v}` 不是正整数: {e}"))?;
            }
            "-v" | "--verbose" => verbose = true,
            "--opt" => {
                let v = next_value("--opt")?;
                let (name, value) = v
                    .split_once('=')
                    .ok_or_else(|| format!("`--opt {v}` 需要 name=value 形式"))?;
                opts.push((name.to_string(), value.to_string()));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("未知参数 `{other}`\n\n{USAGE}"));
            }
            other => {
                if file.is_some() {
                    return Err(format!("只接受一个输入文件，多余的: `{other}`"));
                }
                file = Some(PathBuf::from(other));
            }
        }
    }

    Ok(Args {
        file: file.ok_or("缺少输入文件")?,
        frames,
        out,
        format,
        hwdec,
        repeat,
        verbose,
        speed,
        size,
        opts,
    })
}
