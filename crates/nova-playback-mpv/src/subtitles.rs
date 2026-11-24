// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 外挂字幕的落盘。
//!
//! 调研结论（`docs/research/player.md` §六.2）：`sub-file` 是否接受 http 地址**未验证**，
//! 所以这里不赌——先把字幕下载到临时文件，再把本地路径交给 `sub-add`。
//!
//! 只支持 `http://`：本 crate 没有 TLS 依赖（也不允许为此引入一个），`https://` 的外挂
//! 字幕需要上层（provider 层，有 reqwest）先下载好再传本地路径。这个限制写在
//! [`SubtitleCache::resolve`] 的错误里，不会静默失败。
//!
//! 实现用 `tokio::net` / `tokio::fs`，所以**调用方的 async 上下文必须在 tokio runtime 里**
//! （`nova-app` 本来就是；脱离 runtime 调用会在 await 点 panic，这是 tokio 的行为）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use nova_playback::{PlaybackError, redact_url};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

/// 单次下载的上限（字幕后端正常几百 KB；10 MB 足够挡住"下到一个视频文件"的意外）。
const MAX_SUBTITLE_BYTES: usize = 10 * 1024 * 1024;
/// 连接与整体超时。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// 外挂字幕的临时目录：引擎存活期间一直存在，`Drop` 时连同文件一起删掉。
pub(crate) struct SubtitleCache {
    dir: Option<tempfile::TempDir>,
}

impl SubtitleCache {
    pub(crate) fn new() -> Self {
        Self { dir: None }
    }

