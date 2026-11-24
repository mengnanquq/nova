// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 地址校验、地址拼接与重试策略。
//!
//! ## 地址
//!
//! [`parse_base_url`] 只接受 `http` / `https` 且必须带主机名。这不是洁癖：
//! 媒体服务器只可能走这两种传输，`file://`（本地文件）与 `javascript:`（伪协议）
//! 出现在“服务器地址”输入框里只会是误输入或恶意构造，越早拒绝越省事；
//! 而 `http://` 本身是合法的（局域网自建服务器），只是会被
//! [`nova_core::ServerInfo::is_plaintext`] 标出来提示用户。
//!
//! 地址里的 userinfo（`http://user:pass@host`）也会被拒绝：解析器接受它，
//! 但那个口令只会被明文带进 `ServerInfo.base_url`、`StreamTarget` 与日志，
//! 而认证本来就走[`Provider::authenticate`](crate::Provider::authenticate)。
//!
//! ## TLS
//!
//! 本模块不提供任何"跳过证书校验"的入口，也从不调用 `danger_accept_invalid_certs`。
//! 自签名证书只能走"把根证书导入系统信任库"这条路。
//!
//! ## 重试
//!
//! 只对**幂等 GET** 的 5xx、429 与超时重试，认证失败与其余 4xx 一律不重试：
//! 令牌错了重试一百次也还是错的，只会让服务端多记几条失败登录。
//! 429 带 `Retry-After` 时按它的要求等（见 [`RETRY_AFTER_CAP`]），但等待时长有上限，
//! 不会被服务端用一个大数字拖死，且总尝试次数不变。

use std::time::{Duration, SystemTime};

use url::Url;

use crate::ProviderError;

/// 默认的最大尝试次数（含首次请求，即最多重试 2 次）。
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// 默认的首个退避间隔。
pub const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(200);

/// 默认的退避上限。
pub const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(2);

/// 服务端 `Retry-After` 的等待上限。
///
/// 5 秒是“用户还愿意等一次自动重试”的边界：真被限流时正确的做法是让界面提示
/// “稍后再试”，而不是让后台请求挂在那里等一分钟。更长的 `Retry-After`
/// （或一个离谱的 429 循环）会被截到这个值。
pub const RETRY_AFTER_CAP: Duration = Duration::from_secs(5);

/// 解析并规范化服务器基址。
///
/// 规范化内容包括：去掉末尾的 `/`，丢弃查询串与片段（基址不该带它们），
/// 只保留 scheme / host / port / path 前缀。
pub fn parse_base_url(raw: &str) -> Result<Url, ProviderError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::Unsupported("服务器地址不能为空"));
    }

    let mut url = Url::parse(trimmed).map_err(|_| {
        ProviderError::Unsupported("服务器地址无法解析，请填写完整地址（含 http:// 或 https://）")
    })?;

    match url.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(ProviderError::Unsupported("服务器地址必须是 http 或 https"));
        }
    }

    if url.host_str().is_none_or(str::is_empty) {
        return Err(ProviderError::Unsupported("服务器地址缺少主机名"));
    }

    // `http://user:pass@host` 是合法 URL，但这里的口令只会被明文带进
    // `ServerInfo.base_url`、`StreamTarget` 与日志。认证走 `Provider::authenticate`，
    // 地址里不需要也不应该有任何凭证。
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProviderError::Unsupported(
            "服务器地址里不能带用户名/密码，请把认证交给登录流程",
        ));
    }

    url.set_query(None);
    url.set_fragment(None);

    // 统一去掉末尾斜杠，拼接时不会出现 `//Items`。
    let path = url.path().trim_end_matches('/').to_owned();
    if path.is_empty() {
        url.set_path("/");
    } else {
        url.set_path(&path);
    }

    Ok(url)
}

