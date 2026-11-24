// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! mpv 错误码 / 日志 → [`PlaybackError`] 的分类（纯函数，可单测）。
//!
//! 界面永远不显示 mpv 的原始错误（`reqwest`/`mpv` 的文法对用户毫无意义），
//! 而且原始文本里可能有播放直链的令牌：所以这里只输出分类，细节留给 `tracing`。

use nova_playback::PlaybackError;

/// `MPV_ERROR_*` 里本引擎用作判据的几个常量（与 client.h 一致）。
mod codes {
    pub(super) const UNKNOWN_FORMAT: i32 = -17;
    pub(super) const UNSUPPORTED: i32 = -18;
    pub(super) const NOT_IMPLEMENTED: i32 = -19;
    pub(super) const LOADING_FAILED: i32 = -13;
    pub(super) const NOTHING_TO_PLAY: i32 = -16;
    pub(super) const VO_INIT_FAILED: i32 = -15;
}

/// 把 mpv 的 `MPV_END_FILE` 错误码分类。
///
/// 规则（与任务书一致，另加一条 401/403 → `Auth` 的细化）：
///
/// | 条件 | 结果 |
/// |---|---|
/// | `NOT_IMPLEMENTED` / `UNSUPPORTED` / `UNKNOWN_FORMAT` / `NOTHING_TO_PLAY` | `Unsupported` |
/// | `LOADING_FAILED` + 日志里有 401/403 | `Auth` |
/// | `LOADING_FAILED` + 日志里有其他 4xx | `NotFound` |
/// | `LOADING_FAILED` + URL 是 http(s) | `Network` |
/// | 日志里有解码失败字样 | `Decode` |
/// | 其余（含 VO 初始化失败） | `Engine("…")`，**不含 URL** |
pub(crate) fn classify(code: i32, is_network_url: bool, log_tail: &[String]) -> PlaybackError {
    if code == codes::NOT_IMPLEMENTED
        || code == codes::UNSUPPORTED
        || code == codes::UNKNOWN_FORMAT
        || code == codes::NOTHING_TO_PLAY
    {
        return PlaybackError::Unsupported;
    }
    if code == codes::LOADING_FAILED {
        match http_status(log_tail) {
            Some(401 | 403) => return PlaybackError::Auth,
            Some(400..=499) => return PlaybackError::NotFound,
            _ => {}
        }
        if is_network_url {
            return PlaybackError::Network;
        }
    }
    if mentions_decode(log_tail) {
        return PlaybackError::Decode;
    }
    if code == codes::VO_INIT_FAILED {
        // VO 初始化失败是"我们这边的渲染链坏了"，不是片源问题；重试/换版本都没用。
        return PlaybackError::engine("mpv VO 初始化失败：渲染上下文不可用");
    }
    PlaybackError::engine(format!(
        "mpv 播放失败（code {code}: {}）",
        safe_reason(code)
    ))
}

/// 日志尾部的 HTTP 状态码（mpv/ffmpeg 会打 `HTTP error 404 Not Found`）。
pub(crate) fn http_status(log_tail: &[String]) -> Option<u16> {
    for line in log_tail.iter().rev() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("http") {
            continue;
        }
        for token in lower.split(|c: char| !c.is_ascii_digit()) {
            if token.len() != 3 {
                continue;
            }
            if let Ok(status) = token.parse::<u16>() {
                if (400..600).contains(&status) {
                    return Some(status);
                }
            }
        }
    }
    None
}

fn mentions_decode(log_tail: &[String]) -> bool {
    log_tail.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("error while decoding")
            || lower.contains("failed to decode")
            || lower.contains("could not decode")
            || lower.contains("decoder error")
            || lower.contains("invalid nal")
    })
}

fn safe_reason(code: i32) -> String {
    crate::ffi::error_string(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn unsupported_codes_map_to_unsupported() {
        for code in [-17, -18, -19, -16] {
            assert_eq!(classify(code, false, &[]), PlaybackError::Unsupported);
            assert_eq!(classify(code, true, &[]), PlaybackError::Unsupported);
        }
    }

    #[test]
    fn loading_failed_is_classified_by_log_and_scheme() {
        // 4xx 日志 → NotFound / Auth
        assert_eq!(
            classify(-13, true, &lines(&["[ffmpeg] HTTP error 404 Not Found"])),
            PlaybackError::NotFound
        );
        assert_eq!(
            classify(-13, true, &lines(&["[ffmpeg] HTTP error 401 Unauthorized"])),
            PlaybackError::Auth
        );
        assert_eq!(
            classify(-13, true, &lines(&["[ffmpeg] HTTP error 403 Forbidden"])),
            PlaybackError::Auth
        );
        // 没有状态码但是网络地址 → Network
        assert_eq!(
            classify(-13, true, &lines(&["Failed to open /videos/1/stream"])),
            PlaybackError::Network
        );
        // 本地文件加载失败不是网络问题
        assert!(matches!(
            classify(-13, false, &lines(&["Failed to open file"])),
            PlaybackError::Engine(_)
        ));
    }

    #[test]
    fn decode_failures_are_not_network_errors() {
        let log = lines(&["[ffmpeg] Error while decoding stream #0:0: Invalid data"]);
        assert_eq!(classify(0, true, &log), PlaybackError::Decode);
        assert_eq!(classify(-20, false, &log), PlaybackError::Decode);
    }

    #[test]
    fn engine_message_never_contains_a_url() {
        let error = classify(
            -20,
            true,
            &lines(&["http://192.168.1.9:8096/v?api_key=SECRET"]),
        );
        let text = format!("{error:?} {} {:?}", error, error.raw_message());
        assert!(!text.contains("192.168"), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
        assert!(!text.contains("api_key"), "{text}");
    }

    #[test]
    fn http_status_picks_the_last_status_in_the_tail() {
        assert_eq!(
            http_status(&lines(&["HTTP error 500 boom", "HTTP error 404 Not Found"])),
            Some(404)
        );
        assert_eq!(http_status(&lines(&["nothing here"])), None);
        assert_eq!(http_status(&lines(&["HTTP error 200 OK"])), None);
    }
}
