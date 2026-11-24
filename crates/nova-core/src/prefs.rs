// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 用户偏好。全部本地保存（SQLite），默认值面向"中文用户的家庭影院"场景。

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::language::Language;

/// 字幕策略。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleMode {
    /// 有字幕就开（按语言偏好挑一条）。
    #[default]
    Always,
    /// 只在外语音轨时开。
    ForeignAudioOnly,
    /// 只看强制字幕。
    Forced,
    /// 从不自动开启（用户仍可手动选中）。
    Never,
}

/// 界面主题。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// 跟随系统。
    #[default]
    System,
    /// 深色。
    Dark,
    /// 浅色。
    Light,
}

/// 用户偏好。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
    /// 字幕语言优先级（按顺序匹配）。
    pub subtitle_languages: Vec<Language>,
    /// 字幕策略。
    pub subtitle_mode: SubtitleMode,
    /// 是否接受听障字幕。
    pub allow_hearing_impaired: bool,
    /// 音轨语言优先级。
    pub audio_languages: Vec<Language>,
    /// 音轨是否优先选环绕声（同一语言下）。
    pub prefer_surround_audio: bool,
    /// 界面主题。
    pub theme: Theme,
    /// 启动即全屏。
    pub start_fullscreen: bool,
    /// 进度超过该值才计入"继续观看"。
    pub resume_min_position: Duration,
    /// 剩余时长小于该值即视为看完。
    pub finished_threshold: Duration,
    /// 播放进度上报间隔。
    pub progress_report_interval: Duration,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            subtitle_languages: vec![Language::new("zh-Hans"), Language::english()],
            subtitle_mode: SubtitleMode::Always,
            allow_hearing_impaired: false,
            audio_languages: vec![Language::new("zh"), Language::english()],
            prefer_surround_audio: true,
            theme: Theme::System,
            start_fullscreen: false,
            resume_min_position: Duration::from_secs(30),
            finished_threshold: Duration::from_secs(90),
            progress_report_interval: Duration::from_secs(10),
        }
    }
}

impl UserPreferences {
    /// 上报道具间隔至少 5 秒，避免用户把间隔配成 0 之后打成请求风暴。
    pub fn effective_report_interval(&self) -> Duration {
        self.progress_report_interval.max(Duration::from_secs(5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let prefs = UserPreferences::default();
        assert_eq!(prefs.subtitle_mode, SubtitleMode::Always);
        assert_eq!(prefs.effective_report_interval(), Duration::from_secs(10));
        assert!(prefs.finished_threshold >= Duration::from_secs(60));
    }

    #[test]
    fn report_interval_is_clamped() {
        let prefs = UserPreferences {
            progress_report_interval: Duration::from_secs(1),
            ..UserPreferences::default()
        };
        assert_eq!(prefs.effective_report_interval(), Duration::from_secs(5));

        let prefs = UserPreferences {
            progress_report_interval: Duration::ZERO,
            ..UserPreferences::default()
        };
        assert_eq!(prefs.effective_report_interval(), Duration::from_secs(5));
    }

    #[test]
    fn serde_roundtrip() {
        let prefs = UserPreferences::default();
        let json = serde_json::to_string(&prefs).unwrap();
        let back: UserPreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(back, prefs);
    }
}