/// 在基址上拼接路径段与查询参数。
///
/// 路径段与查询值都会按 URL 规则百分号编码，因此条目 id、搜索词、令牌里的特殊字符
/// 不会把地址拼坏；反过来，也不会有人能通过搜索词注入额外的查询参数。
///
/// 例外是 `.` 与 `..`：它们**不会**被百分号编码（属于 URL 路径规范的一部分），
/// 于是恶意服务端只要把 `Id` 填成 `..` 就能让地址多出一层。这类段一律拒绝。
pub(crate) fn build_url(
    base: &Url,
    segments: &[&str],
    query: &[(&str, &str)],
) -> Result<Url, ProviderError> {
    let mut url = base.clone();
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|()| ProviderError::Unsupported("服务器地址不能作为基址"))?;
        for segment in segments {
            validate_path_segment(segment)?;
            path.push(segment);
        }
    }
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url)
}

/// 校验一个将被拼进请求路径的段。
///
/// 拒绝三类值：
///
/// - `.` / `..`——`Url::path_segments_mut().push` 不会编码它们，反向代理可能据此
///   把请求规范化到上层路径（`/Items/../Images/...`）。
/// - 控制字符（`\r` / `\n` / `\0` 等 C0 与 DEL）——它们唯一的作用是切开请求行或头部。
/// - 空串——拼出来的是 `//`，会把一个路径段变成两个。
///
/// 其余字符（包括 `/`、`?`、`&`）不在这里拒绝：它们会被百分号编码，
/// 拼出来的仍然是**一个**路径段，不需要额外规则。
pub(crate) fn validate_path_segment(segment: &str) -> Result<(), ProviderError> {
    if segment.is_empty() {
        return Err(ProviderError::Unsupported("请求路径不能包含空段"));
    }
    if segment == "." || segment == ".." {
        return Err(ProviderError::Unsupported("请求路径不能包含 . 或 .. 段"));
    }
    if segment.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(ProviderError::Unsupported("请求路径不能包含控制字符"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Retry-After
// ---------------------------------------------------------------------------

/// 解析 `Retry-After` 头，返回“还要等多久”。
///
/// 两种合法格式都支持（RFC 9110 §10.2.3）：
///
/// - 秒数：`120`
/// - IMF-fixdate：`Sun, 06 Nov 1994 08:49:37 GMT`
///
/// 其它任何写法（包括早就不用了的 RFC 850 / asctime 格式）一律返回 `None`：
/// 调用方退回指数退避就好，没必要为了兼容一个已废弃格式写两个解析器。
/// 日期在过去时返回 `Some(Duration::ZERO)`（“现在就可以重试”）。
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = parse_http_date(trimmed)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// 解析 IMF-fixdate（RFC 1123 / RFC 9110 的 `HTTP-date`）。
fn parse_http_date(value: &str) -> Option<SystemTime> {
    let mut parts = value.split_whitespace();
    // 星期几不影响结果（日期本身已经完整），只校验形状。
    let weekday = parts.next()?;
    let weekday_ok = weekday.len() == 4
        && weekday.ends_with(',')
        && weekday.as_bytes()[..3].iter().all(u8::is_ascii_alphabetic);
    if !weekday_ok {
        return None;
    }
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_from_name(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;

    let mut clock = parts.next()?.split(':');
    let hour: u32 = clock.next()?.parse().ok()?;
    let minute: u32 = clock.next()?.parse().ok()?;
    let second: u32 = clock.next()?.parse().ok()?;
    if clock.next().is_some() {
        return None;
    }
    if parts.next()? != "GMT" {
        return None;
    }

    if !(1..=31).contains(&day) || hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).ok()?))
}

/// 英文月份缩写 → 1..=12。只认 IMF-fixdate 的大小写形式。
fn month_from_name(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let index = MONTHS.iter().position(|m| name.eq_ignore_ascii_case(m))?;
    Some(u32::try_from(index).unwrap_or(0) + 1)
}

/// 公历年月日 → 距 1970-01-01 的天数（Howard Hinnant 的 `days_from_civil`）。
///
/// 不引入日期库：一个现在时 + 历史日期都能算对的小函数，比多一个依赖便宜。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let month_from_march = i64::from((month + 9) % 12); // Mar = 0
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 重试策略。
///
/// 默认最多 3 次尝试、指数退避 200ms → 400ms（上限 2s）。
/// 测试可以注入更小的间隔把整个重试路径跑完而不拖慢测试。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    retry_after_cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_BASE_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
            retry_after_cap: RETRY_AFTER_CAP,
        }
    }
}

