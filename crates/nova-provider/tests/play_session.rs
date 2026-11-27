// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! `PlaySessionId` 必须绑在**会话**上，而不是绑在条目上。
//!
//! 审查 🟡4：同一部片重播时，旧会话的 `Exit`/`stop` 是异步发的；与此同时新会话的
//! `PlaybackInfo`（`refresh_before_play`）已经把条目上的 `PlaySessionId` 覆盖成新值。
//! 如果上报在**发送那一刻**才去查"这个条目最近一次的会话标识"，旧会话的
//! `stop` 就会带上新会话的标识——服务端关掉的是新会话，旧会话一直留着。
//!
//! 所以 player 在起播拿到 `PlaybackInfo` 的那一刻就把标识快照进本会话
//! （`PlaybackProgress::play_session_id`），上报只负责原样带回。这里断言的是
//! "客户端实际发出的请求体"。

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{ITEM_ID, SOURCE_ID, provider};
use nova_core::{ItemId, PlayMethod, PlaybackProgress, SourceId};
use nova_provider::{Flavor, MediaProvider};
use nova_secrets::{MemoryStore, SecretString};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// 每次 `PlaybackInfo` 换一个 `PlaySessionId`（模拟重播时服务端分配的新会话）。
struct RotatingSessions(Mutex<VecDeque<String>>);

impl RotatingSessions {
    fn new(sessions: &[&str]) -> Self {
        Self(Mutex::new(
            sessions.iter().map(|s| (*s).to_owned()).collect(),
        ))
    }
}

impl Respond for RotatingSessions {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let next = match self.0.lock() {
            Ok(mut queue) => queue.pop_front(),
            Err(poisoned) => poisoned.into_inner().pop_front(),
        };
        let mut body = common::json_fixture("playback_info");
        if let Some(session) = next {
            body["PlaySessionId"] = serde_json::Value::String(session);
        }
        ResponseTemplate::new(200).set_body_json(body)
    }
}

fn progress(seq: u64, play_session_id: Option<String>) -> PlaybackProgress {
    PlaybackProgress {
        session_seq: seq,
        play_session_id,
        item_id: ItemId::new(ITEM_ID),
        source_id: SourceId::new(SOURCE_ID),
        position: Duration::from_secs(30),
        duration: Some(Duration::from_secs(9816)),
        played: false,
        paused: false,
        method: PlayMethod::DirectPlay,
    }
}

/// 某个上报端点上实际发出的 `PlaySessionId`（按发送顺序）。
async fn posted_sessions(server: &MockServer, target: &str) -> Vec<Option<String>> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == target)
        .map(|request| {
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null);
            body.get("PlaySessionId")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect()
}

#[tokio::test]
async fn replayed_item_keeps_the_old_play_session_id_on_stop() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, Flavor::Jellyfin, Arc::clone(&store));

    common::mount_login(&server, Flavor::Jellyfin).await;
    provider
        .authenticate("tester", &SecretString::new("correct horse"))
        .await
        .expect("登录应当成功");

    Mock::given(method("GET"))
        .and(path(format!("/Items/{ITEM_ID}/PlaybackInfo")))
        .respond_with(RotatingSessions::new(&["sess-old", "sess-new"]))
        .expect(2)
        .mount(&server)
        .await;
    for endpoint in ["/Sessions/Playing/Progress", "/Sessions/Playing/Stopped"] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
    }

    let item = ItemId::new(ITEM_ID);
    // 旧会话：取一次 `PlaybackInfo`，进度在起播时就快照下它给的会话标识。
    provider.sources(&item).await.expect("第一次取资源");
    let old = provider
        .latest_play_session_id(&item)
        .expect("第一次 PlaybackInfo 应当给出会话标识");
    assert_eq!(old, "sess-old");

    // 同一部片重播：又取一次 `PlaybackInfo`，条目上的记录被新会话覆盖。
    provider.sources(&item).await.expect("第二次取资源");
    assert_eq!(
        provider.latest_play_session_id(&item).as_deref(),
        Some("sess-new"),
        "用例前提：重播时 PlaybackInfo 会覆盖条目上的会话标识"
    );

    // 旧会话的收尾在晚一步的发送里：带着起播时的快照，服务端关掉的是旧会话。
    provider
        .report_progress(&progress(1, Some(old.clone())))
        .await
        .expect("旧会话的进度上报应当成功");
    provider
        .report_stopped(&progress(1, Some(old.clone())))
        .await
        .expect("旧会话的 stop 应当成功");

    assert_eq!(
        posted_sessions(&server, "/Sessions/Playing/Progress").await,
        [Some("sess-old".to_owned())],
        "旧会话的进度不能被新会话的 PlaybackInfo 改写标识"
    );
    assert_eq!(
        posted_sessions(&server, "/Sessions/Playing/Stopped").await,
        [Some("sess-old".to_owned())],
        "重播后旧会话的 stop 必须带旧会话的 PlaySessionId"
    );

    // 兼容：没有携带标识的进度（旧版本落盘的待发文件、`play_session_id` 为 `None`）
    // 退回"按条目查最近一次"的老行为，而不是干脆不带标识。
    provider
        .report_stopped(&progress(0, None))
        .await
        .expect("没有标识的 stop 也应当成功");
    assert_eq!(
        posted_sessions(&server, "/Sessions/Playing/Stopped").await,
        [Some("sess-old".to_owned()), Some("sess-new".to_owned())],
        "没有标识时退回条目上最近一次的会话标识"
    );
}
