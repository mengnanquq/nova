// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 体检 R7-D2：详情页的播放文案与真正的起播位置必须**永远一致**。
//!
//! 回归背景：`detail_from_item` 以前自己算文案（直接用原始 `resume_position`），
//! 起播却走 `resume_position_for_item`。6489 在服务端是 `Played = true` 且带 45:47
//! 的续播点：按钮写着「从 45:47 继续」，实际从头播，进度到不了 Emby 的 MinResumePct，
//! 服务端把续播点清零——用户再也回不到原来的位置。
//!
//! 这里不重复测 `resume_position` 的每个分支（那在 `nova-playback` 里），只钉住
//! "文案 = `resume_label(实际起播位置)`" 这条不变量。

use std::time::Duration;

use nova_app::views::detail_from_item;
use nova_core::{ImageSet, ItemId, MediaItem, MediaKind, UserData, UserPreferences};
use nova_playback::{resume_label, resume_position_for_item};

fn item(played: bool, resume_secs: u64, runtime_secs: u64) -> MediaItem {
    MediaItem {
        id: ItemId::new("6489"),
        kind: MediaKind::Movie,
        title: "测试片".to_owned(),
        sort_title: String::new(),
        original_title: None,
        year: None,
        overview: None,
        genres: Vec::new(),
        runtime: Some(Duration::from_secs(runtime_secs)),
        parent_id: None,
        index_number: None,
        index_number_end: None,
        child_count: None,
        images: ImageSet::default(),
        user_data: UserData {
            played,
            resume_position: Duration::from_secs(resume_secs),
            ..UserData::default()
        },
    }
}

/// 详情页文案与起播位置同源：`Played = true` + 中段续播点也不能例外。
#[test]
fn detail_label_matches_the_position_playback_starts_from() {
    let prefs = UserPreferences::default();
    for (played, resume_secs, runtime_secs) in [
        // 6489 的真实形态：看完过，但服务端仍留着一个 45:47 的续播点。
        (true, 2747, 5483),
        // 常规的"看了 12:34 还没看完"。
        (false, 754, 7200),
        // `Played = true` 且续播点贴近片尾：从头播，文案也必须是「播放」。
        (true, 5440, 5483),
        // 续播点太靠前（< resume_min_position）：同样从头播。
        (false, 10, 7200),
        // 没有续播点。
        (true, 0, 7200),
    ] {
        let item = item(played, resume_secs, runtime_secs);
        let at = resume_position_for_item(&item, &prefs);
        let view = detail_from_item(&item, &[], &prefs);
        assert_eq!(
            view.play_label,
            resume_label(at),
            "played={played} resume={resume_secs}s runtime={runtime_secs}s：\
             文案必须与起播位置同源（起播 {}s）",
            at.as_secs()
        );
    }
}

/// 主用例：`Played = true` + 45:47 的续播点 → 文案「从 45:47 继续」，且真的从那里起播。
#[test]
fn played_item_with_a_mid_resume_point_keeps_the_label_and_the_position() {
    let prefs = UserPreferences::default();
    let item = item(true, 2747, 5483);
    let view = detail_from_item(&item, &[], &prefs);
    assert_eq!(view.play_label, "从 45:47 继续");
    assert_eq!(
        resume_position_for_item(&item, &prefs),
        Duration::from_secs(2747),
        "文案说从 45:47 继续，起播就必须是 45:47（`played` 不能一票否决）"
    );
}
