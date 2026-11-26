// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! mpv worker 线程：唯一调用 `mpv_wait_event` 的地方。
//!
//! 职责：
//!
//! - 串行执行控制面命令（open / play / pause / seek / 选轨 …）；
//! - 把 mpv 事件翻译成 [`EngineEvent`] 并投递到事件流；
//! - 观察 `time-pos` / `pause` / `core-idle` / `cache-buffering-state` / `track-list` /
//!   `eof-reached`，据此产生位置、缓冲、轨道、结束事件；
//! - **断言硬解**（ADR-0001 坑 1）：`hwdec-current` 与请求值不一致就 `tracing::warn!`
//!   并把实际值写进诊断，绝不静默降级；
//! - 定时采样 `frame-drop-count` → `DroppedFrames`。
//!
//! 渲染不在这里：`mpv_render_context_*` 必须在持有 GL 上下文的线程上调用
//! （见 [`crate::engine::MpvEngine::render`]）。这里只用 `mpv_handle`。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nova_playback::{EngineDiagnostics, EngineEvent, PlaybackError, redact_secrets};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

use crate::events::{BufferSignals, ObservedProperty, event_for, is_buffering};
use crate::ffi::{EndFileReason, Handle, MpvEvent, PropValue, RawTrack, TrackKind, raw};

/// 属性观察的 id（会作为事件的 `reply_userdata` 回来）。
const OBS_TIME_POS: u64 = 1;
const OBS_PAUSE: u64 = 2;
const OBS_CORE_IDLE: u64 = 3;
const OBS_CACHE: u64 = 4;
const OBS_TRACK_LIST: u64 = 5;
const OBS_EOF: u64 = 6;

/// `mpv_wait_event` 的超时：既能让事件及时到达，又能在没事件时轮询命令通道。
const EVENT_WAIT_SECS: f64 = 0.02;
/// 轮询属性（丢帧、视频尺寸、hwdec）的周期。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 日志尾部保留多少行（用来判 4xx / 解码失败）。
const LOG_TAIL_LIMIT: usize = 60;

/// 引擎 → worker 的控制面命令。
#[derive(Debug)]
pub(crate) enum Command {
    Open {
        url: String,
        headers: Vec<(String, String)>,
        start: Duration,
        audio: Option<u32>,
        subtitle: SubtitleChoice,
    },
    Play,
    Pause,
    Seek(Duration),
    SetVolume(f32),
    SetMuted(bool),
    SelectAudio(u32),
    SelectSubtitle(SubtitleChoice),
    /// 字幕延迟（秒，**可负**：正数表示字幕更晚出现）。
    SetSubtitleDelay(f64),
    /// 字幕上抬量（屏幕高度的百分比，0 = 原位）：控制层可见时把字幕抬到它上方。
    ///
    /// 百分比而不是像素：mpv 能用得上的那个属性（`sub-pos`）就是百分比，
    /// 而像素值需要屏幕高度才能换算——那个信息在界面侧，不在引擎里。
    SetSubtitleMargin(i64),
    SetRate(f32),
    Quit,
}

/// 归一化后的字幕选择（外挂字幕在这一步已经落盘成路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubtitleChoice {
    Off,
    Track(u32),
    External(PathBuf),
}

