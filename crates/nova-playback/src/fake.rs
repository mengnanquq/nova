// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 伪造引擎：不解码任何东西，只记录命令并让测试注入事件。
//!
//! 本 crate 的全部状态机 / 上报测试都基于它；引擎实现方（`nova-playback-mpv` 之类）也可以
//! 拿它当"上层接线是否正确"的对照。

use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use futures::channel::mpsc;

use crate::engine::{EngineDiagnostics, EngineEvent, OpenRequest, PlaybackEngine};
use crate::error::PlaybackError;
use crate::tracks::SubtitleSelection;

/// 引擎收到的一条命令（按顺序记录，便于断言）。
#[derive(Clone, Debug, PartialEq)]
pub enum EngineCommand {
    /// 打开。
    Open(OpenRequest),
    /// 播放。
    Play,
    /// 暂停。
    Pause,
    /// 跳转。
    Seek(Duration),
    /// 音量。
    SetVolume(f32),
    /// 静音。
    SetMuted(bool),
    /// 选音轨。
    SelectAudio(u32),
    /// 选字幕。
    SelectSubtitle(SubtitleSelection),
    /// 字幕延迟。
    SetSubtitleDelay(Duration),
    /// 播放速度。
    SetRate(f32),
    /// 关闭。
    Close,
}

/// 伪造引擎。
pub struct FakeEngine {
    commands: Vec<EngineCommand>,
    sender: mpsc::UnboundedSender<EngineEvent>,
    receiver: mpsc::UnboundedReceiver<EngineEvent>,
    next_error: Option<PlaybackError>,
    diagnostics: EngineDiagnostics,
}

impl FakeEngine {
    /// 新建。
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            commands: Vec::new(),
            sender,
            receiver,
            next_error: None,
            diagnostics: EngineDiagnostics::default(),
        }
    }

    /// 让**下一条**命令返回错误（模拟打不开、seek 失败等）。
    pub fn fail_next(&mut self, error: PlaybackError) {
        self.next_error = Some(error);
    }

    /// 注入一个引擎事件（可以在 `open` 之后随时调用）。
    pub fn emit(&self, event: EngineEvent) {
        // 接收端被丢弃只说明测试不再关心事件，不影响命令记录。
        let _ = self.sender.unbounded_send(event);
    }

    /// 已收到的全部命令。
    pub fn commands(&self) -> &[EngineCommand] {
        &self.commands
    }

    /// 取出并清空命令记录。
    pub fn take_commands(&mut self) -> Vec<EngineCommand> {
        std::mem::take(&mut self.commands)
    }

    /// 最近一次 open 的请求。
    pub fn last_open(&self) -> Option<&OpenRequest> {
        self.commands
            .iter()
            .rev()
            .find_map(|command| match command {
                EngineCommand::Open(request) => Some(request),
                _ => None,
            })
    }

    /// 设置诊断信息（模拟硬解生效 / 失效）。
    pub fn set_diagnostics(&mut self, diagnostics: EngineDiagnostics) {
        self.diagnostics = diagnostics;
    }

    fn record(&mut self, command: EngineCommand) -> Result<(), PlaybackError> {
        self.commands.push(command);
        match self.next_error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlaybackEngine for FakeEngine {
    async fn open(&mut self, request: OpenRequest) -> Result<(), PlaybackError> {
        self.record(EngineCommand::Open(request))
    }

    async fn play(&mut self) -> Result<(), PlaybackError> {
        self.record(EngineCommand::Play)
    }

    async fn pause(&mut self) -> Result<(), PlaybackError> {
        self.record(EngineCommand::Pause)
    }

    async fn seek(&mut self, position: Duration) -> Result<(), PlaybackError> {
        self.record(EngineCommand::Seek(position))
    }

    async fn set_volume(&mut self, volume: f32) -> Result<(), PlaybackError> {
        self.record(EngineCommand::SetVolume(volume))
    }

    async fn set_muted(&mut self, muted: bool) -> Result<(), PlaybackError> {
        self.record(EngineCommand::SetMuted(muted))
    }

    async fn select_audio(&mut self, index: u32) -> Result<(), PlaybackError> {
        self.record(EngineCommand::SelectAudio(index))
    }

    async fn select_subtitle(&mut self, selection: SubtitleSelection) -> Result<(), PlaybackError> {
        self.record(EngineCommand::SelectSubtitle(selection))
    }

    async fn set_subtitle_delay(&mut self, delay: Duration) -> Result<(), PlaybackError> {
        self.record(EngineCommand::SetSubtitleDelay(delay))
    }

    async fn set_rate(&mut self, rate: f32) -> Result<(), PlaybackError> {
        self.record(EngineCommand::SetRate(rate))
    }

    async fn close(&mut self) -> Result<(), PlaybackError> {
        self.record(EngineCommand::Close)
    }

    fn events(&mut self) -> &mut (dyn Stream<Item = EngineEvent> + Unpin + Send) {
        &mut self.receiver
    }

    fn diagnostics(&self) -> EngineDiagnostics {
        self.diagnostics.clone()
    }
}