impl RetryPolicy {
    /// 只改最大尝试次数。`0` 会被当成 `1`（至少请求一次）。
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            ..Self::default()
        }
    }

    /// 不重试。
    pub fn none() -> Self {
        Self::new(1)
    }

    /// 指定退避间隔与上限。
    pub fn with_delays(mut self, base_delay: Duration, max_delay: Duration) -> Self {
        self.base_delay = base_delay;
        self.max_delay = max_delay.max(base_delay);
        self
    }

    /// 指定服务端 `Retry-After` 的等待上限（默认 [`RETRY_AFTER_CAP`]）。
    ///
    /// 测试里把它调小，就能在不真的等几秒的前提下断言 `Retry-After` 生效。
    pub fn with_retry_after_cap(mut self, cap: Duration) -> Self {
        self.retry_after_cap = cap;
        self
    }

    /// 最大尝试次数（含首次）。
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts.max(1)
    }

    /// 第 `retry` 次重试（1 起）之前的等待时间。
    pub fn delay_for(&self, retry: u32) -> Duration {
        let exponent = retry.saturating_sub(1).min(16);
        let factor = 1_u32 << exponent;
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }

    /// 服务端 `Retry-After` 的上限。
    pub fn retry_after_cap(&self) -> Duration {
        self.retry_after_cap
    }

    /// 结合 `Retry-After` 的第 `retry` 次重试等待时间。
    ///
    /// `Retry-After` 的语义是“**至少**等这么久”，所以它只能把等待推长，
    /// 不能缩短指数退避；同时两边都被 `retry_after_cap` / 自己的上限截住，
    /// 避免一个恶意的 429 把请求拖死在后台。
    pub(crate) fn delay_with_retry_after(
        &self,
        retry: u32,
        retry_after: Option<Duration>,
    ) -> Duration {
        let base = self.delay_for(retry);
        match retry_after {
            Some(after) => after.min(self.retry_after_cap).max(base),
            None => base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https() {
        let https = parse_base_url("https://media.example.com:8920").unwrap();
        assert_eq!(https.as_str(), "https://media.example.com:8920/");

        let plain = parse_base_url("  http://192.168.1.10:8096/  ").unwrap();
        assert_eq!(plain.host_str(), Some("192.168.1.10"));
        assert_eq!(plain.port(), Some(8096));
    }

    #[test]
    fn keeps_path_prefix_and_drops_trailing_slash() {
        let url = parse_base_url("https://example.com/emby/").unwrap();
        assert_eq!(url.path(), "/emby");
        assert_eq!(url.as_str(), "https://example.com/emby");
    }

    #[test]
    fn drops_query_and_fragment_from_base() {
        let url = parse_base_url("https://example.com?api_key=leak#frag").unwrap();
        assert_eq!(url.query(), None);
        assert_eq!(url.fragment(), None);
    }

    #[test]
    fn rejects_other_schemes_and_missing_host() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://example.com",
            "",
            "   ",
            "https://",
            "not a url",
        ] {
            let error = parse_base_url(bad).expect_err(&format!("应当拒绝 {bad:?}"));
            assert!(
                matches!(error, ProviderError::Unsupported(_)),
                "{bad:?} 的错误变体不对：{error:?}"
            );
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn rejects_hostless_http() {
        // 空主机名：`http://:8096`、`http://?q`、`http://` 都不行。
        // 注意 `http:///x` 按 URL 规范等价于 `http://x/`，那是合法的。
        for bad in ["http://:8096", "http://?q", "http://"] {
            let error = parse_base_url(bad).expect_err(&format!("应当拒绝 {bad:?}"));
            assert!(
                matches!(error, ProviderError::Unsupported(_)),
                "{bad:?} → {error:?}"
            );
        }
        assert_eq!(parse_base_url("http:///x").unwrap().host_str(), Some("x"));
    }

    #[test]
    fn rejects_urls_with_userinfo() {
        for bad in [
            "http://user:pass@192.168.1.10:8096",
            "https://user@media.example.com",
            "https://:pass@media.example.com",
            "https://user:@media.example.com",
        ] {
            let error = parse_base_url(bad).expect_err(&format!("{bad:?} 带用户名/密码必须拒绝"));
            assert!(
                matches!(error, ProviderError::Unsupported(_)),
                "{bad:?} → {error:?}"
            );
            let text = error.to_string();
            assert!(
                text.contains("用户名") && text.contains("密码"),
                "错误文案应当说明地址里不能带用户名/密码：{text}"
            );
        }
        // 末尾的 `@` 没有 userinfo，不该被误伤。
        assert_eq!(
            parse_base_url("http://media.example.com/@home")
                .unwrap()
                .path(),
            "/@home"
        );
    }

    #[test]
    fn build_url_rejects_dot_segments_and_control_chars() {
        let base = parse_base_url("http://127.0.0.1:8096").unwrap();
        for bad in ["..", ".", "a\rb", "a\nb", "a\0b", "\u{7f}"] {
            let error = build_url(&base, &["Items", bad], &[])
                .expect_err(&format!("路径段 {bad:?} 必须拒绝"));
            assert!(
                matches!(error, ProviderError::Unsupported(_)),
                "{bad:?} → {error:?}"
            );
        }
        // `.` / `..` 与包含控制字符的段不能出现在生成的地址里。
        for bad in ["https://example.com/emby", "http://127.0.0.1:8096"] {
            let base = parse_base_url(bad).unwrap();
            let error =
                build_url(&base, &["Users", "..", "Items"], &[]).expect_err("`..` 段必须拒绝");
            assert!(
                error.to_string().contains("..") || error.to_string().contains('.'),
                "错误文案应当说明段不合规：{error}"
            );
        }
        // 查询值不受影响：它本来就会被百分号编码。
        let url = build_url(&base, &["Items"], &[("v", "..")]).unwrap();
        assert_eq!(url.query(), Some("v=.."));
        assert_eq!(url.path(), "/Items");
    }

    #[test]
    fn build_url_appends_segments_under_the_prefix() {
        let base = parse_base_url("http://127.0.0.1:8096").unwrap();
        let url = build_url(
            &base,
            &["Items", "abc", "Images", "Primary"],
            &[("tag", "t 1")],
        )
        .unwrap();
        assert_eq!(url.path(), "/Items/abc/Images/Primary");
        assert_eq!(url.query(), Some("tag=t+1"));

        let prefixed = parse_base_url("https://example.com/emby").unwrap();
        let url = build_url(&prefixed, &["Users", "u", "Views"], &[]).unwrap();
        assert_eq!(url.as_str(), "https://example.com/emby/Users/u/Views");
    }

    #[test]
    fn build_url_encodes_path_segments_and_query() {
        let base = parse_base_url("http://127.0.0.1:8096").unwrap();
        let url = build_url(
            &base,
            &["Items", "a/b?c"],
            &[("Limit", "20"), ("SearchTerm", "银翼 & 2049")],
        )
        .unwrap();
        // 路径段里的分隔符必须被编码，不能拼出额外的路径层级。
        assert_eq!(url.path(), "/Items/a%2Fb%3Fc");
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs,
            vec![
                ("Limit".to_owned(), "20".to_owned()),
                ("SearchTerm".to_owned(), "银翼 & 2049".to_owned()),
            ]
        );
    }

    #[test]
    fn retry_policy_backoff_is_exponential_and_capped() {
        let policy =
            RetryPolicy::new(4).with_delays(Duration::from_millis(100), Duration::from_millis(250));
        assert_eq!(policy.max_attempts(), 4);
        assert_eq!(policy.delay_for(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for(3), Duration::from_millis(250));
        assert_eq!(policy.delay_for(9), Duration::from_millis(250));
    }

    #[test]
    fn retry_policy_defaults_and_edges() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts(), DEFAULT_MAX_ATTEMPTS);
        assert_eq!(policy.delay_for(1), DEFAULT_BASE_DELAY);
        assert_eq!(RetryPolicy::new(0).max_attempts(), 1);
        assert_eq!(RetryPolicy::none().max_attempts(), 1);
        // 上限不小于基准间隔。
        let policy =
            RetryPolicy::none().with_delays(Duration::from_millis(500), Duration::from_millis(10));
        assert_eq!(policy.delay_for(5), Duration::from_millis(500));
    }

    #[test]
    fn retry_after_is_parsed_as_seconds_or_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(784_111_777 - 3_600);
        // 秒数
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(120)),
        );
        assert_eq!(
            parse_retry_after(" 0 ", now),
            Some(Duration::ZERO),
            "首尾空白要容忍",
        );
        // HTTP 日期（IMF-fixdate）：1994-11-06T08:49:37Z = 784111777
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(Duration::from_secs(3_600)),
        );
        // 过去的日期 → 立刻可以重试，而不是一直等。
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", SystemTime::UNIX_EPOCH),
            Some(Duration::from_secs(784_111_777)),
        );
        assert_eq!(
            parse_retry_after(
                "Sun, 06 Nov 1994 08:49:37 GMT",
                SystemTime::UNIX_EPOCH + Duration::from_secs(784_111_777 + 10),
            ),
            Some(Duration::ZERO),
        );
    }

    #[test]
    fn unparsable_retry_after_is_ignored() {
        let now = SystemTime::UNIX_EPOCH;
        for bad in [
            "",
            "   ",
            "soon",
            "-1",
            "1.5",
            "Sun, 32 Nov 1994 08:49:37 GMT",  // 日期越界
            "Sun, 06 Foo 1994 08:49:37 GMT",  // 月份不认识
            "Sun, 06 Nov 1994 25:49:37 GMT",  // 小时越界
            "Sun, 06 Nov 1994 08:49 GMT",     // 缺秒
            "Sunday, 06-Nov-94 08:49:37 GMT", // 废弃的 RFC 850 格式
            "Sun Nov  6 08:49:37 1994",       // 废弃的 asctime 格式
            "Sun, 06 Nov 1994 08:49:37 UTC",  // 时区必须是 GMT
        ] {
            assert_eq!(parse_retry_after(bad, now), None, "{bad:?} 应当被忽略",);
        }
    }

    #[test]
    fn retry_after_raises_the_delay_but_is_capped() {
        let policy = RetryPolicy::new(3)
            .with_delays(Duration::from_millis(10), Duration::from_millis(20))
            .with_retry_after_cap(Duration::from_millis(500));
        assert_eq!(policy.retry_after_cap(), Duration::from_millis(500));

        // 没有 Retry-After：指数退避。
        assert_eq!(
            policy.delay_with_retry_after(1, None),
            Duration::from_millis(10)
        );
        // 比退避短：不能缩短等待（Retry-After 是“至少等这么久”）。
        assert_eq!(
            policy.delay_with_retry_after(2, Some(Duration::from_millis(1))),
            Duration::from_millis(20),
        );
        // 比退避长：按服务端要求等。
        assert_eq!(
            policy.delay_with_retry_after(1, Some(Duration::from_millis(300))),
            Duration::from_millis(300),
        );
        // 离谱的大值：截到上限。
        assert_eq!(
            policy.delay_with_retry_after(1, Some(Duration::from_secs(3_600))),
            Duration::from_millis(500),
        );
        // 默认上限是 5 秒。
        assert_eq!(RetryPolicy::default().retry_after_cap(), RETRY_AFTER_CAP);
        assert_eq!(RETRY_AFTER_CAP, Duration::from_secs(5));
    }
}
