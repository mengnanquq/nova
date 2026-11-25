// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放页视图模型（阶段 4）：控制层、音轨 / 字幕菜单、缓冲与错误、自动隐藏。
//!
//! 这里是**纯数据 + 纯函数**：不碰播放引擎、不做 I/O、不依赖任何时钟。
//! 时间基（`Duration`）由上层决定怎么推进，所以：
//!
//! * 进度 / 缓冲 / 剩余时长都是 `Duration`，格式化在本文件里用单测覆盖；
//! * 控制层自动隐藏由 [`PlayerView::tick`] 按"增量"推进，测试可以直接灌虚拟时间，
//!   真实窗口则把 Slint `Timer` 的间隔当增量喂进来（见 `pages::player::TICK_INTERVAL`）。

use std::time::Duration;

/// 控制层自动隐藏延迟：3 秒无输入（规范"播放器交互规范（阶段 4）/ 控制层"）。
pub const AUTO_HIDE_DELAY: Duration = Duration::from_secs(3);

/// 音量快捷键步长：`↑` / `↓` 每次 ±5%。
pub const VOLUME_STEP: f32 = 0.05;

/// 音量 / 静音调整后数字文本的可见时长。
///
/// 常驻的只有喇叭图标；数字只在“刚调过”时短时出现，不然一排数字抢注意力。
pub const VOLUME_HINT_DURATION: Duration = Duration::from_millis(1200);

/// 字幕抬升量相对画面高度的比例（控制层可见时生效）。
///
/// 800px 高的窗口 → 280px，正好把字幕抬到传输行上方。
pub const SUBTITLE_SAFE_AREA_RATIO: f32 = 0.35;
/// 字幕抬升下限（小窗口不至于把字幕顶到画面正中）。
pub const SUBTITLE_MARGIN_MIN: i64 = 160;
/// 字幕抬升上限（2K / 4K 大窗口不要抬过头）。
pub const SUBTITLE_MARGIN_MAX: i64 = 400;

/// 时间行的渲染模式：已播/总长 与 剩余时长**二选一**（不同时出现）。
///
/// 默认 [`TimeMode::Remaining`]：官方播放器的时间位显示的就是**剩余时长**
/// （`.time-remaining__time`），详情页/媒体库也不该出现"已播/总长"这种调试口吻。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeMode {
    /// `"12:34 / 2:18:00"`。
    Elapsed,
    /// `"-2:05:26"`。
    #[default]
    Remaining,
}

/// 音轨 / 字幕列表项。列表来自引擎真实枚举，不猜。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackItem {
    /// 引擎侧的轨道编号（mpv 的 `aid` / `sid`）。
    pub index: u32,
    /// 主文案，例如 `"国语 · AC3 5.1"` 或 `"简体中文 (SRT)"`。
    pub label: String,
    /// 次要文案，例如 `"2 声道 · 48 kHz"`。
    pub detail: Option<String>,
    /// 是否是当前选中项。
    pub selected: bool,
    /// 图形字幕（PGS / VobSub）：样式由引擎渲染，界面不能承诺可改字体。
    pub is_bitmap_subtitle: bool,
}

impl TrackItem {
    /// 新建一条轨道。
    pub fn new(index: u32, label: impl Into<String>) -> Self {
        Self {
            index,
            label: label.into(),
            detail: None,
            selected: false,
            is_bitmap_subtitle: false,
        }
    }

    /// 设置次要文案。
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// 设置是否选中。
    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// 标记为图形字幕（界面会追加"引擎渲染，样式不可改"）。
    pub fn with_bitmap_subtitle(mut self) -> Self {
        self.is_bitmap_subtitle = true;
        self
    }
}