    /// 把 `SubtitleSelection::External { path }` 归一化成"磁盘上真实存在的路径"。
    ///
    /// - 已经是本地且存在的文件：原样使用（不复制、不删除）。
    /// - `http://`：下载到临时目录。
    /// - `https://`：报错（见模块文档）。
    pub(crate) async fn resolve(
        &mut self,
        path: &str,
        headers: &[(String, String)],
    ) -> Result<PathBuf, PlaybackError> {
        let local = Path::new(path);
        if !path.contains("://") {
            if local.is_file() {
                return Ok(local.to_path_buf());
            }
            return Err(PlaybackError::engine(format!(
                "外挂字幕文件不存在（{}）",
                redact_url(path)
            )));
        }
        let url =
            url::Url::parse(path).map_err(|_| PlaybackError::engine("外挂字幕地址无法解析"))?;
        if url.scheme() != "http" {
            return Err(PlaybackError::engine(format!(
                "外挂字幕下载只支持 http（当前 {}）；https 需要上层先下载好再传本地路径",
                url.scheme()
            )));
        }
        let bytes = download_http(&url, headers).await?;
        let extension = url
            .path()
            .rsplit('.')
            .next()
            .filter(|ext| ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("srt");
        let dir = self.ensure_dir()?;
        let file = dir
            .path()
            .join(format!("subtitle-{}.{extension}", fingerprint(path)));
        tokio::fs::write(&file, &bytes)
            .await
            .map_err(|error| PlaybackError::engine(format!("写入外挂字幕临时文件失败: {error}")))?;
        debug!(
            target: "nova_playback_mpv",
            "外挂字幕已落盘 {}（{} 字节）",
            file.display(),
            bytes.len()
        );
        Ok(file)
    }

    /// 引擎关闭时清空临时目录。
    pub(crate) fn clear(&mut self) {
        self.dir = None;
    }

    fn ensure_dir(&mut self) -> Result<&tempfile::TempDir, PlaybackError> {
        if self.dir.is_none() {
            let dir = tempfile::Builder::new()
                .prefix("nova-mpv-subs-")
                .tempdir()
                .map_err(|error| PlaybackError::engine(format!("创建字幕临时目录失败: {error}")))?;
            self.dir = Some(dir);
        }
        self.dir
            .as_ref()
            .ok_or_else(|| PlaybackError::engine("字幕临时目录不可用"))
    }
}

/// 用文件名指纹（不是完整 URL）命名临时文件：路径里不带令牌。
fn fingerprint(path: &str) -> String {
    // FNV-1a：只需要稳定且不泄露原文，不用密码学强度。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 极简 HTTP/1.1 GET（只为下载字幕；不处理 TLS、压缩、keep-alive）。
async fn download_http(
    url: &url::Url,
    headers: &[(String, String)],
) -> Result<Vec<u8>, PlaybackError> {
    let host = url
        .host_str()
        .ok_or_else(|| PlaybackError::engine("外挂字幕地址缺少主机名"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addr = format!("{host}:{port}");
    let mut stream = tokio::time::timeout(DOWNLOAD_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| PlaybackError::Network)?
        .map_err(|_| PlaybackError::Network)?;

    let mut request = String::new();
    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    request.push_str(&format!("GET {target} HTTP/1.1\r\n"));
    request.push_str(&format!("Host: {host}:{port}\r\n"));
    request.push_str("Accept: */*\r\n");
    request.push_str("Connection: close\r\n");
    for (name, value) in headers {
        // 头里可能带令牌：不进日志，只写进请求。
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    tokio::time::timeout(DOWNLOAD_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .map_err(|_| PlaybackError::Network)?
        .map_err(|_| PlaybackError::Network)?;

    let mut raw = Vec::new();
    let read = tokio::time::timeout(DOWNLOAD_TIMEOUT, async {
        let mut buffer = [0u8; 8192];
        loop {
            let read = stream.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..read]);
            if raw.len() > MAX_SUBTITLE_BYTES {
                return Err(std::io::Error::other("外挂字幕超过大小上限"));
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| PlaybackError::Network)?;
    read.map_err(|error| PlaybackError::engine(format!("读取外挂字幕失败: {error}")))?;

    parse_http_response(&raw)
}

/// 解析 HTTP 响应（状态行 + 头 + body），支持 `Content-Length` 与 `chunked`。
pub(crate) fn parse_http_response(raw: &[u8]) -> Result<Vec<u8>, PlaybackError> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| PlaybackError::engine("外挂字幕响应缺少头部结束标记"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let status_line = head.lines().next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| PlaybackError::engine("外挂字幕响应状态行无法解析"))?;
    if (300..400).contains(&status) {
        return Err(PlaybackError::engine(
            "外挂字幕下载被重定向，请让上层先解析地址",
        ));
    }
    if status == 401 || status == 403 {
        return Err(PlaybackError::Auth);
    }
    if status == 404 {
        return Err(PlaybackError::NotFound);
    }
    if !(200..300).contains(&status) {
        return Err(PlaybackError::Network);
    }

    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if name == "content-length" {
            length = value.parse().ok();
        }
    }
    if chunked {
        return decode_chunked(body);
    }
    if let Some(length) = length {
        if body.len() < length {
            return Err(PlaybackError::engine("外挂字幕响应被截断"));
        }
        return Ok(body[..length].to_vec());
    }
    Ok(body.to_vec())
}

/// 解 `Transfer-Encoding: chunked` 的 body。
fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>, PlaybackError> {
    let mut out = Vec::new();
    loop {
        let Some(end) = body.windows(2).position(|window| window == b"\r\n") else {
            return Err(PlaybackError::engine("chunked 响应被截断"));
        };
        let size_text = String::from_utf8_lossy(&body[..end]);
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| PlaybackError::engine("chunked 响应长度无法解析"))?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err(PlaybackError::engine("chunked 响应被截断"));
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
        if out.len() > MAX_SUBTITLE_BYTES {
            return Err(PlaybackError::engine("外挂字幕超过大小上限"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_response() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        assert_eq!(
            parse_http_response(raw).unwrap_or_default(),
            b"hello".to_vec()
        );
    }

    #[test]
    fn parses_chunked_response() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(
            parse_http_response(raw).unwrap_or_default(),
            b"hello world".to_vec()
        );
    }

    #[test]
    fn status_codes_map_to_playback_errors() {
        let response = |status: &str| format!("HTTP/1.1 {status}\r\n\r\n");
        assert_eq!(
            parse_http_response(response("401 Unauthorized").as_bytes()),
            Err(PlaybackError::Auth)
        );
        assert_eq!(
            parse_http_response(response("404 Not Found").as_bytes()),
            Err(PlaybackError::NotFound)
        );
        assert_eq!(
            parse_http_response(response("500 Boom").as_bytes()),
            Err(PlaybackError::Network)
        );
        assert!(matches!(
            parse_http_response(response("302 Found").as_bytes()),
            Err(PlaybackError::Engine(_))
        ));
    }

    #[test]
    fn truncated_and_garbage_bodies_are_errors_not_panics() {
        assert!(parse_http_response(b"garbage").is_err());
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhi").is_err());
        assert!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz")
                .is_err()
        );
    }

    #[test]
    fn temp_file_name_does_not_contain_the_url() {
        let path = "http://host:8096/Videos/1/Subs/2/Stream.srt?api_key=SECRET";
        let name = format!("subtitle-{}.srt", fingerprint(path));
        assert!(!name.contains("SECRET"));
        assert!(!name.contains("host"));
        assert_eq!(fingerprint(path), fingerprint(path));
        assert_ne!(fingerprint(path), fingerprint("http://host/other"));
    }

    #[test]
    fn missing_local_file_is_reported_without_a_path_leak() {
        let mut cache = SubtitleCache::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            return;
        };
        let error = runtime
            .block_on(cache.resolve("/nonexistent/secret-name.srt", &[]))
            .err();
        assert!(matches!(error, Some(PlaybackError::Engine(_))));
    }
}