/// 引擎与 worker、渲染线程共享的状态（只有它需要加锁）。
#[derive(Debug)]
pub(crate) struct SharedState {
    /// 当前诊断快照。
    pub diagnostics: EngineDiagnostics,
    /// 请求的硬解模式（原始字符串，用于比对 `hwdec-current`）。
    pub requested_hwdec: String,
    pub paused: bool,
    pub core_idle: bool,
    pub cache_percent: Option<f64>,
    pub eof: bool,
    pub has_video: bool,
    pub video_size: Option<(u32, u32)>,
    pub dropped_frames: u64,
    pub buffering: bool,
    /// 最近一次 `render()` 的时间（watchdog 用）。
    pub last_render: Instant,
    /// 首帧时间（真的把一帧渲染进我们的纹理的时刻）。
    pub first_frame_at: Option<Instant>,
    /// 本次打开的时刻（首帧耗时的基线）。
    pub opened_at: Instant,
    /// 起播阶段的事件顺序（实测记录，供报告与诊断用）。
    pub trace: Vec<(&'static str, Duration)>,
    /// 硬解降级是否已经警告过（只警告一次，避免刷屏）。
    pub hwdec_warned: bool,
    /// GL 实现名（`glGetString(GL_RENDERER)`），由 `attach_gl` 填入。
    pub renderer: String,
}

impl SharedState {
    pub(crate) fn new(requested_hwdec: String) -> Self {
        let now = Instant::now();
        Self {
            diagnostics: EngineDiagnostics {
                requested_hwdec: Some(requested_hwdec.clone()),
                ..EngineDiagnostics::default()
            },
            requested_hwdec,
            paused: false,
            core_idle: true,
            cache_percent: None,
            eof: false,
            has_video: false,
            video_size: None,
            dropped_frames: 0,
            buffering: false,
            last_render: now,
            first_frame_at: None,
            opened_at: now,
            trace: Vec::new(),
            hwdec_warned: false,
            renderer: String::new(),
        }
    }

    /// 记一笔起播阶段的时序（相对于本次打开）。
    pub(crate) fn trace(&mut self, label: &'static str) {
        let at = self.opened_at.elapsed();
        self.trace.push((label, at));
    }

    /// 重新开始一次打开（清掉上一次的时序与缓冲状态）。
    pub(crate) fn begin_open(&mut self) {
        self.paused = false;
        self.core_idle = true;
        self.cache_percent = None;
        self.eof = false;
        self.has_video = false;
        self.video_size = None;
        self.dropped_frames = 0;
        self.buffering = false;
        self.first_frame_at = None;
        self.opened_at = Instant::now();
        self.last_render = self.opened_at;
        self.trace.clear();
        self.hwdec_warned = false;
    }
}

/// 事件投递端：把 `EngineEvent` 塞进事件流（tokio 的 unbounded channel 自己负责唤醒）。
#[derive(Clone)]
pub(crate) struct EventSink {
    tx: UnboundedSender<EngineEvent>,
}

impl EventSink {
    pub(crate) fn new(tx: UnboundedSender<EngineEvent>) -> Self {
        Self { tx }
    }

    pub(crate) fn emit(&self, event: EngineEvent) {
        // 接收端被丢弃（引擎已关闭）只说明没人关心事件了，不是错误。
        let _ = self.tx.send(event);
    }
}

/// 设置属性观察（`mpv_initialize` 之后、任何文件加载之前调用）。
pub(crate) fn observe_properties(handle: &Handle) -> Result<(), String> {
    handle.observe(OBS_TIME_POS, "time-pos", raw::mpv_format_MPV_FORMAT_DOUBLE)?;
    handle.observe(OBS_PAUSE, "pause", raw::mpv_format_MPV_FORMAT_FLAG)?;
    handle.observe(OBS_CORE_IDLE, "core-idle", raw::mpv_format_MPV_FORMAT_FLAG)?;
    handle.observe(
        OBS_CACHE,
        "cache-buffering-state",
        raw::mpv_format_MPV_FORMAT_DOUBLE,
    )?;
    handle.observe(
        OBS_TRACK_LIST,
        "track-list",
        raw::mpv_format_MPV_FORMAT_NODE,
    )?;
    handle.observe(OBS_EOF, "eof-reached", raw::mpv_format_MPV_FORMAT_FLAG)?;
    Ok(())
}

/// worker 线程的入口：一直跑到收到 [`Command::Quit`] 或命令通道断开。
pub(crate) struct Worker {
    handle: Arc<Handle>,
    events: EventSink,
    shared: Arc<Mutex<SharedState>>,
    /// 当前文件的媒体描述（用于错误分类：是不是网络地址）。
    is_network_url: bool,
    log_tail: VecDeque<String>,
    desired: Desired,
    last_tracks: Option<Vec<RawTrack>>,
    last_buffering: bool,
    last_dropped: u64,
    last_reported_dropped: u64,
    last_poll: Instant,
    loaded: bool,
    ended_reported: bool,
    volume: f32,
    muted: bool,
    rate: f32,
    subtitle_delay: f64,
    /// 字幕上抬量（屏幕高度的百分比，`0` = 原位）。
    subtitle_margin: i64,
}

/// 用户意图（重开 / 重新枚举轨道之后要重放一遍，否则刚切的轨会悄悄变回去）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Desired {
    audio: Option<u32>,
    subtitle: SubtitleChoice,
}