/// 播放页视图模型。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerView {
    /// 主标题（片名）。
    pub title: String,
    /// 副标题（`"第 2 季 · 第 5 集"` / `"2024 · 科幻"`）。
    pub subtitle: Option<String>,
    /// 当前播放位置。
    pub position: Duration,
    /// 总时长；未知时为 `None`（进度条退化为不确定态）。
    pub duration: Option<Duration>,
    /// 已缓冲位置。
    pub buffered: Option<Duration>,
    /// 是否正在播放。
    pub playing: bool,
    /// 是否正在缓冲（画面保留，角落提示，控件不自动隐藏）。
    pub buffering: bool,
    /// 音量 `0.0..=1.0`。
    pub volume: f32,
    /// 是否静音。
    pub muted: bool,
    /// 播放速率（`1.0` 为正常）。
    pub rate: f32,
    /// 音轨列表。
    pub audio_tracks: Vec<TrackItem>,
    /// 字幕列表（"关闭"不在此列，界面上单独一条）。
    pub subtitle_tracks: Vec<TrackItem>,
    /// 音轨菜单是否展开。
    pub audio_menu_open: bool,
    /// 字幕菜单是否展开。
    pub subtitle_menu_open: bool,
    /// 错误原因（可读文案，不是引擎原始错误）。
    pub error: Option<String>,
    /// 错误卡上的动作文案，例如 `"重试"`。
    pub error_action: Option<String>,
    /// 硬件解码未生效的提示，例如 `"硬件解码未生效：正在用 CPU 软解"`。
    pub hwdec_notice: Option<String>,
    /// 诊断用一行（引擎 / 直连或转码 / 实际 hwdec），可关。
    pub engine_line: Option<String>,
    /// 诊断行“展开”位。播放画面上**不渲染**（官方播放器没有任何常驻诊断文字），
    /// 只作为数据给 `--debug-socket` 的 `view` 用。
    pub show_engine_detail: bool,
    /// 时间行的渲染模式（点击时间文本切换）。
    pub time_mode: TimeMode,
    /// 控制层当前是否可见。
    pub controls_visible: bool,
    /// 距上一次输入过去了多久（由 [`PlayerView::tick`] 推进）。
    idle: Duration,
    /// 音量数字还要显示多久（由 [`PlayerView::tick`] 递减）。
    volume_hint: Duration,
}

