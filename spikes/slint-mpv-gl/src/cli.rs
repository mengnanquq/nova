// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 与窗口无关的参数解析（放在 lib 里是为了能跑单元测试）。

/// 解析 `--size WxH`。
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("--size 需要 WxH 形式，收到 `{text}`"))?;
    let w: u32 = w
        .trim()
        .parse()
        .map_err(|e| format!("--size 宽度非法: {e}"))?;
    let h: u32 = h
        .trim()
        .parse()
        .map_err(|e| format!("--size 高度非法: {e}"))?;
    if w == 0 || h == 0 || w > 7680 || h > 4320 {
        return Err(format!("--size {w}x{h} 超出合理范围"));
    }
    Ok((w, h))
}