impl Desired {
    fn new(audio: Option<u32>, subtitle: SubtitleChoice) -> Self {
        Self { audio, subtitle }
    }
}

impl Worker {
    pub(crate) fn new(
        handle: Arc<Handle>,
        events: EventSink,
        shared: Arc<Mutex<SharedState>>,
        volume: f32,
    ) -> Self {
        Self {
            handle,
            events,
            shared,
            is_network_url: false,
            log_tail: VecDeque::new(),
            desired: Desired::new(None, SubtitleChoice::Off),
            last_tracks: None,
            last_buffering: false,
            last_dropped: 0,
            last_reported_dropped: 0,
            last_poll: Instant::now(),
            loaded: false,
            ended_reported: false,
            volume,
            muted: false,
            rate: 1.0,
            subtitle_delay: 0.0,
            subtitle_margin: 0,
        }
    }

    /// 主循环。
    pub(crate) fn run(mut self, commands: Receiver<Command>) {
        loop {
            // 先把命令处理完（用户操作的延迟优先于事件投递）。
            loop {
                match commands.try_recv() {
                    Ok(Command::Quit) => return,
                    Ok(command) => self.apply(command),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            }
            if let Some(event) = self.handle.wait_event(EVENT_WAIT_SECS) {
                self.on_event(event);
            }
            if self.last_poll.elapsed() >= POLL_INTERVAL {
                self.poll_properties();
                self.last_poll = Instant::now();
            }
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Quit => {}
            Command::Open {
                url,
                headers,
                start,
                audio,
                subtitle,
            } => self.open(&url, &headers, start, audio, subtitle),
            Command::Play => self.set_flag("pause", false),
            Command::Pause => self.set_flag("pause", true),
            Command::Seek(position) => {
                let seconds = position.as_secs_f64();
                self.command(&["seek", &format!("{seconds:.3}"), "absolute"]);
            }
            Command::SetVolume(volume) => {
                self.volume = volume.clamp(0.0, 1.0);
                self.set_property("volume", &format!("{:.3}", self.volume * 100.0));
            }
            Command::SetMuted(muted) => {
                self.muted = muted;
                self.set_flag("mute", muted);
            }
            Command::SelectAudio(index) => {
                self.desired.audio = Some(index);
                self.apply_audio();
            }
            Command::SelectSubtitle(choice) => {
                self.desired.subtitle = choice;
                self.apply_subtitle();
            }
            Command::SetSubtitleDelay(seconds) => {
                self.subtitle_delay = if seconds.is_finite() { seconds } else { 0.0 };
                self.apply_subtitle_delay();
            }
            Command::SetSubtitleMargin(y) => {
                self.subtitle_margin = y.clamp(0, SUBTITLE_LIFT_MAX_PERCENT);
                self.apply_subtitle_margin();
            }
            Command::SetRate(rate) => {
                self.rate = if rate.is_finite() { rate } else { 1.0 };
                self.set_property("speed", &format!("{:.3}", self.rate));
            }
        }
    }

    fn open(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        start: Duration,
        audio: Option<u32>,
        subtitle: SubtitleChoice,
    ) {
        self.is_network_url = url.starts_with("http://") || url.starts_with("https://");
        self.log_tail.clear();
        self.loaded = false;
        self.ended_reported = false;
        self.last_tracks = None;
        self.last_dropped = 0;
        self.last_reported_dropped = 0;
        self.desired = Desired::new(audio, subtitle);
        {
            let mut shared = self.lock_shared();
            shared.begin_open();
            shared.trace("open");
        }
        // 认证头走 http-header-fields：URL 里不带 api_key，令牌就不会出现在日志/错误里。
        if !headers.is_empty() {
            self.set_property("http-header-fields", &join_http_headers(headers));
        }
        // 续播位置用 `start`（file-local option，每次 load 之前设一次）。
        if start > Duration::ZERO {
            self.set_property("start", &format!("{:.3}", start.as_secs_f64()));
        } else {
            self.set_property("start", "none");
        }
        if let Err(message) = self.handle.command(&["loadfile", url, "replace"]) {
            warn!(
                target: "nova_playback_mpv",
                "loadfile 失败: {}",
                redact_secrets(&message)
            );
            self.events
                .emit(EngineEvent::Error(PlaybackError::engine(message)));
        }
        // `pause` 是 mpv 的 **core 级**属性：`loadfile` 不会把它复位。上一次会话退出时
        // `Player::exit` 设了 `pause=yes`（离开播放页就该停声），这里如果不清，新文件会
        // 以暂停状态加载——表现为第二次起播位置冻结在续播点、不出帧、15 秒后报卡死
        // （体检 R4-1；实测 `loadfile` 前后 `pause` 都是 yes、`core-idle` 是 yes）。
        // 打开即"开始播放"：紧跟 `loadfile` 下发（排在同一个命令队列里，新文件加载完
        // 之前就生效，同时避免把上一部片子意外解暂停而闪一下声音）。
        self.set_flag("pause", false);
    }

    fn on_event(&mut self, event: MpvEvent) {
        match event {
            MpvEvent::StartFile => self.trace_shared("start-file"),
            MpvEvent::FileLoaded => {
                self.loaded = true;
                self.trace_shared("file-loaded");
                self.apply_desired_state();
                self.poll_properties();
            }
            MpvEvent::VideoReconfig => self.trace_shared("video-reconfig"),
            MpvEvent::PlaybackRestart => {
                self.trace_shared("playback-restart");
                self.poll_properties();
            }
            MpvEvent::Idle => {}
            MpvEvent::EndFile { reason, error } => self.on_end_file(reason, error),
            MpvEvent::PropertyChange { id, value } => self.on_property(id, value),
            MpvEvent::Log(line) => {
                let text = format!("[{}] {}", line.level, line.text);
                if self.log_tail.len() >= LOG_TAIL_LIMIT {
                    self.log_tail.pop_front();
                }
                // 日志里可能有播放直链（含令牌）：进缓冲区前先脱敏。
                self.log_tail.push_back(redact_secrets(&text));
                // 原始日志只到 debug，且同样脱敏。
                debug!(
                    target: "nova_playback_mpv",
                    "{} {} {}",
                    line.prefix,
                    line.level,
                    redact_secrets(&line.text)
                );
            }
            MpvEvent::Other(id) => debug!(target: "nova_playback_mpv", "忽略 mpv 事件 {id}"),
        }
    }

    fn on_end_file(&mut self, reason: EndFileReason, error: i32) {
        match reason {
            EndFileReason::Eof => {
                if !self.ended_reported {
                    self.ended_reported = true;
                    self.events.emit(EngineEvent::Ended);
                }
            }
            EndFileReason::Error => {
                let log_tail: Vec<String> = self.log_tail.iter().cloned().collect();
                let error = crate::error::classify(error, self.is_network_url, &log_tail);
                warn!(
                    target: "nova_playback_mpv",
                    "播放失败，分类={:?}，mpv 日志尾部={:?}",
                    error,
                    log_tail.iter().rev().take(3).collect::<Vec<_>>()
                );
                self.events.emit(EngineEvent::Error(error));
            }
            // 用户自己换片 / 关闭：不产生 Ended，也不报错。
            EndFileReason::Stop | EndFileReason::Other => {}
        }
    }

    fn on_property(&mut self, id: u64, value: PropValue) {
        // 先把 mpv 的原始属性归一化成 `ObservedProperty`，再统一走 `events::event_for`
        // 映射：这样"喂一个构造好的事件体，断言发出的 EngineEvent"可以在没有 mpv 的
        // 单元测试里覆盖全部映射（见 events 模块的测试）。
        let observed = match (id, value) {
            (OBS_TIME_POS, PropValue::Double(seconds)) => ObservedProperty::TimePos(seconds),
            (OBS_PAUSE, PropValue::Flag(paused)) => ObservedProperty::Pause(paused),
            (OBS_CORE_IDLE, PropValue::Flag(idle)) => ObservedProperty::CoreIdle(idle),
            (OBS_CACHE, PropValue::Double(percent)) => ObservedProperty::Cache(Some(percent)),
            (OBS_CACHE, PropValue::Int(percent)) => ObservedProperty::Cache(Some(percent as f64)),
            (OBS_CACHE, _) => ObservedProperty::Cache(None),
            (OBS_EOF, PropValue::Flag(reached)) => ObservedProperty::Eof(reached),
            (OBS_TRACK_LIST, PropValue::Tracks(tracks)) => {
                self.publish_tracks(tracks);
                return;
            }
            _ => return,
        };
        // 观察到的属性先落到共享状态（缓冲判定与 watchdog 都读它）。
        {
            let mut shared = self.lock_shared();
            match &observed {
                ObservedProperty::Pause(paused) => shared.paused = *paused,
                ObservedProperty::CoreIdle(idle) => shared.core_idle = *idle,
                ObservedProperty::Cache(percent) => shared.cache_percent = *percent,
                ObservedProperty::Eof(reached) => shared.eof = *reached,
                _ => {}
            }
        }
        if matches!(
            observed,
            ObservedProperty::Pause(_) | ObservedProperty::CoreIdle(_) | ObservedProperty::Cache(_)
        ) {
            self.refresh_buffering();
        }
        // `time-pos` 只在本次文件 **file-loaded 之后**才算数。
        //
        // `loadfile replace` 从下发到新文件真正加载完成之间（WAN 上实测能到几秒），
        // mpv 里还是**上一部片子**：它的 `time-pos` 依旧会被观察者送进来，新文件刚接上时
        // 也可能先报一个 0。这些值一旦被当成"当前会话的位置"，退出时就会被提交到服务端
        // （体检 R5-1：从未播放的条目被写入上一部的读数、已有续播点被清成 0）。
        //
        // 实测（mpv 0.42，IPC 观察同一组属性）：`loadfile replace` 之后依次是
        // `end-file` → `time-pos=None` → `start-file` → `file-loaded` → `time-pos=<新起点>`；
        // 也就是说 `file-loaded` 之前还会有一拍旧文件的读数，滤掉它才是"本次打开的事件"。
        if matches!(observed, ObservedProperty::TimePos(_)) && !self.loaded {
            return;
        }
        if matches!(observed, ObservedProperty::Eof(true)) {
            // END_FILE 已经报过 Ended 时不重复。
            if self.ended_reported {
                return;
            }
            self.ended_reported = true;
        }
        if let Some(event) = event_for(observed) {
            self.events.emit(event);
        }
    }

    /// 定时采样：视频尺寸、硬解、解码器、丢帧、轨道兜底枚举。
    fn poll_properties(&mut self) {
        let width = self.handle.get_i64("video-params/w");
        let height = self.handle.get_i64("video-params/h");
        let size = match (width, height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w as u32, h as u32)),
            _ => None,
        };
        let active_hwdec = self.handle.get_string("hwdec-current");
        let video_codec = self.handle.get_string("video-codec");
        let dropped = self.handle.get_i64("frame-drop-count").unwrap_or(0).max(0) as u64;