impl PlayerView {
    /// 新建播放页。默认停在"未播放、无时长"的状态，控制层可见。
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            volume: 1.0,
            rate: 1.0,
            controls_visible: true,
            ..Self::default()
        }
    }

    /// 设置副标题。
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// 设置当前播放位置。
    pub fn with_position(mut self, position: Duration) -> Self {
        self.position = position;
        self
    }

    /// 设置总时长。
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// 设置已缓冲位置。
    pub fn with_buffered(mut self, buffered: Duration) -> Self {
        self.buffered = Some(buffered);
        self
    }

    /// 设置播放 / 暂停。
    pub fn with_playing(mut self, playing: bool) -> Self {
        self.playing = playing;
        self
    }

    /// 设置缓冲态。
    pub fn with_buffering(mut self, buffering: bool) -> Self {
        self.buffering = buffering;
        self
    }

    /// 设置音量（自动夹到 `0.0..=1.0`）。
    pub fn with_volume(mut self, volume: f32) -> Self {
        self.volume = clamp_unit(volume);
        self
    }

    /// 设置静音。
    pub fn with_muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    /// 设置播放速率（非正值会被忽略，保持原值）。
    pub fn with_rate(mut self, rate: f32) -> Self {
        if rate.is_finite() && rate > 0.0 {
            self.rate = rate;
        }
        self
    }

    /// 设置音轨列表。
    pub fn with_audio_tracks(mut self, tracks: Vec<TrackItem>) -> Self {
        self.audio_tracks = tracks;
        self
    }

    /// 设置字幕列表。
    pub fn with_subtitle_tracks(mut self, tracks: Vec<TrackItem>) -> Self {
        self.subtitle_tracks = tracks;
        self
    }

    /// 设置错误卡内容（原因 + 动作文案）。
    pub fn with_error(mut self, reason: impl Into<String>, action: impl Into<String>) -> Self {
        self.error = Some(reason.into());
        self.error_action = Some(action.into());
        self
    }

    /// 设置"硬件解码未生效"提示。
    pub fn with_hwdec_notice(mut self, notice: impl Into<String>) -> Self {
        self.hwdec_notice = Some(notice.into());
        self
    }

    /// 设置诊断行。
    pub fn with_engine_line(mut self, line: impl Into<String>) -> Self {
        self.engine_line = Some(line.into());
        self
    }

    /// 设置完整诊断行是否展开（等价于按了一次 `Ctrl+D`）。
    pub fn with_engine_detail(mut self, show: bool) -> Self {
        self.show_engine_detail = show;
        self
    }

    /// 设置时间行模式。
    pub fn with_time_mode(mut self, mode: TimeMode) -> Self {
        self.time_mode = mode;
        self
    }

    /// 设置控制层可见性（用户操作后一般走 [`PlayerView::note_input`]）。
    pub fn with_controls_visible(mut self, visible: bool) -> Self {
        self.controls_visible = visible;
        self
    }

    // ---- 时间文本 ----

    /// 当前位置文本：`"12:34"` / `"1:02:03"`。
    pub fn position_text(&self) -> String {
        clock_text(self.position)
    }

    /// 总时长文本：未知时为 `None`。
    pub fn duration_text(&self) -> Option<String> {
        self.duration.map(clock_text)
    }

    /// `"12:34 / 2:18:00"`；时长未知时只有位置。
    pub fn time_line(&self) -> String {
        match self.duration_text() {
            Some(total) => format!("{} / {total}", self.position_text()),
            None => self.position_text(),
        }
    }

    /// 剩余时长（Infuse 风格的 `-2:05:26` 数据源）；时长未知或已过末尾时返回 `None`。
    pub fn remaining(&self) -> Option<Duration> {
        let total = self.duration?;
        if self.position >= total {
            return None;
        }
        Some(total - self.position)
    }

    /// 剩余时长文本：`"-2:05:26"`。
    pub fn remaining_text(&self) -> Option<String> {
        self.remaining()
            .map(|left| format!("-{}", clock_text(left)))
    }

    /// 实际渲染的时间行：`time_mode` 决定是"已播/总长"还是"剩余"。
    ///
    /// 剩余时长未知（没有时长或已到末尾）时退回已播/总长，不会渲染成空文本。
    pub fn time_text(&self) -> String {
        match self.time_mode {
            TimeMode::Elapsed => self.time_line(),
            TimeMode::Remaining => self.remaining_text().unwrap_or_else(|| self.time_line()),
        }
    }

    /// 点击时间文本：在"已播/总长"与"剩余"之间切换。
    pub fn toggle_time_mode(&mut self) {
        self.time_mode = match self.time_mode {
            TimeMode::Elapsed => TimeMode::Remaining,
            TimeMode::Remaining => TimeMode::Elapsed,
        };
        self.note_input();
    }

    // ---- 进度 ----

    /// 已播比例 `0.0..=1.0`；时长未知或为 0 时返回 `None`（调用方不要拿到 `NaN`）。
    pub fn progress_fraction(&self) -> Option<f32> {
        fraction(self.position, self.duration?)
    }

    /// 已缓冲比例，**永远不小于** [`PlayerView::progress_fraction`]，且不会超过 1.0。
    pub fn buffered_fraction(&self) -> Option<f32> {
        let progress = self.progress_fraction()?;
        let buffered = match self.buffered {
            Some(buffered) => fraction(buffered, self.duration.unwrap_or_default()).unwrap_or(0.0),
            None => 0.0,
        };
        Some(buffered.max(progress).clamp(0.0, 1.0))
    }

    /// 把拖动进度条得到的比例换算成目标位置；时长未知时返回 `None`。
    pub fn position_at_ratio(&self, ratio: f32) -> Option<Duration> {
        let total = self.duration?;
        if total.is_zero() {
            return None;
        }
        let ratio = clamp_unit(ratio);
        Some(total.mul_f32(ratio))
    }

    /// 拖动进度条时气泡里的时间文本。
    ///
    /// 时长未知时退回当前位置文本，至少不会显示空白。
    pub fn scrub_text(&self, ratio: f32) -> String {
        match self.position_at_ratio(ratio) {
            Some(target) => clock_text(target),
            None => self.position_text(),
        }
    }

    /// 相对跳转的落点（`seconds` 为负表示后退），已夹到 `[0, 时长]`。
    pub fn shift_position(&self, seconds: i64) -> Duration {
        let current = self.position.as_secs_f64();
        let target = (current + seconds as f64).max(0.0);
        let target = Duration::from_secs_f64(target);
        match self.duration {
            Some(total) => target.min(total),
            None => target,
        }
    }

    // ---- 音量 / 速率 ----

    /// 实际音量比例（静音时为 0）。
    pub fn volume_fraction(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            clamp_unit(self.volume)
        }
    }

    /// 音量文本：`"60%"`；静音时为 `"静音"`。
    pub fn volume_text(&self) -> String {
        if self.muted {
            return "静音".to_string();
        }
        format!("{}%", (clamp_unit(self.volume) * 100.0).round() as i32)
    }

    /// 速率文本：`"1.0×"` / `"1.25×"`。
    pub fn rate_text(&self) -> String {
        format!("{}×", trim_float(self.rate))
    }

    /// 标记一次音量变化：数字文本再过 [`VOLUME_HINT_DURATION`] 才隐藏。
    pub fn note_volume_change(&mut self) {
        self.volume_hint = VOLUME_HINT_DURATION;
        self.note_input();
    }

    /// 音量数字当前是否该显示（只在刚调过时短时显示）。
    pub fn volume_hint_visible(&self) -> bool {
        !self.volume_hint.is_zero()
    }

    // ---- 菜单 ----

    /// 是否有菜单展开（展开时不自动隐藏控制层）。
    pub fn menus_open(&self) -> bool {
        self.audio_menu_open || self.subtitle_menu_open
    }

    /// 展开音轨菜单（自动收起字幕菜单）。
    pub fn open_audio_menu(&mut self) {
        self.audio_menu_open = true;
        self.subtitle_menu_open = false;
        self.note_input();
    }

    /// 展开字幕菜单（自动收起音轨菜单）。
    pub fn open_subtitle_menu(&mut self) {
        self.subtitle_menu_open = true;
        self.audio_menu_open = false;
        self.note_input();
    }

    /// 收起全部菜单。
    pub fn close_menus(&mut self) {
        self.audio_menu_open = false;
        self.subtitle_menu_open = false;
    }

    /// 切换音轨菜单（互斥打开）。
    pub fn toggle_audio_menu(&mut self) {
        if self.audio_menu_open {
            self.close_menus();
        } else {
            self.open_audio_menu();
        }
    }

    /// 切换字幕菜单（互斥打开）。
    pub fn toggle_subtitle_menu(&mut self) {
        if self.subtitle_menu_open {
            self.close_menus();
        } else {
            self.open_subtitle_menu();
        }
    }

    /// 当前选中的音轨。
    pub fn selected_audio_track(&self) -> Option<&TrackItem> {
        self.audio_tracks.iter().find(|track| track.selected)
    }

    /// 当前选中的字幕（`None` 表示字幕关闭）。
    pub fn selected_subtitle_track(&self) -> Option<&TrackItem> {
        self.subtitle_tracks.iter().find(|track| track.selected)
    }

    /// 选中一条音轨并收起菜单；轨道不存在时返回 `false` 且不改状态。
    pub fn select_audio_track(&mut self, index: u32) -> bool {
        if !select_in(&mut self.audio_tracks, index) {
            return false;
        }
        self.close_menus();
        true
    }

    /// 选中一条字幕，`None` 表示关闭字幕；轨道不存在时返回 `false` 且不改状态。
    pub fn select_subtitle_track(&mut self, index: Option<u32>) -> bool {
        match index {
            Some(index) => {
                if !select_in(&mut self.subtitle_tracks, index) {
                    return false;
                }
            }
            None => {
                for track in &mut self.subtitle_tracks {
                    track.selected = false;
                }
            }
        }
        self.close_menus();
        true
    }

    // ---- 控制层自动隐藏 ----

    /// 标记一次用户输入：立刻显示控制层并把计时归零。
    ///
    /// 任何键、鼠标移动、点击都该调它。
    pub fn note_input(&mut self) {
        self.idle = Duration::ZERO;
        self.controls_visible = true;
    }

    /// 是否允许自动隐藏：暂停、缓冲、菜单展开、错误卡显示时都不隐藏。
    pub fn can_auto_hide(&self) -> bool {
        self.playing && !self.buffering && self.error.is_none() && !self.menus_open()
    }

    /// 推进虚拟时间（增量）。
    ///
    /// 返回控制层可见性是否发生变化——上层据此决定要不要重刷界面，
    /// 这样 3 秒静默期内不会有任何无谓的属性写入。
    pub fn tick(&mut self, delta: Duration) -> bool {
        // 音量提示也会在静默期内到点消失，先退它。
        let mut changed = self.decay_volume_hint(delta);
        if !self.can_auto_hide() {
            self.idle = Duration::ZERO;
            changed |= self.show_controls();
            return changed;
        }
        self.idle = self.idle.saturating_add(delta);
        if self.idle >= AUTO_HIDE_DELAY && self.controls_visible {
            self.controls_visible = false;
            changed = true;
        }
        changed
    }

    /// 强制显示 / 隐藏控制层。
    pub fn set_controls_visible(&mut self, visible: bool) -> bool {
        let changed = self.controls_visible != visible;
        self.controls_visible = visible;
        if visible {
            self.idle = Duration::ZERO;
        }
        changed
    }

    /// 点击画面：显示的控制层收起，收起时显示。
    pub fn toggle_controls(&mut self) {
        let visible = !self.controls_visible;
        self.set_controls_visible(visible);
    }

    // ---- 状态提示 ----

    /// 控制层底部状态行。
    ///
    /// 优先级：`hwdec_notice` > `engine_line`。硬件解码没生效是用户可行动的偏差，
    /// 必须压过诊断行；诊断行仍然留在数据里（诊断面板可以单独读它）。
    pub fn status_line(&self) -> Option<&str> {
        self.hwdec_notice.as_deref().or(self.engine_line.as_deref())
    }

    /// 诊断行是否还可以显示（被 `hwdec_notice` 顶掉时为 `false`）。
    pub fn engine_line_visible(&self) -> bool {
        self.hwdec_notice.is_none() && self.engine_line.is_some()
    }

    /// 常驻的短标签：诊断行的第一段（`"直连 · hwdec=vaapi · 1080p60"` → `"直连"`）。
    ///
    /// 没有诊断行时返回 `None`。
    pub fn transport_mode(&self) -> Option<&str> {
        self.engine_line
            .as_deref()
            .and_then(|line| line.split('·').next())
            .map(str::trim)
            .filter(|head| !head.is_empty())
    }

    /// 完整诊断行是否该渲染（需要 `show_engine_detail` 打开且确实有诊断行）。
    pub fn engine_detail_text(&self) -> Option<&str> {
        if self.show_engine_detail {
            self.engine_line.as_deref()
        } else {
            None
        }
    }

    /// 切换完整诊断行（`Ctrl+D`）。
    pub fn toggle_engine_detail(&mut self) {
        self.show_engine_detail = !self.show_engine_detail;
        self.note_input();
    }

    /// 控制层可见时字幕要抬多高（mpv 的 `sub-margin-y`，单位 px）。
    ///
    /// `blend-subtitles=video` 时字幕烧在视频层，控制层的传输行会直接盖住底部常驻区，
    /// 所以控制层可见时把字幕抬到传输行上方，隐藏时归零。
    pub fn subtitle_margin(&self, viewport_height: f32) -> i64 {
        if !self.controls_visible || !viewport_height.is_finite() || viewport_height <= 0.0 {
            return 0;
        }
        let lift = (viewport_height * SUBTITLE_SAFE_AREA_RATIO).round();
        (lift as i64).clamp(SUBTITLE_MARGIN_MIN, SUBTITLE_MARGIN_MAX)
    }

    fn decay_volume_hint(&mut self, delta: Duration) -> bool {
        if self.volume_hint.is_zero() {
            return false;
        }
        self.volume_hint = self.volume_hint.saturating_sub(delta);
        true
    }

    fn show_controls(&mut self) -> bool {
        if self.controls_visible {
            return false;
        }
        self.controls_visible = true;
        true
    }
}

