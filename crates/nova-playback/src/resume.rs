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
/// 分支顺序有讲究：`played` 最先判，因为服务端可能在用户看完后又保留了一个接近片尾的
/// `resume_position`；此时按"看完"处理而不是"续播到片尾"。
pub fn resume_position(
    user_data: &UserData,
    duration: Option<Duration>,
    prefs: &UserPreferences,
) -> Duration {
    if user_data.played {
        return Duration::ZERO;
    }
    let resume = user_data.resume_position;
    if resume < prefs.resume_min_position {
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
pub fn resume_label(played: bool, position: Duration) -> String {
    if played || position.is_zero() {
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

    #[test]
    fn played_item_starts_over() {
        assert_eq!(
            resume_position(&user(true, 3000), Some(Duration::from_secs(7200)), &prefs()),
            Duration::ZERO
        );
        assert_eq!(resume_label(true, Duration::from_secs(3000)), "播放");
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
        assert_eq!(resume_label(false, Duration::ZERO), "播放");
        assert_eq!(
            resume_label(false, Duration::from_secs(754)),
            "从 12:34 继续"
        );
        assert_eq!(
            resume_label(false, Duration::from_secs(3723)),
            "从 1:02:03 继续"
        );
        assert_eq!(resume_label(true, Duration::ZERO), "播放");
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
