// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `playback-proto` 命令行入口。
//!
//! ```text
//! playback-proto probe <media>
//! playback-proto dump  <media> [-n 5] [-o target/frames]
//! playback-proto bench <media> [-n 300]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use playback_proto::gst_source::GstSource;
use playback_proto::{Error, FrameSource, Result, bench, dump};

const USAGE: &str = "\
用法:
  playback-proto probe <media>                         打印解码器 / 硬解判定 / caps / 流信息
  playback-proto dump  <media> [-n 5] [-o <目录>]      导出前 N 帧为 PNG
  playback-proto bench <media> [-n 300]                解码 N 帧，报三段耗时 p50/p95

<media> 可以是本地路径，也可以是 http(s):// URL（需要 souphttpsrc）。
";

struct Options {
    media: String,
    count: Option<usize>,
    out_dir: Option<PathBuf>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<(String, Options)> {
    let command = args
        .next()
        .ok_or_else(|| Error::Format("缺少子命令".into()))?;
    let mut media = None;
    let mut count = None;
    let mut out_dir = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-n" | "--frames" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::Format(format!("{arg} 后面缺少数字")))?;
                let parsed = value
                    .parse::<usize>()
                    .map_err(|err| Error::Format(format!("{arg} 的值 {value} 无法解析: {err}")))?;
                count = Some(parsed);
            }
            "-o" | "--out" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::Format(format!("{arg} 后面缺少目录")))?;
                out_dir = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other if media.is_none() => media = Some(other.to_string()),
            other => return Err(Error::Format(format!("无法识别的参数 {other}"))),
        }
    }
    let media = media.ok_or_else(|| Error::Format("缺少 <media> 参数".into()))?;
    Ok((
        command,
        Options {
            media,
            count,
            out_dir,
        },
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, options) = match parse_args(args.into_iter()) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("错误: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&command, &options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("错误: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: &str, options: &Options) -> Result<()> {
    match command {
        "probe" => probe(&options.media),
        "dump" => {
            let count = options.count.unwrap_or(5);
            let out_dir = options
                .out_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("target/frames"));
            let mut source = GstSource::open(&options.media)?;
            println!("{}", source.description().render());
            let written = dump::dump_frames(&mut source, count, &out_dir)?;
            println!("导出 {} 帧到 {}:", written.len(), out_dir.display());
            for path in written {
                let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                println!("  {} ({} 字节)", path.display(), size);
            }
            Ok(())
        }
        "bench" => {
            let frames = options.count.unwrap_or(300);
            let mut source = GstSource::open(&options.media)?;
            println!("{}", source.description().render());
            let report = bench::run(&mut source, frames)?;
            println!("{}", report.render());
            Ok(())
        }
        other => Err(Error::Format(format!(
            "未知子命令 {other}（支持 probe / dump / bench）"
        ))),
    }
}

fn probe(media: &str) -> Result<()> {
    let mut source = GstSource::open(media)?;
    let description = source.description();
    println!("{}", description.render());

    // 拉一帧证明解码真的能出数据，而不是只有 caps。
    match source.next_frame()? {
        Some(frame) => println!(
            "首帧            : {}x{} stride={} {} 字节 pts={} ns",
            frame.width,
            frame.height,
            frame.stride,
            frame.data.len(),
            frame
                .pts_ns
                .map(|pts| pts.to_string())
                .unwrap_or_else(|| "（无）".to_string())
        ),
        None => println!("首帧            : 没有解出帧（EOS）"),
    }

    #[cfg(feature = "slint")]
    {
        let steps = playback_proto::slint_bridge::probe_no_backend();
        println!(
            "{}",
            playback_proto::slint_bridge::render_no_backend_probe(&steps)
        );
    }
    Ok(())
}
