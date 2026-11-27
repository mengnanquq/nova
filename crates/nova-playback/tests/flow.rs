// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 端到端接线：只用公开 API，把 `Box<dyn PlaybackEngine>` + `Session` + `TrackSelector`
//! + `ProgressReporter` 串起来跑一遍。用 `FakeEngine`，不需要真实解码器。

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use nova_core::{
    AudioDetails, ItemId, Language, MediaStream, PlayMethod, PlaybackProgress, SourceId, UserData,
    UserPreferences,
};
use nova_playback::{
    EngineCommand, EngineDiagnostics, EngineEvent, FakeEngine, ManualClock, OpenRequest,
    PlaybackEngine, PlaybackError, ProgressReporter, ProgressSink, ProgressTrigger, ReportError,
    Session, SessionState, SubtitleSelection, TrackSelector, TrackSet, resume_label,
    resume_position,
};

fn engine_tracks() -> TrackSet {
    let audio = |index: u32, language: &str| {
        MediaStream::audio(
            index,
            "eac3",
            AudioDetails {
                channels: Some(6),
                sample_rate: Some(48_000),
                bitrate: None,
                profile: None,
            },
            Language::new(language),
        )
    };
    TrackSet::new(vec![
        audio(1, "eng"),
        audio(2, "zh"),
        MediaStream::subtitle(3, "subrip", Language::new("zh-Hans")),
    ])
}

struct CountingSink {
    seen: std::sync::Mutex<Vec<Duration>>,
}

#[async_trait::async_trait]
impl ProgressSink for CountingSink {
    async fn report(&self, progress: &PlaybackProgress) -> Result<(), ReportError> {
        match self.seen.lock() {
            Ok(mut seen) => seen.push(progress.position),
            Err(poisoned) => poisoned.into_inner().push(progress.position),
        }
        Ok(())
    }
}

