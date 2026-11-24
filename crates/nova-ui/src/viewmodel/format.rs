// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 格式化：进度、时长、元信息拼接、badge 判定。
//!
//! 这些是最容易出错的界面逻辑，放在纯 Rust 里用单测覆盖。

use std::time::Duration;

/// 把 `0.0..=1.0` 的进度格式化成百分比；`None` 返回空串。
///
/// 超出范围的输入会被夹到 `0%` / `100%`。
pub fn progress_percent(progress: Option<f32>) -> String {
    match progress {
        Some(value) => format!("{:.0}%", value.clamp(0.0, 1.0) * 100.0),
        None => String::new(),
    }
}

/// 格式化时长：`"2 小时 18 分"` / `"48 分"` / `"小于 1 分"`。
///
/// 分钟向下取整；不足一分钟时返回 `"小于 1 分"`，不显示 `"0 分"`。
pub fn format_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    if total < 60 {
        return "小于 1 分".to_string();
    }
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    match (hours, minutes) {
        (0, minutes) => format!("{minutes} 分"),
        (hours, 0) => format!("{hours} 小时"),
        (hours, minutes) => format!("{hours} 小时 {minutes} 分"),
    }
}

/// 用 `" · "` 拼接元信息，自动丢弃空项。
///
/// `["2021", "", "动作"]` → `"2021 · 动作"`，不会留下双分隔符。
pub fn meta_line(parts: &[&str]) -> String {
    let mut kept: Vec<&str> = Vec::with_capacity(parts.len());
    for part in parts {
        let trimmed = part.trim();
        if !trimmed.is_empty() {
            kept.push(trimmed);
        }
    }
    kept.join(" · ")
}

/// 详情页元信息：年份 · 时长 · 类型（类型内部用 `" / "`）。
pub fn detail_meta_line(year: Option<u16>, runtime: Option<Duration>, genres: &[String]) -> String {
    let year_text = year.map(|value| value.to_string());
    let runtime_text = runtime
        .filter(|duration| !duration.is_zero())
        .map(format_duration);
    let genre_text = if genres.is_empty() {
        None
    } else {
        Some(genres.join(" / "))
    };
    let parts = [
        year_text.as_deref().unwrap_or_default(),
        runtime_text.as_deref().unwrap_or_default(),
        genre_text.as_deref().unwrap_or_default(),
    ];
    meta_line(&parts)
}

/// 单集编号：`"S01E05"`。季/集号缺失时返回 `None`。
pub fn episode_code(season: Option<u32>, episode: Option<u32>) -> Option<String> {
    match (season, episode) {
        (Some(season), Some(episode)) => Some(format!("S{season:02}E{episode:02}")),
        _ => None,
    }
}

/// badge 优先级：`4K` > `HDR` > 单集编号。
pub fn badge(four_k: bool, hdr: bool, season: Option<u32>, episode: Option<u32>) -> Option<String> {
    if four_k {
        return Some("4K".to_string());
    }
    if hdr {
        return Some("HDR".to_string());
    }
    episode_code(season, episode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_formats_percent_and_empty() {
        assert_eq!(progress_percent(Some(0.37)), "37%");
        assert_eq!(progress_percent(None), "");
        assert_eq!(progress_percent(Some(0.0)), "0%");
        assert_eq!(progress_percent(Some(1.0)), "100%");
        assert_eq!(progress_percent(Some(-1.0)), "0%");
        assert_eq!(progress_percent(Some(2.0)), "100%");
    }

    #[test]
    fn duration_uses_hours_minutes_and_less_than_a_minute() {
        assert_eq!(
            format_duration(Duration::from_secs(2 * 3600 + 18 * 60)),
            "2 小时 18 分"
        );
        assert_eq!(format_duration(Duration::from_secs(48 * 60)), "48 分");
        assert_eq!(format_duration(Duration::from_secs(0)), "小于 1 分");
        assert_eq!(format_duration(Duration::from_secs(59)), "小于 1 分");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1 小时");
    }

    #[test]
    fn meta_line_skips_missing_parts() {
        assert_eq!(meta_line(&["2021", "", "动作"]), "2021 · 动作");
        assert_eq!(meta_line(&["", "  ", ""]), "");
        assert_eq!(
            detail_meta_line(Some(2021), None, &["动作".to_string(), "科幻".to_string()]),
            "2021 · 动作 / 科幻"
        );
        assert_eq!(
            detail_meta_line(
                Some(2021),
                Some(Duration::from_secs(2 * 3600 + 18 * 60)),
                &[]
            ),
            "2021 · 2 小时 18 分"
        );
    }

    #[test]
    fn badge_priority_is_4k_then_hdr_then_episode() {
        assert_eq!(badge(true, true, Some(1), Some(5)), Some("4K".to_string()));
        assert_eq!(
            badge(false, true, Some(1), Some(5)),
            Some("HDR".to_string())
        );
        assert_eq!(
            badge(false, false, Some(1), Some(5)),
            Some("S01E05".to_string())
        );
        assert_eq!(badge(false, false, None, None), None);
        assert_eq!(episode_code(Some(12), Some(3)), Some("S12E03".to_string()));
        assert_eq!(episode_code(Some(1), None), None);
    }
}