        {
            let mut shared = self.lock_shared();
            if size.is_some() {
                shared.video_size = size;
                shared.has_video = true;
            }
            shared.diagnostics.active_hwdec = active_hwdec.clone();
            shared.diagnostics.video_decoder = video_codec;
            // 渲染后端串由 `attach_gl` 填入；没有 GL 时只报"自有 FBO"这一层。
            let renderer = shared.renderer.clone();
            shared.diagnostics.render_backend = Some(if renderer.is_empty() {
                "libmpv OpenGL render API（自有 FBO）".to_string()
            } else {
                format!("libmpv OpenGL render API（{renderer}）")
            });
            shared.diagnostics.hw_decode_active = active_hwdec
                .as_deref()
                .is_some_and(|mode| mode != "no" && !mode.is_empty());
            shared.dropped_frames = dropped;
        }
        self.last_dropped = dropped;
        if dropped != self.last_reported_dropped {
            self.last_reported_dropped = dropped;
            if let Some(event) = event_for(ObservedProperty::DroppedFrames(dropped)) {
                self.events.emit(event);
            }
        }

        // ADR-0001 坑 1：硬解静默退化必须被看见。
        self.assert_hwdec(active_hwdec.as_deref());

        // 轨道枚举的兜底：有些容器直到 VIDEO_RECONFIG 之后 `track-list` 才有内容。
        if self.loaded && self.last_tracks.is_none() {
            if let Some(tracks) = self.handle.track_list() {
                self.publish_tracks(tracks);
            }
        }
    }

    fn assert_hwdec(&mut self, active: Option<&str>) {
        let (requested, already_warned) = {
            let shared = self.lock_shared();
            (shared.requested_hwdec.clone(), shared.hwdec_warned)
        };
        if already_warned || requested == "no" {
            return;
        }
        // `None` = mpv 还没写入 hwdec-current（通常在首帧前后才可用）。
        // 这时**不能**判定降级，等下一次属性变化再看；否则每台机器都会先闪一条假警报。
        let Some(active) = active else {
            return;
        };
        if nova_playback::hwdec_is_acceptable(&requested, active) {
            return;
        }
        self.lock_shared().hwdec_warned = true;
        warn!(
            target: "nova_playback_mpv",
            "硬件解码未生效：请求 {requested}，实际 {active}（界面必须提示用户，不允许静默降级）",
        );
    }

    fn publish_tracks(&mut self, tracks: Vec<RawTrack>) {
        // mpv 在加载前会发一条空 track-list：那不是"轨道集变了"，别用它把界面清空。
        if tracks.is_empty() {
            return;
        }
        if self.last_tracks.as_ref() == Some(&tracks) {
            return;
        }
        let has_video = tracks.iter().any(|track| track.kind == TrackKind::Video);
        let size = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .and_then(|track| match (track.width, track.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
                _ => None,
            });
        {
            let mut shared = self.lock_shared();
            shared.has_video = has_video;
            if size.is_some() {
                shared.video_size = size;
            }
        }
        self.last_tracks = Some(tracks.clone());
        if let Some(event) = event_for(ObservedProperty::Tracks(tracks)) {
            self.events.emit(event);
        }
        self.refresh_buffering();
    }

    fn refresh_buffering(&mut self) {
        let (signals, previous) = {
            let shared = self.lock_shared();
            (
                BufferSignals {
                    paused: shared.paused,
                    core_idle: shared.core_idle,
                    cache_percent: shared.cache_percent,
                    eof: shared.eof,
                },
                shared.buffering,
            )
        };
        let buffering = is_buffering(signals);
        if buffering == previous {
            return;
        }
        self.lock_shared().buffering = buffering;
        if self.last_buffering != buffering {
            self.last_buffering = buffering;
            if let Some(event) = event_for(ObservedProperty::Buffering(buffering)) {
                self.events.emit(event);
            }
        }
    }

    fn apply_desired_state(&mut self) {
        self.apply_audio();
        self.apply_subtitle();
        self.apply_subtitle_delay();
        self.apply_subtitle_margin();
        self.set_property("volume", &format!("{:.3}", self.volume * 100.0));
        self.set_flag("mute", self.muted);
        self.set_property("speed", &format!("{:.3}", self.rate));
    }

    fn apply_audio(&mut self) {
        if let Some(index) = self.desired.audio {
            self.set_property("aid", &index.to_string());
        }
    }

    fn apply_subtitle(&mut self) {
        match self.desired.subtitle.clone() {
            SubtitleChoice::Off => self.set_property("sid", "no"),
            // 内嵌 / 服务端字幕直接换 sid，不重启播放。
            SubtitleChoice::Track(index) => self.set_property("sid", &index.to_string()),
            SubtitleChoice::External(path) => {
                self.remove_external_subtitles();
                let path = path.to_string_lossy().into_owned();
                if let Err(message) = self.handle.command(&["sub-add", &path, "select"]) {
                    warn!(
                        target: "nova_playback_mpv",
                        "加载外挂字幕失败: {}",
                        redact_secrets(&message)
                    );
                }
                if let Some(tracks) = self.handle.track_list() {
                    self.publish_tracks(tracks);
                }
            }
        }
    }

    /// 删掉已有的外挂字幕，避免反复切字幕时越积越多。
    fn remove_external_subtitles(&mut self) {
        if let Some(tracks) = self.handle.track_list() {
            let external: Vec<u32> = tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Subtitle && track.is_external)
                .map(|track| track.id)
                .collect();
            for id in external {
                self.command(&["sub-remove", &id.to_string()]);
            }
        }
    }

    fn apply_subtitle_delay(&mut self) {
        // sub-delay 单位是秒（浮点，可负）；正数 = 字幕更晚出现。
        self.set_property("sub-delay", &format!("{:.3}", self.subtitle_delay));
    }

    /// 字幕安全区：把字幕往上抬到控制层上方，避开传输行。
    ///
    /// 用的属性是 **`sub-pos`**（屏幕高度百分比，100 = 原位），不是 `sub-margin-y`：
    /// 真实渲染实测（mpv 0.42，1280x720 黑底 + 同一行字幕，量字幕像素行的位置）：
    ///
    /// | 选项 | ASS 字幕 | 纯文本（SRT） |
    /// |---|---|---|
    /// | 基线（底部 32px） | 32px | 38px |
    /// | `sub-margin-y=280` | **32px（不动）** | 286px |
    /// | `sub-margin-y-offset=280` | **32px（不动）** | **38px（不动）** |
    /// | `sub-ass-force-margins=yes` + `sub-margin-y` | **32px（不动）** | — |
    /// | `sub-pos=70` | 240px | 240px |
    /// | `sub-pos=85` | 136px | 136px |
    ///
    /// 手册对上了：`--sub-margin-y` 是"**unstyled text subtitles**（无样式文本字幕）"的
    /// 边距，ASS 由脚本自己摆位置；`--sub-margin-y-offset` 也只在文本路径生效；
    /// `--sub-ass-force-margins` 只管"黑边里"那一种情况。ASS 里唯一生效的是 `sub-pos`
    /// （实测只动底部对齐的事件：顶部 / 中间的对齐事件位置不变，不会把特效字幕弄乱）。
    ///
    /// `sub-pos` 对纯文本字幕同样生效（实测 SRT `sub-pos=85` 与 ASS 完全一致），
    /// 所以不需要再另外下发 `sub-margin-y`。
    fn apply_subtitle_margin(&mut self) {
        let pos = subtitle_pos(self.subtitle_margin).to_string();
        self.set_property(SUBTITLE_POS_PROPERTY, &pos);
    }

    fn set_flag(&mut self, name: &str, value: bool) {
        self.set_property(name, if value { "yes" } else { "no" });
    }

    fn set_property(&mut self, name: &str, value: &str) {
        if let Err(message) = self.handle.set_property_string(name, value) {
            debug!(
                target: "nova_playback_mpv",
                "设置 {name} 失败: {}",
                redact_secrets(&message)
            );
        }
    }

    fn command(&mut self, args: &[&str]) {
        if let Err(message) = self.handle.command(args) {
            debug!(
                target: "nova_playback_mpv",
                "命令 {} 失败: {}",
                args.first().copied().unwrap_or(""),
                redact_secrets(&message)
            );
        }
    }

    fn trace_shared(&self, label: &'static str) {
        self.lock_shared().trace(label);
    }

    fn lock_shared(&self) -> std::sync::MutexGuard<'_, SharedState> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 可以用 `sub-pos` 把字幕抬起来的上限（**画面**高度的百分比）。