#[tokio::test]
async fn full_playback_flow_through_public_api() {
    let prefs = UserPreferences::default();
    let clock = Arc::new(ManualClock::new());
    let mut engine = FakeEngine::new();
    let mut session = Session::new(clock.clone());
    let mut selector = TrackSelector::new();

    // 1. 详情页决定起播位置与按钮文案。
    let user_data = UserData {
        played: false,
        resume_position: Duration::from_secs(754),
        ..UserData::default()
    };
    let total = Duration::from_secs(7200);
    let resume = resume_position(&user_data, Some(total), &prefs);
    assert_eq!(resume, Duration::from_secs(754));
    assert_eq!(resume_label(resume), "从 12:34 继续");

    // 2. open。
    session.begin(resume, Some(total));
    assert_eq!(session.state(), SessionState::Opening);
    engine
        .open(OpenRequest::new("http://host:8096/v/1?api_key=SECRET").with_resume_at(resume))
        .await
        .unwrap_or_else(|error| panic!("open: {error}"));
    assert_eq!(session.state(), SessionState::Opening);

    // 3. 引擎枚举出真实轨道 → 回到 Session，并按偏好（无手动选择）落一遍。
    engine.emit(EngineEvent::TracksChanged(engine_tracks()));
    let event = engine
        .events()
        .next()
        .await
        .unwrap_or_else(|| panic!("应有轨道事件"));
    session.handle(event);
    assert_eq!(session.tracks().audio().count(), 2);

    let plan = selector.reapply(session.tracks(), &prefs);
    assert_eq!(plan.audio, Some(2), "偏好语言 zh 应选中 2 号音轨");
    assert_eq!(plan.subtitle, SubtitleSelection::Track(3));
    if let Some(index) = plan.audio {
        engine.select_audio(index).await.unwrap_or_default();
    }
    engine
        .select_subtitle(plan.subtitle)
        .await
        .unwrap_or_default();

    // 4. 用户手动切到英文音轨 + 关字幕：手动选择优先。
    selector.select_audio(1);
    selector.select_subtitle(SubtitleSelection::Off);
    let plan = selector.reapply(session.tracks(), &prefs);
    assert_eq!(plan.audio, Some(1));
    assert_eq!(plan.subtitle, SubtitleSelection::Off);

    // 5. 首帧 → 播放中；位置来自引擎。
    engine.emit(EngineEvent::FirstFrame);
    let event = engine
        .events()
        .next()
        .await
        .unwrap_or_else(|| panic!("应有首帧"));
    session.handle(event);
    assert_eq!(session.state(), SessionState::Playing);

    engine.emit(EngineEvent::PositionChanged(Duration::from_secs(760)));
    let event = engine
        .events()
        .next()
        .await
        .unwrap_or_else(|| panic!("应有位置"));
    session.handle(event);
    assert_eq!(session.position(), Duration::from_secs(760));

    // 6. seek：状态机立刻给位置，引擎随后对齐。
    assert!(session.seek(Duration::from_secs(3000)));
    engine
        .seek(session.position())
        .await
        .unwrap_or_else(|error| panic!("seek: {error}"));

    // 7. 进度上报：seek 立即报，重复位置去重。
    let mut reporter = ProgressReporter::new(&prefs, clock.clone());
    let sink = CountingSink {
        seen: std::sync::Mutex::new(Vec::new()),
    };
    let progress = PlaybackProgress {
        item_id: ItemId::new("1"),
        source_id: SourceId::new("s"),
        position: session.position(),
        duration: session.duration(),
        played: false,
        paused: false,
        method: PlayMethod::DirectPlay,
    };
    reporter
        .submit(&sink, ProgressTrigger::Seek, &progress)
        .await;
    reporter
        .submit(&sink, ProgressTrigger::Seek, &progress)
        .await;
    let seen = match sink.seen.lock() {
        Ok(seen) => seen.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    assert_eq!(seen, vec![Duration::from_secs(3000)], "重复位置不重复上报");

    // 8. 结束：位置固定为时长。
    engine.emit(EngineEvent::Ended);
    let event = engine
        .events()
        .next()
        .await
        .unwrap_or_else(|| panic!("应有结束事件"));
    session.handle(event);
    assert_eq!(session.state(), SessionState::Ended);
    assert_eq!(session.position(), total);
}

#[tokio::test]
async fn command_sequence_is_recorded_by_the_engine() {
    let clock = Arc::new(ManualClock::new());
    let prefs = UserPreferences::default();
    let mut engine = FakeEngine::new();
    let mut session = Session::new(clock);

    engine
        .open(OpenRequest::new("http://host/v"))
        .await
        .unwrap_or_default();
    engine.emit(EngineEvent::TracksChanged(engine_tracks()));
    let event = engine
        .events()
        .next()
        .await
        .unwrap_or_else(|| panic!("应有轨道事件"));
    session.handle(event);

    let plan = TrackSelector::new().reapply(session.tracks(), &prefs);
    if let Some(index) = plan.audio {
        engine.select_audio(index).await.unwrap_or_default();
    }
    engine
        .select_subtitle(plan.subtitle)
        .await
        .unwrap_or_default();
    session.begin(Duration::ZERO, Some(Duration::from_secs(600)));
    session.handle(EngineEvent::FirstFrame);
    session.seek(Duration::from_secs(120));
    engine.seek(session.position()).await.unwrap_or_default();
    engine.close().await.unwrap_or_default();

    let commands = engine.take_commands();
    assert!(matches!(commands[0], EngineCommand::Open(_)));
    assert_eq!(commands[1], EngineCommand::SelectAudio(2));
    assert_eq!(
        commands[2],
        EngineCommand::SelectSubtitle(SubtitleSelection::Track(3))
    );
    assert_eq!(commands[3], EngineCommand::Seek(Duration::from_secs(120)));
    assert_eq!(commands[4], EngineCommand::Close);
    assert!(engine.commands().is_empty(), "take_commands 应清空记录");
}

#[tokio::test]
async fn engine_trait_is_object_safe_and_drives_the_event_stream() {
    let mut engine: Box<dyn PlaybackEngine> = Box::new(FakeEngine::new());
    engine
        .open(OpenRequest::new("http://host/v"))
        .await
        .unwrap_or_default();
    engine.play().await.unwrap_or_default();
    engine.set_volume(0.5).await.unwrap_or_default();
    assert_eq!(engine.diagnostics(), EngineDiagnostics::default());
    let _stream = engine.events();
    engine.close().await.unwrap_or_default();
}

#[tokio::test]
async fn engine_failures_and_diagnostics_reach_the_session() {
    let clock = Arc::new(ManualClock::new());
    let mut engine = FakeEngine::new();
    let mut session = Session::new(clock.clone());

    // 硬解静默降级：引擎如实上报 → 界面拿到提示。
    engine.set_diagnostics(EngineDiagnostics {
        requested_hwdec: Some("vaapi".into()),
        active_hwdec: Some("no".into()),
        hw_decode_active: false,
        video_decoder: Some("hevc".into()),
        render_backend: Some("opengl".into()),
    });
    session.set_diagnostics(engine.diagnostics());
    assert!(
        session
            .diagnostics()
            .hwdec_notice()
            .unwrap_or_default()
            .contains("硬件解码未生效")
    );

    // 引擎侧错误：分类进 Session，位置保留以便重试。
    engine.fail_next(PlaybackError::Auth);
    session.begin(Duration::from_secs(120), Some(Duration::from_secs(7200)));
    let result = engine.open(OpenRequest::new("http://host/v")).await;
    assert_eq!(result, Err(PlaybackError::Auth));
    session.fail(PlaybackError::Auth);
    assert_eq!(session.state(), SessionState::Failed);
    assert!(
        session
            .error()
            .is_some_and(PlaybackError::needs_user_action)
    );
    assert_eq!(session.retry(), Duration::from_secs(120));
    assert_eq!(session.state(), SessionState::Opening);

    // 命令记录可用于断言"没有静默重启播放"。
    assert!(engine.last_open().is_some());
}
