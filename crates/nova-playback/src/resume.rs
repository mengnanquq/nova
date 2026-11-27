// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 续播策略（纯函数）。
//!
//! "继续观看"的四个判定只要有一个写错，用户就会看到"又从 0 开始播"或者"点进去直接跳到片尾"。
//! 所以这里全是纯函数，逐条测试。
//!
//! 阈值用的是 [`UserPreferences::resume_min_position`]（太靠前不算进度）与
//! [`UserPreferences::finished_threshold`]（剩余时长，不是百分比——片尾字幕往往好几分钟）。

use std::time::Duration;

use nova_core::{MediaItem, UserData, UserPreferences};

/// 该从哪儿起播。
///
/// 返回 `Duration::ZERO` 表示从头播（按钮文案也会是"播放"）。
///
/// **`played` 不参与判定**：服务端会把一部看完的片子重新标成未看完，也会给
/// `Played = true` 的条目保留一个中段的续播点（用户看完后又跳到中间继续看）。
/// 只要续播点还在有效区间内（≥ `resume_min_position`，且剩余时长多于
/// `finished_threshold`）就从那里续播；"已看完"只由"续播点为空 / 太靠前 /
/// 已经贴近片尾"表达。这与 Emby 官方客户端一致：从 0 起播会让服务端把续播点
/// 清零（播放进度达不到 MinResumePct），用户再也回不到原来的位置。
pub fn resume_position(
    user_data: &UserData,
    duration: Option<Duration>,
    prefs: &UserPreferences,
) -> Duration {
    let resume = user_data.resume_position;
    if resume.is_zero() || resume < prefs.resume_min_position {
        return Duration::ZERO;
    }
    if let Some(total) = duration {
        // 剩余时长不足阈值 = 已看完（片尾字幕 / 片尾曲）。
        if total.saturating_sub(resume) <= prefs.finished_threshold {
            return Duration::ZERO;
        }
    }
    resume
}

/// [`resume_position`] 的便捷版本：时长取条目自己的 `runtime`。
pub fn resume_position_for_item(item: &MediaItem, prefs: &UserPreferences) -> Duration {
    resume_position(&item.user_data, item.runtime, prefs)
}

/// 播放按钮文案：`"播放"` / `"从 12:34 继续"`。
///
/// 调用方应传入已经过 [`resume_position`] 处理的位置，这样"从头播"和"已看完"天然落到"播放"。
/// **不要再自己拿 `played` 或原始 `resume_position` 判文案**：那正是"按钮写着从 45:47
/// 继续、实际却从 0 起播"的来源。
pub fn resume_label(position: Duration) -> String {
    if position.is_zero() {
        return "播放".to_owned();
    }
    format!("从 {} 继续", format_position(position))
}

/// 位置文案：`"12:34"`、`"1:02:03"`。
fn format_position(position: Duration) -> String {
    let total = position.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::{ImageSet, ItemId, MediaKind};

    fn prefs() -> UserPreferences {
        UserPreferences::default()
    }

    fn user(played: bool, resume_secs: u64) -> UserData {
        UserData {
            played,
            resume_position: Duration::from_secs(resume_secs),
            ..UserData::default()
        }
    }

    /// 体检 R7-D2：`played` 不再一票否决。
    ///
    /// 6489 在服务端是 `Played = true` 且带 resume（45:47）。以前这里返回 0，从 0 起播
    /// 后进度到不了 Emby 的 MinResumePct，服务端把续播点清零——再也回不到原位置。
    #[test]
    fn played_with_mid_resume_resumes() {
        let resume = Duration::from_secs(2747);
        let total = Duration::from_secs(5483);
        assert_eq!(
            resume_position(&user(true, 2747), Some(total), &prefs()),
            resume
        );
        assert_eq!(resume_label(resume), "从 45:47 继续");
    }

    /// `played` 且续播点已经贴近片尾（或根本没有续播点）：仍然从头播。
    #[test]
    fn played_at_end_starts_over() {
        let prefs = prefs();
        // 剩 50s < finished_threshold(90s)：看完的片子不能一进去就跳片尾。
        assert_eq!(
            resume_position(&user(true, 4833), Some(Duration::from_secs(4883)), &prefs),
            Duration::ZERO
        );
        // 没有续播点。
        assert_eq!(
            resume_position(&user(true, 0), Some(Duration::from_secs(7200)), &prefs),
            Duration::ZERO
        );
    }

    #[test]
    fn too_early_position_starts_over() {
        let prefs = prefs();
        assert_eq!(prefs.resume_min_position, Duration::from_secs(30));
        assert_eq!(
            resume_position(&user(false, 29), Some(Duration::from_secs(7200)), &prefs),
            Duration::ZERO
        );
        // 正好等于阈值算有效进度。
        assert_eq!(
            resume_position(&user(false, 30), Some(Duration::from_secs(7200)), &prefs),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn finished_threshold_uses_remaining_time() {
        let prefs = prefs();
        assert_eq!(prefs.finished_threshold, Duration::from_secs(90));
        // 剩 90s：正好等于阈值 → 视为看完。
        assert_eq!(
            resume_position(&user(false, 7110), Some(Duration::from_secs(7200)), &prefs),
            Duration::ZERO
        );
        // 剩 91s：还没看完 → 续播。
        assert_eq!(
            resume_position(&user(false, 7109), Some(Duration::from_secs(7200)), &prefs),
            Duration::from_secs(7109)
        );
        // 位置越过时长（服务端脏数据）也不会 panic。
        assert_eq!(
            resume_position(&user(false, 9000), Some(Duration::from_secs(7200)), &prefs),
            Duration::ZERO
        );
    }

    #[test]
    fn normal_resume_is_kept() {
        assert_eq!(
            resume_position(&user(false, 754), Some(Duration::from_secs(7200)), &prefs()),
            Duration::from_secs(754)
        );
        // 时长未知时无法判"看完"，保守地续播。
        assert_eq!(
            resume_position(&user(false, 754), None, &prefs()),
            Duration::from_secs(754)
        );
    }

    #[test]
    fn label_variants() {
        assert_eq!(resume_label(Duration::ZERO), "播放");
        assert_eq!(resume_label(Duration::from_secs(754)), "从 12:34 继续");
        assert_eq!(resume_label(Duration::from_secs(3723)), "从 1:02:03 继续");
    }

    #[test]
    fn item_helper_uses_runtime() {
        let item = |resume_secs: u64, runtime_secs: Option<u64>| MediaItem {
            id: ItemId::new("1"),
            kind: MediaKind::Movie,
            title: "T".into(),
            sort_title: String::new(),
            original_title: None,
            year: None,
            overview: None,
            genres: Vec::new(),
            runtime: runtime_secs.map(Duration::from_secs),
            parent_id: None,
            index_number: None,
            index_number_end: None,
            child_count: None,
            images: ImageSet::default(),
            user_data: user(false, resume_secs),
        };

        assert_eq!(
            resume_position_for_item(&item(754, Some(7200)), &prefs()),
            Duration::from_secs(754)
        );
        assert_eq!(
            resume_position_for_item(&item(754, Some(800)), &prefs()),
            Duration::ZERO
        );
    }
}
