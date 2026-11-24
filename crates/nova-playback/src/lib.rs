// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Nova 的引擎无关播放核心。
//!
//! 这一层定义"播放器"这个接缝，**不链接任何解码器**（没有 libmpv、没有 FFI、没有 `unsafe`）：
//!
//! - [`PlaybackEngine`]：引擎侧要实现的 trait。libmpv / GStreamer / 伪造引擎各写各的实现。
//! - [`Session`]：纯逻辑状态机，把引擎事件翻译成 [`SessionState`]，并负责 400 ms 转圈阈值
//!   与 15 秒卡死判定。
//! - [`ProgressReporter`]：进度上报节奏、去重、失败退避重试与落盘补发。
//! - [`TrackSelector`]：音轨 / 字幕选择，"本次会话手动选择优先于用户偏好"。
//! - [`resume_position`] / [`resume_label`]：从详情页起播时的续播位置与按钮文案。
//! - [`PlaybackError`]：错误分类与可行动的中文提示。
//!
//! # 时间是可注入的
//!
//! 所有跟时间有关的判断（转圈阈值、卡死、上报间隔、重试退避）都走 [`Clock`]，
//! 因此测试用 [`ManualClock`] 推进时间，不需要 `sleep`。
//!
//! # 谁负责什么
//!
//! 这一层**不持有** `item_id` / `source_id`（那是 [`nova_core::PlaybackProgress`] 的字段），
//! 也不做 I/O：调用方把 [`Session::position`] 等状态打包成进度，交给 [`ProgressReporter`]。
//! 这样同一套状态机既能被界面用，也能被命令行用。
//!
//! # 引擎实现方的两个硬要求（来自 ADR-0001，实测踩到）
//!
//! 1. `hwdec=vaapi` 会**静默**退回软解。实现必须在 open 之后断言实际解码器等于请求值，
//!    并用 [`EngineDiagnostics`] 如实上报；[`EngineDiagnostics::hwdec_notice`] 会给出界面文案。
//! 2. 不能只靠合成器的 frame 回调驱动渲染（Wayland 锁屏时回调不触发），实现需要自己的
//!    帧 watchdog。本 crate 的 [`EngineEvent::FramePresented`] 就是给它汇报"这一帧真的上屏了"用的。

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod clock;
mod engine;
mod error;
mod fake;
mod progress;
mod resume;
mod session;
mod tracks;

pub use clock::{Clock, ManualClock, SystemClock};
pub use engine::{
    EngineDiagnostics, EngineEvent, OpenRequest, PlaybackEngine, hwdec_is_acceptable, redact_url,
};
pub use error::{PlaybackError, redact_secrets};
pub use fake::{EngineCommand, FakeEngine};
pub use progress::{
    FlushOutcome, ProgressReporter, ProgressSink, ProgressTrigger, ReportError, ReportOutcome,
};
pub use resume::{resume_label, resume_position, resume_position_for_item};
pub use session::{SPINNER_DELAY, STALL_TIMEOUT, Session, SessionState};
pub use tracks::{SubtitleSelection, TrackPlan, TrackSelector, TrackSet};