///
/// 再高就会把字幕推到画面上半部，所以夹住。这个上限不能太小：控制块贴底、
/// 矮窗口（800×600）下它占了画面高度约 42%，固定 40% 会盖住标题行；
/// 60% 仍然让字幕留在画面下半部（40% 以下再也盖不到人物脸部的主体）。
const SUBTITLE_LIFT_MAX_PERCENT: i64 = 60;
/// 字幕位置对应的 mpv 属性（画面高度百分比，100 = 原位）。
const SUBTITLE_POS_PROPERTY: &str = "sub-pos";
/// mpv 的 `sub-pos` 原位值。
const SUBTITLE_POS_HOME: i64 = 100;

/// 字幕上抬量（**画面**高度的百分比，0 = 原位）→ mpv `sub-pos`。
///
/// 注意 `sub-pos` 越小字幕越高：0 = 画面顶部。
pub(crate) fn subtitle_pos(lift_percent: i64) -> i64 {
    SUBTITLE_POS_HOME - lift_percent.clamp(0, SUBTITLE_LIFT_MAX_PERCENT)
}

/// `http-header-fields` 的拼装。
/// **实测**（mpv 0.42）：逗号是列表分隔符，值里的逗号必须写成 `\,`，多个头之间用
/// **不含空格**的逗号分隔。否则 mpv 会把后续的头折成上一个头的续行（服务器收到
/// `X-A: a\r\n X-B: c` 这种一整个值），而 Jellyfin 的 Authorization 头恰好含逗号，
/// 写错就整个请求变 401。
pub(crate) fn join_http_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(name, value)| format!("{name}: {}", value.replace(',', "\\,")))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 控制层显隐 → 下发给 mpv 的字幕位置属性。
    ///
    /// 隐藏时必须回到原位（`sub-pos=100`），可见时必须真的抬高，且只能是 `sub-pos`——
    /// 真实渲染实测 `sub-margin-y` / `sub-margin-y-offset` / `sub-ass-force-margins`
    /// 对 ASS 字幕**完全无效**（本机 6489 就是 ASS，字幕因此一直被控制层盖住）。
    #[test]
    fn subtitle_lift_maps_to_sub_pos_and_returns_home() {
        assert_eq!(SUBTITLE_POS_PROPERTY, "sub-pos");
        // 控制层隐藏：回原位。
        assert_eq!(subtitle_pos(0), 100);
        // 控制层可见：画面高度的百分比（`nova-app` 已把像素换算成画面百分比）。
        assert_eq!(subtitle_pos(35), 65);
        // 矮窗口（800×600 + 16:9 letterbox）需要 42%，不能被 40% 截掉，
        // 否则字幕又差 2% × 450 ≈ 9px，正好压在标题行上（实测间距只剩
        // “安全间距 − 9px”）。
        assert_eq!(subtitle_pos(42), 58);
        // 异常值不越界：负数当 0，过大夹到上限。
        assert_eq!(subtitle_pos(-5), 100);
        assert_eq!(
            subtitle_pos(999),
            SUBTITLE_POS_HOME - SUBTITLE_LIFT_MAX_PERCENT
        );
        assert!(subtitle_pos(999) > 0, "不能把字幕推到画面上边缘以外");
    }

    #[test]
    fn headers_escape_commas_and_avoid_spaces() {
        let joined = join_http_headers(&[
            (
                "Authorization".to_string(),
                "MediaBrowser Client=\"Nova\", Device=\"pc\", Token=\"abc\"".to_string(),
            ),
            ("X-Other".to_string(), "1".to_string()),
        ]);
        assert_eq!(
            joined,
            "Authorization: MediaBrowser Client=\"Nova\"\\, Device=\"pc\"\\, Token=\"abc\",X-Other: 1"
        );
        // 值里的逗号必须转义，否则 mpv 会把后面的头折成续行。
        assert_eq!(joined.matches("\\,").count(), 2, "{joined}");
        assert_eq!(join_http_headers(&[]), "");
    }
}
