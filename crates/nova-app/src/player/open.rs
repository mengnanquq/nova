// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 播放直链与 `OpenRequest` 的构造（纯函数，可单测）。
//!
//! 两条硬规则来自 `docs/research/player.md` §四：
//!
//! 1. **交给 mpv 的 URL 不带 `api_key`**：令牌走 `Authorization: MediaBrowser Token="…"` 头
//!    （`http-header-fields`）。这样令牌不会出现在 mpv 的日志、错误与崩溃报告里。
//! 2. 起播位置、音轨、字幕都在 `OpenRequest` 里一次给全，避免起播后再下三条命令
//!    （那会多一帧黑屏）。

use std::time::Duration;

use nova_playback::{OpenRequest, TrackPlan};
use url::Url;

/// 从播放直链里摘掉 `api_key`，返回 `(不含令牌的地址, 令牌)`。
///
/// 服务端给的直链形如 `…/Videos/{id}/stream?Static=true&MediaSourceId={sid}&api_key={token}`。
/// 其它查询参数（`Static` / `MediaSourceId`）必须保留：Jellyfin 靠它们区分"直连原始文件"。
pub(crate) fn strip_api_key(raw: &str) -> (String, Option<String>) {
    let Ok(mut url) = Url::parse(raw) else {
        // 解析不了就不要猜：原样返回（mpv 会自己报错），也不假装有令牌。
        return (raw.to_owned(), None);
    };
    let mut token = None;
    let mut kept: Vec<(String, String)> = Vec::new();
    for (name, value) in url.query_pairs() {
        if name.eq_ignore_ascii_case("api_key") || name.eq_ignore_ascii_case("apikey") {
            if token.is_none() {
                token = Some(value.into_owned());
            }
        } else {
            kept.push((name.into_owned(), value.into_owned()));
        }
    }
    if token.is_none() {
        return (raw.to_owned(), None);
    }
    url.set_query(None);
    if !kept.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in kept {
            pairs.append_pair(&name, &value);
        }
    }
    (url.into(), token)
}

/// Jellyfin / Emby 的令牌认证头。
pub(crate) fn authorization_header(token: &str) -> (String, String) {
    (
        "Authorization".to_owned(),
        format!("MediaBrowser Token=\"{token}\""),
    )
}

/// 由直链 + 起播位置 + 轨道计划构造 [`OpenRequest`]。
pub(crate) fn build_request(raw_url: &str, resume_at: Duration, plan: &TrackPlan) -> OpenRequest {
    let (url, token) = strip_api_key(raw_url);
    let mut request = OpenRequest::new(url)
        .with_resume_at(resume_at)
        .with_audio(plan.audio)
        .with_subtitle(plan.subtitle.clone());
    if let Some(token) = token {
        let (name, value) = authorization_header(&token);
        request = request.with_header(name, value);
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_playback::SubtitleSelection;

    fn plan() -> TrackPlan {
        TrackPlan {
            audio: Some(2),
            subtitle: SubtitleSelection::Track(4),
        }
    }

    #[test]
    fn api_key_is_moved_to_the_authorization_header() {
        let (url, token) = strip_api_key(
            "http://127.0.0.1:8096/Videos/abc/stream?Static=true&MediaSourceId=s1&api_key=SECRET",
        );
        assert_eq!(token.as_deref(), Some("SECRET"));
        assert!(!url.contains("SECRET"), "{url}");
        assert!(!url.contains("api_key"), "{url}");
        assert!(url.contains("Static=true"), "{url}");
        assert!(url.contains("MediaSourceId=s1"), "{url}");
    }

    #[test]
    fn request_carries_token_only_in_a_header() {
        let request = build_request(
            "http://127.0.0.1:8096/Videos/abc/stream?Static=true&api_key=SECRET",
            Duration::from_secs(90),
            &plan(),
        );
        assert!(!request.url.contains("SECRET"));
        assert_eq!(request.resume_at, Duration::from_secs(90));
        assert_eq!(request.audio, Some(2));
        assert_eq!(request.subtitle, SubtitleSelection::Track(4));
        assert_eq!(request.headers.len(), 1);
        assert_eq!(request.headers[0].0, "Authorization");
        assert_eq!(request.headers[0].1, "MediaBrowser Token=\"SECRET\"");
        // Debug 输出里不出现令牌（OpenRequest 已脱敏）。
        let debug = format!("{request:?}");
        assert!(!debug.contains("SECRET"), "{debug}");
    }

    #[test]
    fn url_without_token_is_left_alone() {
        let (url, token) = strip_api_key("http://host:8096/Videos/abc/stream?Static=true");
        assert!(token.is_none());
        assert_eq!(url, "http://host:8096/Videos/abc/stream?Static=true");
        let request = build_request("http://host/x.mkv", Duration::ZERO, &plan());
        assert!(request.headers.is_empty());
    }

    #[test]
    fn unparsable_url_does_not_panic() {
        let (url, token) = strip_api_key("not a url");
        assert_eq!(token, None);
        assert_eq!(url, "not a url");
    }
}