/// `hh:mm:ss` / `mm:ss`。超过 1 小时才带小时位。
pub fn clock_text(total: Duration) -> String {
    let secs = total.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn fraction(part: Duration, total: Duration) -> Option<f32> {
    if total.is_zero() {
        return None;
    }
    Some((part.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0))
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_nan() {
        return 0.0;
    }
    value.clamp(0.0, 1.0)
}

fn trim_float(value: f32) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".to_string()
    } else {
        text.to_string()
    }
}

fn select_in(tracks: &mut [TrackItem], index: u32) -> bool {
    if !tracks.iter().any(|track| track.index == index) {
        return false;
    }
    for track in tracks {
        track.selected = track.index == index;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PlayerView {
        PlayerView::new("沙丘：第二部")
            .with_subtitle("2024 · 科幻")
            .with_duration(Duration::from_secs(2 * 3600 + 18 * 60))
            .with_position(Duration::from_secs(12 * 60 + 34))
            .with_buffered(Duration::from_secs(25 * 60))
            .with_playing(true)
    }

    #[test]
    fn clock_text_switches_to_hours_only_when_needed() {
        assert_eq!(clock_text(Duration::from_secs(0)), "0:00");
        assert_eq!(clock_text(Duration::from_secs(754)), "12:34");
        assert_eq!(clock_text(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(clock_text(Duration::from_secs(8280)), "2:18:00");
    }

    #[test]
    fn position_and_duration_text_compose_time_line() {
        let view = sample();
        assert_eq!(view.position_text(), "12:34");
        assert_eq!(view.duration_text().as_deref(), Some("2:18:00"));
        assert_eq!(view.time_line(), "12:34 / 2:18:00");
    }

    #[test]
    fn unknown_duration_degrades_without_nan() {
        let view = PlayerView::new("x").with_position(Duration::from_secs(30));
        assert_eq!(view.duration_text(), None);
        assert_eq!(view.time_line(), "0:30");
        assert_eq!(view.progress_fraction(), None);
        assert_eq!(view.buffered_fraction(), None);
        assert_eq!(view.remaining(), None);
        assert_eq!(view.remaining_text(), None);
        assert_eq!(view.scrub_text(0.5), "0:30");
    }

    #[test]
    fn zero_duration_is_not_a_progress() {
        let view = PlayerView::new("x")
            .with_duration(Duration::ZERO)
            .with_position(Duration::from_secs(10));
        assert_eq!(view.progress_fraction(), None);
        assert_eq!(view.buffered_fraction(), None);
        assert_eq!(view.position_at_ratio(0.5), None);
    }

    #[test]
    fn progress_and_buffered_fractions_are_clamped() {
        let view = sample();
        assert!((view.progress_fraction().unwrap() - 0.0913).abs() < 0.001);
        // buffered 25 分钟 > position 12:34，缓冲段永远盖住已播段。
        let buffered = view.buffered_fraction().unwrap();
        assert!(buffered >= view.progress_fraction().unwrap());

        // 已播超过"已缓冲"时，缓冲比例被拉到已播之后，而不是比它小。
        let behind = sample().with_buffered(Duration::from_secs(60));
        assert!(behind.buffered_fraction().unwrap() >= behind.progress_fraction().unwrap());

        // 超过总时长也不会溢出。
        let overflow = sample().with_buffered(Duration::from_secs(99 * 3600));
        assert!((overflow.buffered_fraction().unwrap() - 1.0).abs() < f32::EPSILON);

        // 没有缓冲信息时退化为"已播到哪就缓冲到哪"。
        let unknown = sample();
        let mut unknown = unknown;
        unknown.buffered = None;
        assert_eq!(
            unknown.buffered_fraction(),
            unknown.progress_fraction(),
            "无缓冲信息时缓冲比例等于已播比例"
        );
    }

    #[test]
    fn remaining_text_is_infuse_style_negative() {
        let view = sample();
        assert_eq!(view.remaining_text().as_deref(), Some("-2:05:26"));
        let almost = view.with_position(Duration::from_secs(8280));
        assert_eq!(almost.remaining_text(), None);
    }

    #[test]
    fn track_menus_are_mutually_exclusive() {
        let mut view = sample();
        view.open_audio_menu();
        assert!(view.audio_menu_open && !view.subtitle_menu_open);
        view.open_subtitle_menu();
        assert!(view.subtitle_menu_open && !view.audio_menu_open);
        view.toggle_subtitle_menu();
        assert!(!view.menus_open());
        view.toggle_audio_menu();
        assert!(view.audio_menu_open);
        view.close_menus();
        assert!(!view.menus_open());
    }

    #[test]
    fn track_selection_updates_flags_and_closes_menu() {
        let mut view = sample()
            .with_audio_tracks(vec![
                TrackItem::new(1, "国语 · AC3 5.1").with_selected(true),
                TrackItem::new(2, "英语 · AAC 2.0"),
            ])
            .with_subtitle_tracks(vec![
                TrackItem::new(3, "简体中文 (SRT)").with_selected(true),
                TrackItem::new(4, "English (PGS)").with_bitmap_subtitle(),
            ]);
        view.open_audio_menu();
        assert!(view.select_audio_track(2));
        assert!(!view.menus_open());
        assert_eq!(
            view.selected_audio_track().map(|track| track.index),
            Some(2)
        );
        assert!(!view.select_audio_track(9), "不存在的轨道不改状态");

        assert!(view.select_subtitle_track(Some(4)));
        let selected = view.selected_subtitle_track().expect("有选中字幕");
        assert_eq!(selected.index, 4);
        assert!(selected.is_bitmap_subtitle);

        assert!(view.select_subtitle_track(None), "None 表示关闭字幕");
        assert!(view.selected_subtitle_track().is_none());
    }

    #[test]
    fn hwdec_notice_wins_over_engine_line() {
        let both = sample()
            .with_engine_line("直连 · hwdec=vaapi")
            .with_hwdec_notice("硬件解码未生效：正在用 CPU 软解");
        assert_eq!(
            both.status_line(),
            Some("硬件解码未生效：正在用 CPU 软解"),
            "同一行里 hwdec 提示优先"
        );
        assert!(!both.engine_line_visible());

        let only_engine = sample().with_engine_line("转码 · hwdec=vaapi");
        assert_eq!(only_engine.status_line(), Some("转码 · hwdec=vaapi"));
        assert!(only_engine.engine_line_visible());

        assert_eq!(PlayerView::new("x").status_line(), None);
    }

    #[test]
    fn auto_hide_after_three_seconds_of_silence() {
        let mut view = sample();
        assert!(view.controls_visible);
        assert!(!view.tick(Duration::from_millis(2800)));
        assert!(view.controls_visible, "3 秒内不该隐藏");
        assert!(view.tick(Duration::from_millis(300)), "跨过 3 秒要隐藏");
        assert!(!view.controls_visible);

        view.note_input();
        assert!(view.controls_visible, "任何输入立刻显示");
        assert!(!view.tick(Duration::from_secs(1)));
    }

    #[test]
    fn auto_hide_is_blocked_while_buffering_paused_or_menu_open() {
        let paused = sample().with_playing(false);
        let mut paused = paused;
        assert!(!paused.can_auto_hide());
        paused.tick(Duration::from_secs(10));
        assert!(paused.controls_visible, "暂停时控件常显");

        let mut buffering = sample().with_buffering(true);
        buffering.tick(Duration::from_secs(10));
        assert!(buffering.controls_visible, "缓冲中控件不自动隐藏");

        let mut menu = sample();
        menu.open_subtitle_menu();
        menu.tick(Duration::from_secs(10));
        assert!(menu.controls_visible, "菜单展开时控件不自动隐藏");

        let mut failed = sample().with_error("服务器不可达", "重试");
        failed.tick(Duration::from_secs(10));
        assert!(failed.controls_visible, "错误卡显示时控件不自动隐藏");
    }

    #[test]
    fn toggle_controls_from_click() {
        let mut view = sample();
        view.toggle_controls();
        assert!(!view.controls_visible);
        view.toggle_controls();
        assert!(view.controls_visible);
    }

    #[test]
    fn volume_and_rate_text() {
        let view = sample().with_volume(0.6);
        assert_eq!(view.volume_text(), "60%");
        assert!((view.volume_fraction() - 0.6).abs() < f32::EPSILON);
        assert_eq!(view.clone().with_muted(true).volume_text(), "静音");
        assert_eq!(view.clone().with_muted(true).volume_fraction(), 0.0);
        assert_eq!(view.clone().with_volume(3.0).volume, 1.0);
        assert_eq!(view.with_volume(f32::NAN).volume, 0.0);
        assert_eq!(sample().rate_text(), "1×");
        assert_eq!(sample().with_rate(1.25).rate_text(), "1.25×");
        assert_eq!(sample().with_rate(0.0).rate, 1.0, "非法速率被忽略");
    }

    #[test]
    fn shift_position_clamps_to_bounds() {
        let view = sample();
        assert_eq!(view.shift_position(15), Duration::from_secs(769));
        assert_eq!(view.shift_position(-15), Duration::from_secs(739));
        assert_eq!(view.shift_position(-99_999), Duration::ZERO);
        assert_eq!(view.shift_position(99_999), Duration::from_secs(8280));

        // 时长未知时只保证不越过 0。
        let unknown = PlayerView::new("x").with_position(Duration::from_secs(5));
        assert_eq!(unknown.shift_position(-60), Duration::ZERO);
        assert_eq!(unknown.shift_position(60), Duration::from_secs(65));
    }

    #[test]
    fn scrub_text_follows_ratio() {
        let view = sample();
        assert_eq!(view.scrub_text(0.0), "0:00");
        assert_eq!(view.scrub_text(0.5), "1:09:00");
        assert_eq!(view.scrub_text(1.0), "2:18:00");
        assert_eq!(view.position_at_ratio(0.5), Some(Duration::from_secs(4140)));
    }

    #[test]
    fn time_mode_renders_only_one_of_the_two() {
        let mut view = sample();
        // 官方播放器的时间位就是剩余时长（`.time-remaining__time`），所以默认是 Remaining。
        assert_eq!(view.time_mode, TimeMode::Remaining);
        assert_eq!(view.time_text(), "-2:05:26", "剩余模式下不叠已播/总长");
        view.toggle_time_mode();
        assert_eq!(view.time_mode, TimeMode::Elapsed);
        assert_eq!(view.time_text(), "12:34 / 2:18:00");
        view.toggle_time_mode();
        assert_eq!(view.time_text(), "-2:05:26");

        let explicit = sample().with_time_mode(TimeMode::Elapsed);
        assert_eq!(explicit.time_text(), "12:34 / 2:18:00");

        // 剩余时长未知（没有时长）时退回已播文本，不渲染空白。
        let unknown = PlayerView::new("x")
            .with_position(Duration::from_secs(30))
            .with_time_mode(TimeMode::Remaining);
        assert_eq!(unknown.time_text(), "0:30");
    }

    #[test]
    fn transport_mode_is_the_first_segment_of_the_engine_line() {
        let view = sample().with_engine_line("直连 · hwdec=vaapi · 1080p60");
        assert_eq!(view.transport_mode(), Some("直连"));
        assert_eq!(view.engine_detail_text(), None, "完整诊断行默认收起");
        assert_eq!(
            view.clone().with_engine_detail(true).engine_detail_text(),
            Some("直连 · hwdec=vaapi · 1080p60")
        );
        assert_eq!(sample().transport_mode(), None);
        assert_eq!(sample().with_engine_line("   ").transport_mode(), None);
    }

    #[test]
    fn engine_detail_toggles_with_ctrl_d() {
        let mut view = sample().with_engine_line("直连 · hwdec=vaapi");
        assert!(!view.show_engine_detail);
        view.toggle_engine_detail();
        assert!(view.show_engine_detail);
        assert_eq!(view.engine_detail_text(), Some("直连 · hwdec=vaapi"));
        view.toggle_engine_detail();
        assert!(!view.show_engine_detail);
    }

    #[test]
    fn volume_hint_hides_after_the_short_delay() {
        let mut view = sample();
        assert!(!view.volume_hint_visible(), "没调过就不显示数字");
        view.note_volume_change();
        assert!(view.volume_hint_visible());
        // 不到点：仍然可见，但不算"界面变了"以外的额外变化。
        assert!(view.tick(Duration::from_millis(600)));
        assert!(view.volume_hint_visible());
        assert!(view.tick(Duration::from_millis(700)));
        assert!(!view.volume_hint_visible(), "短时显示后就收起");
    }

    #[test]
    fn subtitle_margin_lifts_only_while_controls_are_visible() {
        let view = sample();
        assert_eq!(view.subtitle_margin(800.0), 280, "800px 窗口抬 280px");
        assert_eq!(
            view.clone()
                .with_controls_visible(false)
                .subtitle_margin(800.0),
            0,
            "控制层隐藏时字幕归位"
        );
        assert_eq!(view.subtitle_margin(200.0), 160, "小窗口有下限");
        assert_eq!(view.subtitle_margin(4000.0), 400, "大窗口有上限");
        assert_eq!(view.subtitle_margin(0.0), 0);
        assert_eq!(view.subtitle_margin(f32::NAN), 0);
    }
}
