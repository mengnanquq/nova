// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! Emby / Jellyfin 响应的 DTO 与到 [`nova_core`] 模型的映射。
//!
//! # 为什么只有一套 DTO
//!
//! Emby 与 Jellyfin 是同一个代码库的两个分支，条目、媒体源、媒体流这三个核心
//! JSON 结构**字段名与语义完全一致**，真正不同的只有：
//!
//! | 差异点 | Jellyfin | Emby |
//! |---|---|---|
//! | 认证请求的密码字段 | `Pw` | `Password` |
//! | 认证头名字 | `Authorization` | `X-Emby-Authorization` |
//! | 令牌兜底来源 | 响应体 `AccessToken` | 响应体 `AccessToken`，旧版本只给 `X-Emby-Token` 响应头 |
//! | HDR 描述 | `VideoRangeType`（`HDR10` / `DOVI…` / `HLG`） | 只有 `VideoRange`（`SDR` / `HDR`） |
//!
//! 维护两套 DTO 意味着每次兼容性改动都要写两遍、测两遍，而收益只是省掉
//! [`Flavor`] 上的几个 `match`——这是明确的负收益。所以：**一套 DTO，差异集中在
//! [`Flavor`] 与少量可选字段的兜底逻辑里**。将来若某个服务端出现结构性差异
//! （例如字段类型变化），也应该是"给同一套 DTO 加兜底"，而不是分叉出第二套。
//!
//! # 映射原则
//!
//! - 服务端字段一律视为不可信输入：缺字段、空数组、类型越界都不许 panic。
//! - "缺数据"与"数据坏了"分开处理：缺字段退化为 `None` / 空 `Vec`，
//!   结构上无法解释的值才返回 [`ProviderError`]。
//! - 未知条目类型（`BoxSet` / `Person` / `MusicAlbum` …）在**列表**里被静默跳过，
//!   在**单条获取**时明确报 [`ProviderError::InvalidResponse`]，不猜、不崩。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Deserialize;
use url::Url;

use nova_core::{
    AudioDetails, HdrFormat, ImageSet, ItemId, Language, MediaItem, MediaKind, MediaSource,
    MediaStream, PlayMethod, ServerKind, SourceId, StreamKind, SubtitleDetails, UserData,
    VideoDetails,
};

use crate::error::ProviderError;
use crate::http::build_url;
use crate::types::{ImageKind, Library};

/// 每 tick 的纳秒数：服务端的时间单位是 100ns。
const NANOS_PER_TICK: u64 = 100;

/// 一秒包含多少 tick。
const TICKS_PER_SECOND: i64 = 10_000_000;

/// 服务端产品差异。
///
/// 只放真正不同的东西：字段解析完全共用，见本模块文档。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flavor {
    /// Jellyfin。
    Jellyfin,
    /// Emby。
    Emby,
}

impl Flavor {
    /// 对应的统一模型服务端类型。
    pub fn server_kind(self) -> ServerKind {
        match self {
            Self::Jellyfin => ServerKind::Jellyfin,
            Self::Emby => ServerKind::Emby,
        }
    }

    /// 凭证存储键名的前缀（`jellyfin/<server-id>/token`）。
    pub fn credential_prefix(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Emby => "emby",
        }
    }

    /// 认证请求 / 请求头里使用的头名字。
    pub fn auth_header_name(self) -> &'static str {
        match self {
            Self::Jellyfin => "authorization",
            Self::Emby => "x-emby-authorization",
        }
    }

    /// 认证请求体里的密码字段名。
    /// 认证请求里密码字段的名字。
    ///
    /// Emby 历史上用 `Password`，但 4.10 实测只认 `Pw`（发 `Password` 会 401）。
    /// 所以先发 `Pw`，真的被拒再用 `Password` 重试一次——两家、各版本都能覆盖。
    pub fn password_field(self) -> &'static str {
        match self {
            Self::Jellyfin => "Pw",
            Self::Emby => "Pw",
        }
    }

    /// 认证被 401 后是否值得换密码字段名再试一次（只对 Emby 有意义）。
    pub fn legacy_password_field(self) -> Option<&'static str> {
        match self {
            Self::Jellyfin => None,
            Self::Emby => Some("Password"),
        }
    }

    /// 认证响应里令牌的兜底响应头（旧版 Emby 只给这个）。
    pub fn token_header_name(self) -> Option<&'static str> {
        match self {
            Self::Jellyfin => None,
            Self::Emby => Some("x-emby-token"),
        }
    }
}

// ---------------------------------------------------------------------------
// 不可信数组的长度上限
// ---------------------------------------------------------------------------

/// `MediaSources` 数组的长度上限。
///
/// 一个条目真实存在的“版本”通常只有 1~5 个（不同分辨率 / 码率），一万个已经是
/// “服务端把一个库全塞进一个条目”的级别了。没有上限的话，一个坏掉的服务端
/// 可以用几 MB 的响应让客户端分配几 GB 内存。
pub(crate) const MAX_MEDIA_SOURCES: usize = 10_000;

/// 单个媒体源的 `MediaStreams` 长度上限。
///
/// 4K remux 的音轨 + 字幕轨常见几十条，200 条已经非常宽松。
pub(crate) const MAX_MEDIA_STREAMS: usize = 200;

/// `Genres` 的长度上限。类型标签正常不超过 10 个。
pub(crate) const MAX_GENRES: usize = 100;

/// 图片 tag 集合（`ImageTags` / 背景图 tag 列表）的长度上限。
///
/// 图片类型（Primary / Backdrop / Logo / Thumb …）一共十几种，64 足够；
/// 多出来的只可能是噪声。超过上限时保留哪几个键是不确定的（`HashMap` 无序），
/// 但真实响应只有十来个键，碰不到这个分支。
pub(crate) const MAX_IMAGE_TAGS: usize = 64;

/// 生成一个带长度上限的 `Vec` 反序列化器。
///
/// 关键是**边读边丢**：超限的元素读完就 drop，不会攒在 `Vec` 里，
/// 所以模型里始终只有上限那么多元素（响应原文本身的大小由 `reqwest` 的读取上限决定）。
macro_rules! capped_seq {
    ($name:ident, $limit:expr, $what:expr) => {
        fn $name<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
        where
            D: serde::Deserializer<'de>,
            T: Deserialize<'de>,
        {
            struct CappedSeq<T> {
                what: &'static str,
                _marker: std::marker::PhantomData<T>,
            }

            impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for CappedSeq<T> {
                type Value = Vec<T>;

                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(formatter, "{} 的数组", self.what)
                }

                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Self::Value, A::Error> {
                    let limit = $limit;
                    let hint = seq.size_hint().unwrap_or(0).min(limit);
                    let mut kept: Vec<T> = Vec::with_capacity(hint);
                    let mut dropped = 0_usize;
                    while let Some(value) = seq.next_element::<T>()? {
                        if kept.len() < limit {
                            kept.push(value);
                        } else {
                            // 超限的元素立刻 drop，不在内存里攒着。
                            dropped = dropped.saturating_add(1);
                        }
                    }
                    // 只 warn 一次（一个字段一条），不在循环里刷屏。
                    if dropped > 0 {
                        tracing::warn!(field = self.what, limit, dropped, "响应数组超长，已截断");
                    }
                    Ok(kept)
                }
            }

            deserializer.deserialize_seq(CappedSeq {
                what: $what,
                _marker: std::marker::PhantomData,
            })
        }
    };
}

capped_seq!(capped_genres, MAX_GENRES, "Genres");
capped_seq!(capped_backdrop_tags, MAX_IMAGE_TAGS, "BackdropImageTags");
capped_seq!(
    capped_parent_backdrop_tags,
    MAX_IMAGE_TAGS,
    "ParentBackdropImageTags"
);
capped_seq!(capped_media_sources, MAX_MEDIA_SOURCES, "MediaSources");
capped_seq!(capped_media_streams, MAX_MEDIA_STREAMS, "MediaStreams");

/// `ImageTags`（map 形式）的同一个上限。
fn capped_image_tags<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CappedTags;

    impl<'de> serde::de::Visitor<'de> for CappedTags {
        type Value = HashMap<String, String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "ImageTags 对象")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let hint = map.size_hint().unwrap_or(0).min(MAX_IMAGE_TAGS);
            let mut kept: HashMap<String, String> = HashMap::with_capacity(hint);
            let mut dropped = 0_usize;
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if kept.len() < MAX_IMAGE_TAGS {
                    kept.insert(key, value);
                } else {
                    dropped = dropped.saturating_add(1);
                }
            }
            if dropped > 0 {
                tracing::warn!(
                    field = "ImageTags",
                    limit = MAX_IMAGE_TAGS,
                    dropped,
                    "响应数组超长，已截断"
                );
            }
            Ok(kept)
        }
    }

    deserializer.deserialize_map(CappedTags)
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// 一个条目（电影 / 剧集 / 季 / 集 / 媒体库视图共用）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawItem {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(rename = "Type", default)]
    pub(crate) item_type: Option<String>,
    #[serde(default)]
    pub(crate) sort_name: Option<String>,
    #[serde(default)]
    pub(crate) original_title: Option<String>,
    #[serde(default)]
    pub(crate) production_year: Option<i64>,
    #[serde(default)]
    pub(crate) overview: Option<String>,
    #[serde(default, deserialize_with = "capped_genres")]
    pub(crate) genres: Vec<String>,
    #[serde(default)]
    pub(crate) run_time_ticks: Option<i64>,
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
    #[serde(default)]
    pub(crate) index_number: Option<i64>,
    #[serde(default)]
    pub(crate) index_number_end: Option<i64>,
    #[serde(default)]
    pub(crate) child_count: Option<i64>,
    #[serde(default, deserialize_with = "capped_image_tags")]
    pub(crate) image_tags: HashMap<String, String>,
    #[serde(default, deserialize_with = "capped_backdrop_tags")]
    pub(crate) backdrop_image_tags: Vec<String>,
    #[serde(default)]
    pub(crate) parent_backdrop_item_id: Option<String>,
    #[serde(default, deserialize_with = "capped_parent_backdrop_tags")]
    pub(crate) parent_backdrop_image_tags: Vec<String>,
    #[serde(default)]
    pub(crate) user_data: Option<RawUserData>,
    #[serde(default, deserialize_with = "capped_media_sources")]
    pub(crate) media_sources: Vec<RawMediaSource>,
    #[serde(default)]
    pub(crate) collection_type: Option<String>,
}

/// 条目的用户观看状态。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawUserData {
    #[serde(default)]
    pub(crate) played: bool,
    #[serde(default)]
    pub(crate) playback_position_ticks: Option<i64>,
    #[serde(default)]
    pub(crate) play_count: Option<i64>,
    #[serde(default)]
    pub(crate) is_favorite: bool,
}

/// 列表响应（`/Items`、`/Users/{id}/Views` 等）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawItemsResponse {
    #[serde(default)]
    pub(crate) items: Vec<RawItem>,
    #[serde(default)]
    pub(crate) total_record_count: Option<u32>,
}

/// 一个可播放资源。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawMediaSource {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) container: Option<String>,
    #[serde(default)]
    pub(crate) size: Option<i64>,
    #[serde(default)]
    pub(crate) bitrate: Option<i64>,
    /// 资源自身的时长。
    ///
    /// 实测（Jellyfin 12.4）：即使 `GET /Items?Fields=MediaSources` 把 `MediaSources`
    /// 带回来了，这里的 `RunTimeTicks` 仍是 `null`；只有 `PlaybackInfo` 与条目详情
    /// 的 `MediaSources[]` 才带时长。所以从这里拿到 `None` 是正常情况，不是缺字段。
    #[serde(default)]
    pub(crate) run_time_ticks: Option<i64>,
    #[serde(default, deserialize_with = "capped_media_streams")]
    pub(crate) media_streams: Vec<RawMediaStream>,
}

/// 一条媒体流。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawMediaStream {
    #[serde(rename = "Type", default)]
    pub(crate) stream_type: Option<String>,
    #[serde(default)]
    pub(crate) index: Option<i64>,
    #[serde(default)]
    pub(crate) codec: Option<String>,
    #[serde(default)]
    pub(crate) language: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) display_title: Option<String>,
    #[serde(default)]
    pub(crate) is_default: bool,
    #[serde(default)]
    pub(crate) is_forced: bool,
    #[serde(default)]
    pub(crate) is_external: bool,
    #[serde(default)]
    pub(crate) delivery_url: Option<String>,
    #[serde(default)]
    pub(crate) width: Option<i64>,
    #[serde(default)]
    pub(crate) height: Option<i64>,
    #[serde(default)]
    pub(crate) bit_depth: Option<i64>,
    #[serde(default)]
    pub(crate) real_frame_rate: Option<f64>,
    #[serde(default)]
    pub(crate) average_frame_rate: Option<f64>,
    #[serde(default)]
    pub(crate) video_range_type: Option<String>,
    #[serde(default)]
    pub(crate) video_range: Option<String>,
    #[serde(default)]
    pub(crate) is_interlaced: bool,
    #[serde(default)]
    pub(crate) channels: Option<i64>,
    #[serde(default)]
    pub(crate) sample_rate: Option<i64>,
    #[serde(default)]
    pub(crate) bit_rate: Option<i64>,
    #[serde(default)]
    pub(crate) profile: Option<String>,
}

/// `PlaybackInfo` 响应（只用到媒体源部分）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawPlaybackInfo {
    #[serde(default, deserialize_with = "capped_media_sources")]
    pub(crate) media_sources: Vec<RawMediaSource>,
    /// 会话标识。上报进度时必须带上，否则服务端无法把上报关联到播放会话
    /// （实测表现：服务端日志 `PlaybackStopped reported with null media info`，
    /// 且条目上的 `PlaybackPositionTicks` 永远不更新）。
    #[serde(default)]
    pub(crate) play_session_id: Option<String>,
}

/// `GET /System/Info/Public` 响应（只取界面要显示 / 判定产品类型的几项）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawPublicSystemInfo {
    /// 服务端自报的 `Id`。**不是**本机分配的 `ServerId`，只用于展示 / 对账。
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) server_name: Option<String>,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) product_name: Option<String>,
    /// Emby 的 `/System/Info/Public` 带 `LocalAddresses`/`RemoteAddresses`（复数、数组），
    /// Jellyfin 只有单数的 `LocalAddress`。这是两家**唯一稳定可用的判据**：
    /// 我们实测的 Emby 4.10.2 与部分 Jellyfin 构建都不返回 `ProductName`。
    #[serde(default)]
    pub(crate) local_addresses: Option<Vec<String>>,
}

/// `POST /Users/AuthenticateByName` 响应。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawAuthenticationResult {
    #[serde(default)]
    pub(crate) access_token: Option<String>,
    #[serde(default)]
    pub(crate) server_id: Option<String>,
    #[serde(default)]
    pub(crate) user: Option<RawUser>,
}

/// 认证响应里的用户。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawUser {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

/// 服务端错误响应体（只取可读原因）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RawErrorBody {
    #[serde(default)]
    pub(crate) message: Option<String>,
}

// ---------------------------------------------------------------------------
// 时间
// ---------------------------------------------------------------------------

/// `RunTimeTicks` / `PlaybackPositionTicks`（100ns 单位）→ [`Duration`]。
///
/// 负值（服务端偶尔用来表示"未知"）返回 `None`；`0` 返回 `Some(Duration::ZERO)`，
/// "0 是否等于未知"由调用方按语义决定（见 [`runtime_from_ticks`] 与
/// [`resume_from_ticks`]）。
pub(crate) fn ticks_to_duration(ticks: i64) -> Option<Duration> {
    if ticks < 0 {
        return None;
    }
    let ticks = u64::try_from(ticks).unwrap_or(u64::MAX);
    let seconds = ticks / (TICKS_PER_SECOND.unsigned_abs());
    let nanos = (ticks % (TICKS_PER_SECOND.unsigned_abs())) * NANOS_PER_TICK;
    let nanos = u32::try_from(nanos).unwrap_or(0);
    Some(Duration::new(seconds, nanos))
}

/// 片长：缺失或 `<= 0` 都视为未知（`None`）。
///
/// 这一条与 [`ticks_to_duration`] 分开是有原因的：`nova_core::MediaItem::runtime`
/// 为 `Some(0)` 会让 `progress_fraction` 静默返回 `None`，但界面上会显示"0 分钟"。
/// 剧集条目经常不给 `RunTimeTicks`，显示"未知"才是对的。
pub(crate) fn runtime_from_ticks(ticks: Option<i64>) -> Option<Duration> {
    let duration = ticks_to_duration(ticks?)?;
    if duration.is_zero() {
        None
    } else {
        Some(duration)
    }
}

/// 续播位置：缺失或负值都视为 0（"没看过"）。
pub(crate) fn resume_from_ticks(ticks: Option<i64>) -> Duration {
    ticks_to_duration(ticks.unwrap_or(0)).unwrap_or(Duration::ZERO)
}

/// [`Duration`] → 上报进度用的 tick。
pub(crate) fn duration_to_ticks(duration: Duration) -> i64 {
    let ticks = duration.as_nanos() / u128::from(NANOS_PER_TICK);
    i64::try_from(ticks).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// 语言
// ---------------------------------------------------------------------------

/// ISO 639-2（3 字母）→ ISO 639-1（2 字母）。
///
/// 服务端给的语言是 3 字母码（`eng` / `chi` / `zho`），而 `nova_core::Language::matches`
/// 只比较主语言子标签，`eng` 与 `en` 匹配不上——不归一化的话，界面按语言挑字幕会全部落空。
/// 这张表只覆盖常见语种；查不到就原样保留（宁可匹配失败，也不要匹配错）。
const ISO639_2_TO_1: &[(&str, &str)] = &[
    ("ara", "ar"),
    ("bul", "bg"),
    ("ces", "cs"),
    ("chi", "zh"),
    ("zho", "zh"),
    ("dan", "da"),
    ("dut", "nl"),
    ("nld", "nl"),
    ("ell", "el"),
    ("gre", "el"),
    ("eng", "en"),
    ("est", "et"),
    ("fin", "fi"),
    ("fra", "fr"),
    ("fre", "fr"),
    ("ger", "de"),
    ("deu", "de"),
    ("heb", "he"),
    ("hin", "hi"),
    ("hrv", "hr"),
    ("hun", "hu"),
    ("ind", "id"),
    ("ita", "it"),
    ("jpn", "ja"),
    ("kor", "ko"),
    ("lav", "lv"),
    ("lit", "lt"),
    ("may", "ms"),
    ("msa", "ms"),
    ("nor", "nb"),
    ("pol", "pl"),
    ("por", "pt"),
    ("ron", "ro"),
    ("rum", "ro"),
    ("rus", "ru"),
    ("slo", "sk"),
    ("slk", "sk"),
    ("slv", "sl"),
    ("spa", "es"),
    ("srp", "sr"),
    ("swe", "sv"),
    ("tha", "th"),
    ("tur", "tr"),
    ("ukr", "uk"),
    ("vie", "vi"),
];

/// 把服务端语言字段归一化成 [`Language`]。
///
/// - 空串 / `und` / `unknown` / `mul` → [`Language::undetermined`]
/// - 3 字母 ISO 639-2 码 → 2 字母 ISO 639-1（`chi` / `zho` → `zh`）
/// - 主语言统一小写，区域子标签保持原样（`ZH-Hans` → `zh-Hans`）
pub(crate) fn normalize_language(raw: Option<&str>) -> Language {
    let Some(raw) = raw else {
        return Language::undetermined();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Language::undetermined();
    }

    let mut parts = trimmed.split(['-', '_']);
    let Some(primary_raw) = parts.next() else {
        return Language::undetermined();
    };
    let primary = primary_raw.to_ascii_lowercase();
    if matches!(primary.as_str(), "und" | "unknown" | "mul" | "zxx") {
        return Language::undetermined();
    }

    let normalized = ISO639_2_TO_1
        .iter()
        .find(|(three, _)| *three == primary)
        .map_or(primary, |(_, two)| (*two).to_owned());

    let suffix: Vec<&str> = parts.filter(|part| !part.is_empty()).collect();
    if suffix.is_empty() {
        Language::new(normalized)
    } else {
        Language::new(format!("{normalized}-{}", suffix.join("-")))
    }
}

// ---------------------------------------------------------------------------
// 条目
// ---------------------------------------------------------------------------

/// 服务端 `Type` → [`MediaKind`]。不认识的类型返回 `None`。
pub(crate) fn map_kind(raw: &str) -> Option<MediaKind> {
    match raw {
        "Movie" => Some(MediaKind::Movie),
        "Series" => Some(MediaKind::Series),
        "Season" => Some(MediaKind::Season),
        "Episode" => Some(MediaKind::Episode),
        _ => None,
    }
}

/// [`MediaKind`] → 服务端查询参数 `IncludeItemTypes` 的取值。
pub(crate) fn kind_to_api(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "Movie",
        MediaKind::Series => "Series",
        MediaKind::Season => "Season",
        MediaKind::Episode => "Episode",
    }
}

/// 综合公开信息判定服务端类型。
///
/// 顺序：产品名 → Emby 独有的 `LocalAddresses` 字段 → 回退到构造时的 `Flavor`。
/// 为什么需要第二步：实测 Emby 4.10.2 与 Jellyfin 12.x 都可能不返回 `ProductName`，
/// 那时只靠产品名会把 Emby 认成 Jellyfin（凭证键、客户端名、部分端点都会跟着错）。
pub(crate) fn kind_from_public(
    product: Option<&str>,
    has_emby_addresses: bool,
    fallback: ServerKind,
) -> ServerKind {
    let by_product = kind_from_product(product, fallback);
    if by_product != fallback || has_emby_addresses {
        // 产品名给出了明确答案就不看字段；否则用 Emby 独有字段兜底。
        return if by_product != fallback {
            by_product
        } else {
            ServerKind::Emby
        };
    }
    fallback
}

/// 服务端自称的产品名（`/System/Info/Public` 的 `ProductName`）→ [`ServerKind`]。
///
/// 产品名比构造时的 [`Flavor`] 更权威：同一个代码库既可以被包装成
/// `"Emby Server"` 也可以被包装成 `"Jellyfin Server"`。两者都对不上时
/// （第三方分支、改了品牌的构建）回退到 `fallback`（即构造时的 `Flavor`），
/// 不猜。
pub(crate) fn kind_from_product(product: Option<&str>, fallback: ServerKind) -> ServerKind {
    let Some(product) = product.map(str::trim).filter(|name| !name.is_empty()) else {
        return fallback;
    };
    let product = product.to_ascii_lowercase();
    if product.contains("jellyfin") {
        ServerKind::Jellyfin
    } else if product.contains("emby") {
        ServerKind::Emby
    } else {
        fallback
    }
}

/// [`PlayMethod`] → 上报进度里的 `PlayMethod` 取值。
///
/// 服务端用这个字段统计“哪些设备在被迫转码”，写错了界面上的转码标记就会失真。
pub(crate) fn play_method_to_api(method: PlayMethod) -> &'static str {
    match method {
        PlayMethod::DirectPlay => "DirectPlay",
        PlayMethod::DirectStream => "DirectStream",
        PlayMethod::Transcode => "Transcode",
    }
}

/// 单个条目映射。缺 `Id`、缺类型或类型不认识都返回 `InvalidResponse`。
pub(crate) fn map_item(raw: &RawItem, base: &Url) -> Result<MediaItem, ProviderError> {
    let id = raw
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or(ProviderError::InvalidResponse("条目缺少 id"))?;
    let item_type = raw
        .item_type
        .as_deref()
        .ok_or(ProviderError::InvalidResponse("条目缺少类型"))?;
    let kind = map_kind(item_type).ok_or(ProviderError::InvalidResponse(
        "条目的媒体类型不在支持范围内",
    ))?;

    let title = raw.name.clone().unwrap_or_else(|| id.to_owned());
    Ok(MediaItem {
        id: ItemId::new(id),
        kind,
        title: title.clone(),
        sort_title: raw
            .sort_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or(title),
        original_title: raw.original_title.clone().filter(|v| !v.is_empty()),
        year: raw
            .production_year
            .and_then(|year| u16::try_from(year).ok())
            .filter(|year| *year > 0),
        overview: raw.overview.clone().filter(|v| !v.is_empty()),
        genres: raw.genres.clone(),
        runtime: runtime_from_ticks(raw.run_time_ticks),
        parent_id: raw
            .parent_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(ItemId::new),
        index_number: raw.index_number.and_then(|n| u32::try_from(n).ok()),
        index_number_end: raw.index_number_end.and_then(|n| u32::try_from(n).ok()),
        child_count: raw.child_count.and_then(|n| u32::try_from(n).ok()),
        images: map_images(raw, base)?,
        user_data: map_user_data(raw.user_data.as_ref()),
    })
}

/// 列表映射：跳过未知类型 / 缺 id 的条目。
///
/// 媒体库里混着 `BoxSet`、`Person`、`Photo` 这些本层不认识的类型，
/// 因为一条不认识就让整页失败是不可接受的。
///
/// `parent_hint` 是“**直接子项**查询”里请求的父条目：实测 Jellyfin 12.4 的列表
/// 响应不带 `ParentId`，不给回填的话 `MediaItem::parent_id` 在列表路径上恒为 `None`，
/// 本地缓存就无法按媒体库检索（`nova items --cached --library` 只能拿顶层条目应付）。
///
/// 回填规则（两条都很重要）：
///
/// 1. **只在响应里没有 `ParentId` 时回填**：服务端明确给了值就以它为准，不覆盖
///    （空串也算没给）。
/// 2. **只对直接子项查询传 `Some`**：递归查询（`Recursive=true`）里各层级的父项
///    各不相同，拿请求里的 parent 回填会把季 / 集全写成媒体库，语义是错的。
///    调用方（`items()` / `children()`）负责只在确定是直接子项时给 `Some`。
pub(crate) fn map_items(
    raws: &[RawItem],
    base: &Url,
    parent_hint: Option<&ItemId>,
) -> Vec<MediaItem> {
    let mut items = Vec::with_capacity(raws.len());
    for raw in raws {
        match map_item(raw, base) {
            Ok(mut item) => {
                if item.parent_id.is_none() {
                    item.parent_id = parent_hint.cloned();
                }
                items.push(item);
            }
            Err(error) => {
                tracing::debug!(
                    item_type = raw.item_type.as_deref().unwrap_or(""),
                    error = %error,
                    "跳过无法映射的条目"
                );
            }
        }
    }
    items
}

/// 用户观看状态。
pub(crate) fn map_user_data(raw: Option<&RawUserData>) -> UserData {
    let Some(raw) = raw else {
        return UserData::default();
    };
    UserData {
        played: raw.played,
        resume_position: resume_from_ticks(raw.playback_position_ticks),
        play_count: raw
            .play_count
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(0),
        favorite: raw.is_favorite,
    }
}

/// 媒体库映射。不支持的 `CollectionType`（音乐、照片、书…）返回 `None`。
pub(crate) fn map_library(raw: &RawItem) -> Option<Library> {
    let id = raw.id.as_deref().filter(|id| !id.is_empty())?;
    let name = raw.name.clone().filter(|name| !name.is_empty())?;
    // 服务端原值也要留着：`kind` 只区分电影 / 剧集，而界面做筛选、
    // 拼子库查询时往往需要 `movies` / `tvshows` / `music` 这样的原字符串。
    let collection_type = raw.collection_type.as_deref()?;
    let kind = match collection_type {
        "movies" => MediaKind::Movie,
        "tvshows" => MediaKind::Series,
        _ => return None,
    };
    Some(Library {
        id: ItemId::new(id),
        name,
        kind,
        collection_type: Some(collection_type.to_owned()),
        item_count: raw.child_count.and_then(|count| u32::try_from(count).ok()),
    })
}

// ---------------------------------------------------------------------------
// 图片
// ---------------------------------------------------------------------------

/// 图片地址（绝对地址，**不含**令牌）。
pub(crate) fn image_url(
    base: &Url,
    item_id: &str,
    kind: ImageKind,
    tag: Option<&str>,
) -> Result<String, ProviderError> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(tag) = tag.filter(|tag| !tag.is_empty()) {
        query.push(("tag", tag));
    }
    let url = build_url(base, &["Items", item_id, "Images", kind.as_api()], &query)?;
    Ok(url.into())
}

/// `ImageTags` / `BackdropImageTags` / `ParentBackdropItemId` → [`ImageSet`]。
///
/// 背景图优先取"父条目的背景图"：单集自己通常没有背景图，服务端会通过
/// `ParentBackdropItemId` 指到所属剧集的那一张，这正是详情页想要的画面。
///
/// `poster_tag` / `backdrop_tag` 存的是**实际拼进 URL 的那个 tag**：缓存键必须与
/// 图片地址一一对应，否则背景图取自父条目时就会拿着一张恒为 `None` 的 tag
/// 去缓存一张会变的图。
pub(crate) fn map_images(raw: &RawItem, base: &Url) -> Result<ImageSet, ProviderError> {
    let item_id = raw.id.as_deref().unwrap_or_default();

    let poster_tag = non_empty_tag(raw.image_tags.get("Primary"));
    let poster = poster_tag
        .as_deref()
        .map(|tag| image_url(base, item_id, ImageKind::Poster, Some(tag)))
        .transpose()?;

    let (backdrop, backdrop_tag) = match (
        raw.parent_backdrop_image_tags.first(),
        raw.parent_backdrop_item_id.as_deref(),
    ) {
        (Some(tag), Some(parent)) if !parent.is_empty() && !tag.is_empty() => (
            Some(image_url(base, parent, ImageKind::Backdrop, Some(tag))?),
            Some(tag.clone()),
        ),
        _ => match non_empty_tag(raw.backdrop_image_tags.first()) {
            Some(tag) => (
                Some(image_url(base, item_id, ImageKind::Backdrop, Some(&tag))?),
                Some(tag),
            ),
            None => (None, None),
        },
    };

    let logo = raw
        .image_tags
        .get("Logo")
        .map(|tag| image_url(base, item_id, ImageKind::Logo, Some(tag)))
        .transpose()?;

    let thumb = raw
        .image_tags
        .get("Thumb")
        .map(|tag| image_url(base, item_id, ImageKind::Thumb, Some(tag)))
        .transpose()?;

    Ok(ImageSet {
        poster,
        backdrop,
        logo,
        thumb,
        poster_tag,
        backdrop_tag,
    })
}

/// 图片 tag：空串按“没有”处理（空 tag 拼进 URL 只会得到一个必然失效的缓存键）。
fn non_empty_tag(tag: Option<&String>) -> Option<String> {
    tag.map(String::as_str)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// 媒体源与媒体流
// ---------------------------------------------------------------------------

/// 媒体源列表映射。空 `Vec` 就是空——"没有可播放文件"不是错误，
/// 剧集 / 季这类容器本来就没有。
pub(crate) fn map_sources(
    raws: &[RawMediaSource],
    item_id: &ItemId,
    base: &Url,
) -> Vec<MediaSource> {
    let mut sources = Vec::with_capacity(raws.len());
    for raw in raws {
        let Some(id) = raw.id.as_deref().filter(|id| !id.is_empty()) else {
            // 没有 id 就无法拼出播放地址，宁可不给，也不给一个猜出来的。
            tracing::debug!("跳过没有 id 的媒体源");
            continue;
        };
        sources.push(MediaSource {
            id: SourceId::new(id),
            item_id: item_id.clone(),
            name: raw.name.clone().filter(|name| !name.is_empty()),
            container: raw.container.clone().filter(|c| !c.is_empty()),
            size: raw.size.and_then(|size| u64::try_from(size).ok()),
            bitrate: raw.bitrate.and_then(|bitrate| u64::try_from(bitrate).ok()),
            // 列表路径上这个字段实测是 `null`（见 `RawMediaSource::run_time_ticks`），
            // 只有 `PlaybackInfo` / 详情才带得到时长。
            runtime: runtime_from_ticks(raw.run_time_ticks),
            streams: map_streams(&raw.media_streams, base),
        });
    }
    sources
}

/// 媒体流列表映射。
///
/// 跳过 `Type` 缺失、`Index` 缺失或为负、以及 `EmbeddedImage` / `Data` 这类
/// 不属于音视频字幕的流；其余一律保留。视频流缺宽高时按 0 处理（详见
/// [`map_video_details`]）。
///
/// 同一个 `Index` 只保留**第一条**：下游（播放器、`nova_core` 的选轨）都按 index
/// 指代一条流，两条同 index 的流会让“选第 1 条音轨”存在歧义（可能落到字幕上）。
pub(crate) fn map_streams(raws: &[RawMediaStream], base: &Url) -> Vec<MediaStream> {
    let mut streams = Vec::with_capacity(raws.len());
    let mut seen: HashSet<u32> = HashSet::with_capacity(raws.len());
    let mut duplicates = 0_usize;
    for raw in raws {
        let Some(stream) = map_stream(raw, base) else {
            continue;
        };
        if !seen.insert(stream.index) {
            duplicates = duplicates.saturating_add(1);
            continue;
        }
        streams.push(stream);
    }
    if duplicates > 0 {
        tracing::warn!(duplicates, "响应里有重复的流 index，已保留第一条");
    }
    streams
}

fn map_stream(raw: &RawMediaStream, base: &Url) -> Option<MediaStream> {
    let kind = match raw.stream_type.as_deref()? {
        "Video" => StreamKind::Video,
        "Audio" => StreamKind::Audio,
        "Subtitle" => StreamKind::Subtitle,
        _ => return None,
    };
    let index = u32::try_from(raw.index?).ok()?;
    let codec = raw
        .codec
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let language = normalize_language(raw.language.as_deref());
    let title = raw
        .title
        .clone()
        .or_else(|| raw.display_title.clone())
        .filter(|value| !value.is_empty());

    let video = match kind {
        StreamKind::Video => Some(map_video_details(raw)),
        _ => None,
    };
    let audio = match kind {
        StreamKind::Audio => Some(AudioDetails {
            channels: raw
                .channels
                .and_then(|channels| u16::try_from(channels).ok()),
            sample_rate: raw
                .sample_rate
                .and_then(|rate| u32::try_from(rate).ok())
                .filter(|rate| *rate > 0),
            bitrate: raw.bit_rate.and_then(|rate| u64::try_from(rate).ok()),
            profile: raw.profile.clone().filter(|profile| !profile.is_empty()),
        }),
        _ => None,
    };
    let subtitle = match kind {
        StreamKind::Subtitle => Some(SubtitleDetails {
            format: Some(codec.clone()).filter(|codec| !codec.is_empty()),
            hearing_impaired: is_hearing_impaired(raw),
        }),
        _ => None,
    };

    Some(MediaStream {
        index,
        kind,
        codec,
        language,
        title,
        is_default: raw.is_default,
        is_forced: raw.is_forced,
        is_external: raw.is_external,
        url: raw
            .delivery_url
            .as_deref()
            .filter(|url| !url.is_empty())
            .and_then(|url| absolute_url(base, url)),
        video,
        audio,
        subtitle,
    })
}

/// 视频流细节。
///
/// 宽高缺失时按 0 处理：保留这条流（编码、位深、HDR 仍然有用），
/// 让 `VideoDetails::pixels()` 为 0、`is_uhd()` 为 `false`。
/// 丢弃整条流会让"按分辨率挑版本"完全失效，反而丢信息。
fn map_video_details(raw: &RawMediaStream) -> VideoDetails {
    VideoDetails {
        width: raw
            .width
            .and_then(|width| u32::try_from(width).ok())
            .unwrap_or(0),
        height: raw
            .height
            .and_then(|height| u32::try_from(height).ok())
            .unwrap_or(0),
        frame_rate: raw
            .real_frame_rate
            .or(raw.average_frame_rate)
            .filter(|rate| *rate > 0.0)
            .map(|rate| rate as f32),
        bit_depth: raw.bit_depth.and_then(|depth| u8::try_from(depth).ok()),
        hdr: map_hdr(raw),
        interlaced: raw.is_interlaced,
    }
}

/// HDR 格式。
///
/// Jellyfin 10.9+ 给 `VideoRangeType`；Emby 只有 `VideoRange`（`SDR` / `HDR`）。
/// 两者都缺失时返回 `None`（= SDR 或未知），不要臆测 HDR。
pub(crate) fn map_hdr(raw: &RawMediaStream) -> Option<HdrFormat> {
    if let Some(range_type) = raw.video_range_type.as_deref() {
        let normalized = range_type.trim().to_ascii_uppercase();
        return match normalized.as_str() {
            "" | "SDR" | "UNKNOWN" => None,
            "HDR10" => Some(HdrFormat::Hdr10),
            "HDR10PLUS" | "HDR10+" => Some(HdrFormat::Hdr10Plus),
            "DOVI" | "DOVIWITHHDR10" | "DOVIWITHHLG" | "DOVIWITHSDR" | "DOVIWITHEL" => {
                Some(HdrFormat::DolbyVision)
            }
            "HLG" => Some(HdrFormat::Hlg),
            other => Some(HdrFormat::Other(other.to_owned())),
        };
    }
    match raw.video_range.as_deref().map(str::trim) {
        Some(range) if range.eq_ignore_ascii_case("HDR") => Some(HdrFormat::Other("HDR".into())),
        _ => None,
    }
}

/// 听障字幕判定。
///
/// 服务端没有专门的字段，但轨道标题里通常写着 `SDH` / `Hearing Impaired` / `听障`，
/// 这是唯一可用的信号。
fn is_hearing_impaired(raw: &RawMediaStream) -> bool {
    let haystack = format!(
        "{} {}",
        raw.title.as_deref().unwrap_or_default(),
        raw.display_title.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    haystack.contains("sdh") || haystack.contains("hearing impaired") || haystack.contains("听障")
}

/// 外挂字幕 / 外部音轨的地址：相对地址基于服务器基址补全，绝对地址原样保留。
///
/// 绝对地址里的 `user:pass@` 会被**剥离**而不是直接丢弃：地址的其余部分
/// （host / path / 查询串）仍然有用，保留它至少不会让字幕静默消失；
/// 而凭证一旦进了模型就会被播放器命令行、日志、崩溃报告与截图看到。
fn absolute_url(base: &Url, raw: &str) -> Option<String> {
    if let Ok(mut url) = Url::parse(raw) {
        return match url.scheme() {
            "http" | "https" => {
                if !url.username().is_empty() || url.password().is_some() {
                    tracing::debug!("外挂流地址带 userinfo，已剥离");
                    url.set_username("").ok()?;
                    url.set_password(None).ok()?;
                }
                Some(url.into())
            }
            _ => None,
        };
    }
    // 基址可能带路径前缀（`https://host/emby`），`Url::join` 会把它当成文件名替换掉，
    // 所以先补一个结尾斜杠，让相对地址落在前缀下面。
    let mut root = base.clone();
    let mut path = root.path().to_owned();
    if !path.ends_with('/') {
        path.push('/');
    }
    root.set_path(&path);
    root.join(raw.trim_start_matches('/')).ok().map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        crate::parse_base_url("http://127.0.0.1:8096").unwrap()
    }

    fn prefixed_base() -> Url {
        crate::parse_base_url("https://media.example.com/emby").unwrap()
    }

    fn fixture(name: &str) -> String {
        match name {
            "item_movie" => include_str!("../tests/fixtures/item_movie.json").to_owned(),
            "item_episode" => include_str!("../tests/fixtures/item_episode.json").to_owned(),
            "items_movies" => include_str!("../tests/fixtures/items_movies.json").to_owned(),
            "views" => include_str!("../tests/fixtures/views_jellyfin.json").to_owned(),
            "playback_info" => include_str!("../tests/fixtures/playback_info.json").to_owned(),
            "auth_jellyfin" => include_str!("../tests/fixtures/auth_jellyfin.json").to_owned(),
            "auth_emby" => include_str!("../tests/fixtures/auth_emby.json").to_owned(),
            other => panic!("未知 fixture：{other}"),
        }
    }

    fn raw_item(name: &str) -> RawItem {
        serde_json::from_str(&fixture(name)).expect("fixture 应当能解析")
    }

    #[test]
    fn flavor_differences() {
        assert_eq!(Flavor::Jellyfin.server_kind(), ServerKind::Jellyfin);
        assert_eq!(Flavor::Emby.server_kind(), ServerKind::Emby);
        assert_eq!(Flavor::Jellyfin.credential_prefix(), "jellyfin");
        assert_eq!(Flavor::Emby.credential_prefix(), "emby");
        assert_eq!(Flavor::Jellyfin.auth_header_name(), "authorization");
        assert_eq!(Flavor::Emby.auth_header_name(), "x-emby-authorization");
        // 两家现在都用 `Pw` 作为首选字段；Emby 的旧字段名只在 401 后重试时用到。
        assert_eq!(Flavor::Jellyfin.password_field(), "Pw");
        assert_eq!(Flavor::Emby.password_field(), "Pw");
        assert_eq!(Flavor::Jellyfin.legacy_password_field(), None);
        assert_eq!(Flavor::Emby.legacy_password_field(), Some("Password"));
        assert_eq!(Flavor::Jellyfin.token_header_name(), None);
        assert_eq!(Flavor::Emby.token_header_name(), Some("x-emby-token"));
    }

    #[test]
    fn ticks_conversion_boundaries() {
        assert_eq!(ticks_to_duration(0), Some(Duration::ZERO));
        assert_eq!(ticks_to_duration(1), Some(Duration::from_nanos(100)));
        assert_eq!(
            ticks_to_duration(9_999_999),
            Some(Duration::new(0, 999_999_900))
        );
        assert_eq!(ticks_to_duration(10_000_000), Some(Duration::from_secs(1)));
        assert_eq!(
            ticks_to_duration(98_160_000_000),
            Some(Duration::from_secs(9816))
        );
        // 负值是"未知"，不是 0。
        assert_eq!(ticks_to_duration(-1), None);
        assert_eq!(ticks_to_duration(i64::MIN), None);
        // 极大值不能溢出成 panic。
        assert!(ticks_to_duration(i64::MAX).is_some());
    }

    #[test]
    fn runtime_treats_zero_and_missing_as_unknown() {
        assert_eq!(runtime_from_ticks(None), None);
        assert_eq!(runtime_from_ticks(Some(0)), None);
        assert_eq!(runtime_from_ticks(Some(-1)), None);
        assert_eq!(
            runtime_from_ticks(Some(98_160_000_000)),
            Some(Duration::from_secs(9816))
        );
    }

    #[test]
    fn resume_position_defaults_to_zero() {
        assert_eq!(resume_from_ticks(None), Duration::ZERO);
        assert_eq!(resume_from_ticks(Some(0)), Duration::ZERO);
        assert_eq!(resume_from_ticks(Some(-5)), Duration::ZERO);
        assert_eq!(
            resume_from_ticks(Some(12_000_000)),
            Duration::from_millis(1200)
        );
    }

    #[test]
    fn duration_to_ticks_roundtrip() {
        assert_eq!(duration_to_ticks(Duration::ZERO), 0);
        assert_eq!(duration_to_ticks(Duration::from_secs(1)), 10_000_000);
        assert_eq!(
            duration_to_ticks(Duration::from_nanos(1_500)),
            15,
            "不足一个 tick 的部分应当截断"
        );
        for secs in [0, 1, 60, 3600, 98_160] {
            let duration = Duration::from_secs(secs);
            assert_eq!(
                ticks_to_duration(duration_to_ticks(duration)),
                Some(duration)
            );
        }
    }

    #[test]
    fn language_normalization() {
        assert_eq!(normalize_language(Some("eng")).as_str(), "en");
        assert_eq!(normalize_language(Some("ENG")).as_str(), "en");
        assert_eq!(normalize_language(Some("chi")).as_str(), "zh");
        assert_eq!(normalize_language(Some("zho")).as_str(), "zh");
        assert_eq!(normalize_language(Some("zho-Hant")).as_str(), "zh-Hant");
        assert_eq!(
            normalize_language(Some("ZH-Hans-CN")).as_str(),
            "zh-Hans-CN"
        );
        assert_eq!(normalize_language(Some("en_US")).as_str(), "en-US");
        assert_eq!(normalize_language(Some("jpn")).as_str(), "ja");
        // 不认识的三字母码原样保留（小写），不要猜。
        assert_eq!(normalize_language(Some("tlh")).as_str(), "tlh");
        assert!(normalize_language(Some("und")).is_undetermined());
        assert!(normalize_language(Some("unknown")).is_undetermined());
        assert!(normalize_language(Some("   ")).is_undetermined());
        assert!(normalize_language(None).is_undetermined());
    }

    #[test]
    fn normalized_language_matches_core_language() {
        // 归一化的意义就在这里：服务端的 eng 要能和用户偏好的 en 匹配上。
        let from_server = normalize_language(Some("eng"));
        assert!(from_server.matches(&Language::english()));
        assert!(normalize_language(Some("chi")).matches(&Language::new("zh-CN")));
    }

    #[test]
    fn kind_mapping_and_api_names() {
        assert_eq!(map_kind("Movie"), Some(MediaKind::Movie));
        assert_eq!(map_kind("Series"), Some(MediaKind::Series));
        assert_eq!(map_kind("Season"), Some(MediaKind::Season));
        assert_eq!(map_kind("Episode"), Some(MediaKind::Episode));
        assert_eq!(map_kind("BoxSet"), None);
        assert_eq!(map_kind(""), None);
        for kind in [
            MediaKind::Movie,
            MediaKind::Series,
            MediaKind::Season,
            MediaKind::Episode,
        ] {
            assert_eq!(map_kind(kind_to_api(kind)), Some(kind));
        }
    }

    #[test]
    fn product_name_decides_the_server_kind() {
        assert_eq!(
            kind_from_public(Some("Jellyfin Server"), false, ServerKind::Emby),
            ServerKind::Jellyfin,
            "服务端自称比构造时的 flavor 更权威"
        );
        assert_eq!(
            kind_from_public(Some("Emby Server"), true, ServerKind::Jellyfin),
            ServerKind::Emby
        );
        assert_eq!(
            kind_from_public(Some("emby"), false, ServerKind::Jellyfin),
            ServerKind::Emby,
            "大小写不敏感"
        );
        // 对不上（第三方分支 / 改了品牌）就回退，不猜。
        for unknown in [None, Some(""), Some("   "), Some("Plex")] {
            assert_eq!(
                kind_from_public(unknown, false, ServerKind::Jellyfin),
                ServerKind::Jellyfin,
                "{unknown:?}"
            );
        }
    }

    #[test]
    fn play_method_names_match_the_server_enum() {
        assert_eq!(play_method_to_api(PlayMethod::DirectPlay), "DirectPlay");
        assert_eq!(play_method_to_api(PlayMethod::DirectStream), "DirectStream");
        assert_eq!(play_method_to_api(PlayMethod::Transcode), "Transcode");
    }

    #[test]
    fn movie_fixture_maps_every_field() {
        let raw = raw_item("item_movie");
        let item = map_item(&raw, &base()).unwrap();

        assert_eq!(item.id.as_str(), "3b6f1d2c8a9e4f5b8c7d1e2f3a4b5c6d");
        assert_eq!(item.kind, MediaKind::Movie);
        assert_eq!(item.title, "银翼杀手 2049");
        assert_eq!(item.sort_title, "Blade Runner 2049");
        assert_eq!(item.original_title.as_deref(), Some("Blade Runner 2049"));
        assert_eq!(item.year, Some(2017));
        assert!(
            item.overview
                .as_deref()
                .unwrap_or_default()
                .contains("复制人")
        );
        assert_eq!(item.genres, vec!["剧情".to_owned(), "科幻".to_owned()]);
        assert_eq!(item.runtime, Some(Duration::from_secs(9816)));
        assert_eq!(
            item.parent_id.as_ref().map(ItemId::as_str),
            Some("f1e2d3c4b5a69788796a5b4c3d2e1f00")
        );
        assert_eq!(item.index_number, None);
        assert_eq!(item.child_count, None);

        // 用户数据
        assert!(item.user_data.played);
        assert_eq!(item.user_data.play_count, 1);
        assert!(!item.user_data.favorite);
        assert_eq!(item.user_data.resume_position, Duration::ZERO);

        // 图片：绝对地址，且不带令牌。
        assert_eq!(
            item.images.poster.as_deref(),
            Some(
                "http://127.0.0.1:8096/Items/3b6f1d2c8a9e4f5b8c7d1e2f3a4b5c6d/Images/Primary?tag=d41d8cd98f00b204e9800998ecf8427e"
            )
        );
        assert_eq!(
            item.images.backdrop.as_deref(),
            Some(
                "http://127.0.0.1:8096/Items/3b6f1d2c8a9e4f5b8c7d1e2f3a4b5c6d/Images/Backdrop?tag=0f1e2d3c4b5a69788796a5b4c3d2e1f0"
            )
        );
        assert!(item.images.logo.is_none());
        assert_eq!(
            item.images.thumb.as_deref(),
            Some(
                "http://127.0.0.1:8096/Items/3b6f1d2c8a9e4f5b8c7d1e2f3a4b5c6d/Images/Thumb?tag=1a2b3c4d5e6f708192a3b4c5d6e7f809"
            )
        );
        // 版本标记：缓存键必须跟着图片地址走。
        assert_eq!(
            item.images.poster_tag.as_deref(),
            Some("d41d8cd98f00b204e9800998ecf8427e")
        );
        assert_eq!(
            item.images.backdrop_tag.as_deref(),
            Some("0f1e2d3c4b5a69788796a5b4c3d2e1f0"),
            "BackdropImageTags[0]"
        );
        for url in [
            item.images.poster.as_deref(),
            item.images.backdrop.as_deref(),
            item.images.thumb.as_deref(),
        ] {
            let url = url.unwrap();
            assert!(!url.contains("api_key"), "图片地址不应带令牌：{url}");
        }
    }

    #[test]
    fn movie_fixture_sources_and_streams() {
        let raw = raw_item("item_movie");
        let item_id = ItemId::new(raw.id.clone().unwrap());
        let sources = map_sources(&raw.media_sources, &item_id, &prefixed_base());
        assert_eq!(sources.len(), 1);

        let source = &sources[0];
        assert_eq!(source.id.as_str(), "7d8e9f0a1b2c3d4e5f60718293a4b5c6");
        assert_eq!(source.item_id, item_id);
        assert_eq!(
            source.name.as_deref(),
            Some("Blade.Runner.2049.2017.2160p.mkv")
        );
        assert_eq!(source.container.as_deref(), Some("mkv"));
        assert_eq!(source.size, Some(32_212_254_720));
        assert_eq!(source.bitrate, Some(26_000_000));
        assert_eq!(
            source.runtime,
            Some(Duration::from_secs(9816)),
            "详情 / PlaybackInfo 的 MediaSources[].RunTimeTicks 要映射到 MediaSource::runtime"
        );
        assert_eq!(source.streams.len(), 5);

        // 视频流
        let video = source.video_stream().unwrap();
        assert_eq!(video.index, 0);
        assert_eq!(video.codec, "hevc");
        assert!(video.is_default);
        assert!(!video.is_external);
        assert!(video.language.is_undetermined(), "视频流通常不带语言");
        let details = video.video.as_ref().unwrap();
        assert_eq!(details.width, 3840);
        assert_eq!(details.height, 2160);
        assert!(details.is_uhd());
        assert_eq!(details.bit_depth, Some(10));
        assert_eq!(details.hdr, Some(HdrFormat::DolbyVision));
        assert!(!details.interlaced);
        let frame_rate = details.frame_rate.unwrap();
        assert!((frame_rate - 23.976).abs() < 0.01, "帧率={frame_rate}");

        // 音频流
        let audio: Vec<_> = source.audio_streams().collect();
        assert_eq!(audio.len(), 2);
        assert_eq!(audio[0].codec, "truehd");
        assert_eq!(audio[0].language.as_str(), "en");
        assert_eq!(
            audio[0].title.as_deref(),
            Some("English - Dolby TrueHD with Atmos - 7.1")
        );
        let audio_details = audio[0].audio.as_ref().unwrap();
        assert_eq!(audio_details.channels, Some(8));
        assert!(audio_details.is_surround());
        assert_eq!(audio_details.sample_rate, Some(48_000));
        assert_eq!(audio_details.bitrate, Some(4_500_000));
        assert_eq!(
            audio_details.profile.as_deref(),
            Some("Dolby TrueHD with Dolby Atmos")
        );

        assert_eq!(audio[1].codec, "aac");
        assert_eq!(audio[1].language.as_str(), "zh");
        assert!(!audio[1].is_default);

        // 字幕流
        let subtitles: Vec<_> = source.subtitle_streams().collect();
        assert_eq!(subtitles.len(), 2);
        assert_eq!(subtitles[0].index, 3);
        assert_eq!(subtitles[0].codec, "pgssub");
        assert!(
            subtitles[0].is_bitmap_subtitle(),
            "PGS 必须被识别成图形字幕"
        );
        assert_eq!(
            subtitles[0].subtitle.as_ref().unwrap().format.as_deref(),
            Some("pgssub")
        );
        assert!(!subtitles[0].subtitle.as_ref().unwrap().hearing_impaired);

        // 外挂字幕：地址基于基址补全，且不带令牌。
        let external = subtitles[1];
        assert!(external.is_external);
        assert!(!external.is_bitmap_subtitle());
        assert!(external.subtitle.as_ref().unwrap().hearing_impaired);
        assert_eq!(
            external.url.as_deref(),
            Some(
                "https://media.example.com/emby/Videos/3b6f1d2c8a9e4f5b8c7d1e2f3a4b5c6d/7d8e9f0a1b2c3d4e5f60718293a4b5c6/Subtitles/4/Stream.subrip"
            )
        );
        assert!(!external.url.as_deref().unwrap().contains("api_key"));
    }

    #[test]
    fn episode_fixture_uses_parent_backdrop_and_has_no_runtime() {
        let raw = raw_item("item_episode");
        let item = map_item(&raw, &base()).unwrap();

        assert_eq!(item.kind, MediaKind::Episode);
        assert_eq!(item.runtime, None, "缺 RunTimeTicks 应当是未知而不是 0");
        assert_eq!(item.index_number, Some(1));
        assert_eq!(item.child_count, None);
        assert_eq!(
            item.parent_id.as_ref().map(ItemId::as_str),
            Some("5a4b3c2d1e0f4a3b2c1d0e9f8a7b6c5d")
        );
        // 背景图来自父条目（剧集），海报来自自己。
        assert_eq!(
            item.images.backdrop.as_deref(),
            Some(
                "http://127.0.0.1:8096/Items/5a4b3c2d1e0f4a3b2c1d0e9f8a7b6c5d/Images/Backdrop?tag=9f8e7d6c5b4a39281706f5e4d3c2b1a0"
            )
        );
        assert_eq!(
            item.images.poster.as_deref(),
            Some(
                "http://127.0.0.1:8096/Items/9a8b7c6d5e4f4a3b2c1d0e9f8a7b6c5d/Images/Primary?tag=2b3c4d5e6f708192a3b4c5d6e7f80910"
            )
        );
        assert_eq!(
            item.images.poster_tag.as_deref(),
            Some("2b3c4d5e6f708192a3b4c5d6e7f80910")
        );
        // 背景图来自父条目，tag 也必须是父条目那张的——否则缓存键与地址对不上。
        assert_eq!(
            item.images.backdrop_tag.as_deref(),
            Some("9f8e7d6c5b4a39281706f5e4d3c2b1a0")
        );
        // 续播位置 12 分 34 秒 = 754 秒。
        assert_eq!(item.user_data.resume_position, Duration::from_secs(754));
        assert_eq!(item.user_data.play_count, 2);
        assert!(!item.user_data.played);
        assert!(item.user_data.favorite);
        assert!(raw.media_sources.is_empty(), "剧集详情不该带媒体源");
    }

    #[test]
    fn items_list_skips_unknown_types_and_counts_total() {
        let response: RawItemsResponse = serde_json::from_str(&fixture("items_movies")).unwrap();
        assert_eq!(response.total_record_count, Some(4));
        assert_eq!(response.items.len(), 4);

        let items = map_items(&response.items, &prefixed_base(), None);
        assert_eq!(items.len(), 3, "BoxSet 应当被跳过而不是让整页失败");
        assert_eq!(items[0].title, "银翼杀手 2049");
        assert_eq!(items[1].title, "沙丘");
        assert_eq!(items[2].title, "降临");
        // 列表响应里的 `ProductionYear` 必须进 `MediaItem::year`：
        // 以前请求不带 `Fields=ProductionYear` 时服务端给 null，
        // 本地缓存全库年份为空 →「发行年份从新到旧」排序退化成标题序。
        assert_eq!(
            items.iter().map(|item| item.year).collect::<Vec<_>>(),
            vec![Some(2017), Some(2021), Some(2016)]
        );
        assert!(items.iter().all(|item| item.kind == MediaKind::Movie));
        // 带路径前缀的基址也要拼对。
        assert!(
            items[0]
                .images
                .poster
                .as_deref()
                .unwrap()
                .starts_with("https://media.example.com/emby/Items/")
        );
    }

    #[test]
    fn single_item_of_unknown_type_is_an_error() {
        let raw: RawItem =
            serde_json::from_str(r#"{"Id":"x","Name":"合辑","Type":"BoxSet"}"#).unwrap();
        let error = map_item(&raw, &base()).expect_err("未知类型必须明确报错");
        assert!(matches!(error, ProviderError::InvalidResponse(_)));
        assert_eq!(
            error.to_string(),
            "服务端响应不合规：条目的媒体类型不在支持范围内"
        );
    }

    #[test]
    fn item_without_id_or_type_is_an_error() {
        let no_id: RawItem = serde_json::from_str(r#"{"Name":"x","Type":"Movie"}"#).unwrap();
        assert!(matches!(
            map_item(&no_id, &base()),
            Err(ProviderError::InvalidResponse("条目缺少 id"))
        ));

        let no_type: RawItem = serde_json::from_str(r#"{"Id":"a","Name":"x"}"#).unwrap();
        assert!(matches!(
            map_item(&no_type, &base()),
            Err(ProviderError::InvalidResponse("条目缺少类型"))
        ));
    }

    #[test]
    fn missing_optional_fields_fall_back() {
        let raw: RawItem =
            serde_json::from_str(r#"{"Id":"a","Type":"Movie","Name":"无元数据"}"#).unwrap();
        let item = map_item(&raw, &base()).unwrap();
        assert_eq!(item.title, "无元数据");
        assert_eq!(item.sort_title, "无元数据", "sort_title 缺失时用 title");
        assert_eq!(item.original_title, None);
        assert_eq!(item.year, None);
        assert_eq!(item.overview, None);
        assert!(item.genres.is_empty());
        assert_eq!(item.runtime, None);
        assert_eq!(item.parent_id, None);
        assert_eq!(item.images, ImageSet::default());
        assert_eq!(item.user_data, UserData::default());

        // 没有 Name 时用 id 当标题，不要给空串。
        let unnamed: RawItem = serde_json::from_str(r#"{"Id":"b","Type":"Series"}"#).unwrap();
        assert_eq!(map_item(&unnamed, &base()).unwrap().title, "b");
    }

    #[test]
    fn year_out_of_range_is_dropped_not_truncated() {
        let raw: RawItem =
            serde_json::from_str(r#"{"Id":"a","Type":"Movie","ProductionYear":120000}"#).unwrap();
        assert_eq!(map_item(&raw, &base()).unwrap().year, None);
        let zero: RawItem =
            serde_json::from_str(r#"{"Id":"a","Type":"Movie","ProductionYear":0}"#).unwrap();
        assert_eq!(map_item(&zero, &base()).unwrap().year, None);
    }

    #[test]
    fn sources_without_id_are_skipped() {
        let raw: Vec<RawMediaSource> = serde_json::from_str(
            r#"[{"Name":"没有 id"},{"Id":"ok","Container":"mkv"},{"Id":"","Container":"mp4"}]"#,
        )
        .unwrap();
        let sources = map_sources(&raw, &ItemId::new("item"), &base());
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id.as_str(), "ok");
        assert_eq!(sources[0].item_id, ItemId::new("item"));
        assert!(sources[0].streams.is_empty(), "没给流就是空，不是错误");
    }

    #[test]
    fn empty_media_sources_yield_empty_vec() {
        let raw: Vec<RawMediaSource> = Vec::new();
        assert!(map_sources(&raw, &ItemId::new("item"), &base()).is_empty());

        // PlaybackInfo 空响应同样只是"没有资源"。
        let info: RawPlaybackInfo = serde_json::from_str(r#"{"MediaSources":[]}"#).unwrap();
        assert!(info.media_sources.is_empty());
    }

    #[test]
    fn empty_stream_list_and_missing_video_stream() {
        let only_audio: Vec<RawMediaStream> = serde_json::from_str(
            r#"[{"Type":"Audio","Index":1,"Codec":"aac","Language":"eng","Channels":2},
                {"Type":"EmbeddedImage","Index":2},
                {"Type":"Data","Index":3},
                {"Type":"Audio"}]"#,
        )
        .unwrap();
        let streams = map_streams(&only_audio, &base());
        assert_eq!(streams.len(), 1, "缺 Index / 非音视频字幕的流应当被跳过");
        assert_eq!(streams[0].kind, StreamKind::Audio);

        let source = MediaSource {
            id: SourceId::new("s"),
            item_id: ItemId::new("i"),
            name: None,
            container: None,
            size: None,
            bitrate: None,
            runtime: None,
            streams,
        };
        assert!(source.video_stream().is_none());
        assert!(map_streams(&[], &base()).is_empty());
    }

    #[test]
    fn video_stream_without_dimensions_keeps_stream() {
        let raw: Vec<RawMediaStream> =
            serde_json::from_str(r#"[{"Type":"Video","Index":0,"Codec":"HEVC","BitDepth":10}]"#)
                .unwrap();
        let streams = map_streams(&raw, &base());
        assert_eq!(streams.len(), 1);
        let details = streams[0].video.as_ref().unwrap();
        assert_eq!(details.width, 0);
        assert_eq!(details.height, 0);
        assert!(!details.is_uhd());
        assert_eq!(details.pixels(), 0);
        assert_eq!(details.bit_depth, Some(10));
        assert_eq!(details.hdr, None);
        assert_eq!(streams[0].codec, "hevc", "编码名统一小写");
    }

    #[test]
    fn hdr_variants() {
        fn hdr(json: &str) -> Option<HdrFormat> {
            let raw: RawMediaStream = serde_json::from_str(json).unwrap();
            map_hdr(&raw)
        }
        assert_eq!(hdr(r#"{"VideoRangeType":"SDR"}"#), None);
        assert_eq!(hdr(r#"{"VideoRangeType":"HDR10"}"#), Some(HdrFormat::Hdr10));
        assert_eq!(
            hdr(r#"{"VideoRangeType":"HDR10Plus"}"#),
            Some(HdrFormat::Hdr10Plus)
        );
        assert_eq!(
            hdr(r#"{"VideoRangeType":"DOVI"}"#),
            Some(HdrFormat::DolbyVision)
        );
        assert_eq!(
            hdr(r#"{"VideoRangeType":"DOVIWithEL"}"#),
            Some(HdrFormat::DolbyVision)
        );
        assert_eq!(hdr(r#"{"VideoRangeType":"HLG"}"#), Some(HdrFormat::Hlg));
        assert_eq!(
            hdr(r#"{"VideoRangeType":"SomeFutureFormat"}"#),
            Some(HdrFormat::Other("SOMEFUTUREFORMAT".into()))
        );
        // Emby 只有 VideoRange。
        assert_eq!(
            hdr(r#"{"VideoRange":"HDR"}"#),
            Some(HdrFormat::Other("HDR".into()))
        );
        assert_eq!(hdr(r#"{"VideoRange":"SDR"}"#), None);
        assert_eq!(hdr(r#"{}"#), None);
        // VideoRangeType 优先于 VideoRange。
        assert_eq!(
            hdr(r#"{"VideoRangeType":"HDR10","VideoRange":"HDR"}"#),
            Some(HdrFormat::Hdr10)
        );
    }

    #[test]
    fn external_stream_with_absolute_url_is_kept_as_is() {
        let raw: Vec<RawMediaStream> = serde_json::from_str(
            r#"[{"Type":"Subtitle","Index":5,"Codec":"subrip","IsExternal":true,
                 "DeliveryUrl":"https://subs.example.com/a.srt"}]"#,
        )
        .unwrap();
        let streams = map_streams(&raw, &prefixed_base());
        assert_eq!(
            streams[0].url.as_deref(),
            Some("https://subs.example.com/a.srt")
        );

        // 非 http(s) 的 DeliveryUrl 直接丢掉，不要让播放器去解读它。
        let weird: Vec<RawMediaStream> = serde_json::from_str(
            r#"[{"Type":"Subtitle","Index":6,"DeliveryUrl":"file:///etc/passwd"}]"#,
        )
        .unwrap();
        assert_eq!(map_streams(&weird, &base())[0].url, None);
    }

    #[test]
    fn libraries_map_and_skip_unsupported_collections() {
        let response: RawItemsResponse = serde_json::from_str(&fixture("views")).unwrap();
        let libraries: Vec<Library> = response.items.iter().filter_map(map_library).collect();
        assert_eq!(libraries.len(), 2, "音乐库不在本层支持范围内");
        assert_eq!(libraries[0].id.as_str(), "a1b2c3d4e5f60718293a4b5c6d7e8f90");
        assert_eq!(libraries[0].name, "电影");
        assert_eq!(libraries[0].kind, MediaKind::Movie);
        assert_eq!(
            libraries[0].collection_type.as_deref(),
            Some("movies"),
            "服务端原值要留给 CLI / 界面"
        );
        assert_eq!(libraries[0].item_count, None);
        assert_eq!(libraries[1].name, "剧集");
        assert_eq!(libraries[1].kind, MediaKind::Series);
        assert_eq!(libraries[1].collection_type.as_deref(), Some("tvshows"));
    }

    #[test]
    fn image_url_encodes_and_keeps_prefix() {
        let url = image_url(&prefixed_base(), "abc", ImageKind::Backdrop, Some("t=1")).unwrap();
        assert_eq!(
            url,
            "https://media.example.com/emby/Items/abc/Images/Backdrop?tag=t%3D1"
        );
        let no_tag = image_url(&base(), "abc", ImageKind::Logo, None).unwrap();
        assert_eq!(no_tag, "http://127.0.0.1:8096/Items/abc/Images/Logo");
    }

    #[test]
    fn user_data_defaults_and_clamping() {
        assert_eq!(map_user_data(None), UserData::default());
        let raw: RawUserData =
            serde_json::from_str(r#"{"Played":true,"PlayCount":-3,"IsFavorite":true}"#).unwrap();
        let data = map_user_data(Some(&raw));
        assert!(data.played);
        assert!(data.favorite);
        assert_eq!(data.play_count, 0, "负的播放次数不能变成巨大数字");
        assert_eq!(data.resume_position, Duration::ZERO);
    }

    #[test]
    fn auth_fixtures_parse() {
        let jellyfin: RawAuthenticationResult =
            serde_json::from_str(&fixture("auth_jellyfin")).unwrap();
        assert_eq!(
            jellyfin.access_token.as_deref(),
            Some("8f3a1c9d7e5b2a46c0d9e8f7a6b5c4d3")
        );
        assert_eq!(
            jellyfin.server_id.as_deref(),
            Some("6f0c5a1e4b7d4b6f9a2c3d8e1f5b7a90")
        );
        assert_eq!(
            jellyfin.user.as_ref().and_then(|user| user.name.as_deref()),
            Some("alice")
        );

        let emby: RawAuthenticationResult = serde_json::from_str(&fixture("auth_emby")).unwrap();
        assert_eq!(
            emby.access_token.as_deref(),
            Some("d4c3b2a1908f7e6d5c4b3a29180f7e6d")
        );
        assert_eq!(
            emby.user.as_ref().and_then(|user| user.name.as_deref()),
            Some("bob")
        );
    }

    #[test]
    fn playback_info_fixture_parses() {
        let info: RawPlaybackInfo = serde_json::from_str(&fixture("playback_info")).unwrap();
        assert_eq!(info.media_sources.len(), 1);
        let sources = map_sources(&info.media_sources, &ItemId::new("i"), &base());
        assert_eq!(sources[0].container.as_deref(), Some("mkv"));
        assert_eq!(sources[0].streams.len(), 5);
    }

    #[test]
    fn absolute_delivery_url_with_userinfo_is_stripped() {
        // 服务端返回的绝对地址可能带 `user:pass@`；带着它会把口令带进模型、
        // 播放器命令行、截图与日志。保留地址（同一个 host / path）但不保留凭证。
        let raw: Vec<RawMediaStream> = serde_json::from_str(
            r#"[{"Type":"Subtitle","Index":5,"Codec":"subrip","IsExternal":true,
                 "DeliveryUrl":"https://user:pass@subs.example.com/a.srt"}]"#,
        )
        .unwrap();
        let streams = map_streams(&raw, &prefixed_base());
        let url = streams[0].url.as_deref().expect("地址应当保留");
        assert_eq!(url, "https://subs.example.com/a.srt");
        assert!(!url.contains("user"), "用户名必须被剥离：{url}");
        assert!(!url.contains("pass"), "密码必须被剥离：{url}");

        assert_eq!(
            absolute_url(&base(), "http://u:p@cdn.example/a.srt"),
            Some("http://cdn.example/a.srt".to_owned())
        );
        // 没有 userinfo 的地址原样保留（含端口与查询串）。
        assert_eq!(
            absolute_url(&base(), "https://cdn.example:8443/a.srt?k=v"),
            Some("https://cdn.example:8443/a.srt?k=v".to_owned())
        );
    }

    #[test]
    fn oversized_array_fields_are_truncated() {
        // 恶意服务端用超大数组打爆内存：每个数组字段都有上限，超限就截断。
        let genres: Vec<String> = (0..MAX_GENRES * 3).map(|i| format!("g{i}")).collect();
        let raw: RawItem = serde_json::from_value(serde_json::json!({
            "Id": "a",
            "Type": "Movie",
            "Name": "x",
            "Genres": genres,
        }))
        .unwrap();
        assert_eq!(raw.genres.len(), MAX_GENRES, "类型标签必须被截断");
        let item = map_item(&raw, &base()).unwrap();
        assert_eq!(item.genres.len(), MAX_GENRES);
        assert_eq!(item.genres[0], "g0", "截断保留前面的元素");
    }

    #[test]
    fn oversized_stream_and_source_lists_are_truncated() {
        let streams: Vec<serde_json::Value> = (0..MAX_MEDIA_STREAMS * 2)
            .map(|i| {
                serde_json::json!({
                    "Type": "Audio",
                    "Index": i,
                    "Codec": "aac",
                })
            })
            .collect();
        // 上限在 DTO 字段上（`MediaStreams` / `MediaSources`），所以要通过容器反序列化。
        let source: RawMediaSource = serde_json::from_value(serde_json::json!({
            "Id": "s0",
            "MediaStreams": streams,
        }))
        .expect("超大流列表应当能解析");
        assert_eq!(
            source.media_streams.len(),
            MAX_MEDIA_STREAMS,
            "流列表必须被截断"
        );
        assert_eq!(
            map_streams(&source.media_streams, &base()).len(),
            MAX_MEDIA_STREAMS
        );

        // 媒体源列表的上限同样在 DTO 字段上；两处字段（PlaybackInfo 与条目详情）都要过。
        let sources = serde_json::Value::Array(
            (0..MAX_MEDIA_SOURCES + 5)
                .map(|i| serde_json::json!({ "Id": format!("s{i}"), "Container": "mkv" }))
                .collect(),
        );

        let info: RawPlaybackInfo = serde_json::from_value(serde_json::json!({
            "MediaSources": sources.clone(),
        }))
        .expect("超大源列表应当能解析");
        assert_eq!(
            info.media_sources.len(),
            MAX_MEDIA_SOURCES,
            "媒体源列表必须被截断"
        );
        assert_eq!(
            map_sources(&info.media_sources, &ItemId::new("item"), &base()).len(),
            MAX_MEDIA_SOURCES
        );

        let item: RawItem = serde_json::from_value(serde_json::json!({
            "Id": "a",
            "Type": "Movie",
            "Name": "x",
            "MediaSources": sources,
        }))
        .expect("条目详情里的超大源列表应当能解析");
        assert_eq!(item.media_sources.len(), MAX_MEDIA_SOURCES);
    }

    #[test]
    fn oversized_image_tag_sets_are_truncated() {
        let tags: serde_json::Map<String, serde_json::Value> = (0..MAX_IMAGE_TAGS * 2)
            .map(|i| (format!("Tag{i}"), serde_json::Value::String("t".to_owned())))
            .collect();
        let raw: RawItem = serde_json::from_value(serde_json::json!({
            "Id": "a",
            "Type": "Movie",
            "Name": "x",
            "ImageTags": tags,
            "BackdropImageTags": (0..MAX_IMAGE_TAGS * 2)
                .map(|i| format!("b{i}"))
                .collect::<Vec<_>>(),
            "ParentBackdropImageTags": (0..MAX_IMAGE_TAGS * 2)
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>(),
        }))
        .unwrap();
        assert_eq!(raw.image_tags.len(), MAX_IMAGE_TAGS);
        assert_eq!(raw.backdrop_image_tags.len(), MAX_IMAGE_TAGS);
        assert_eq!(raw.parent_backdrop_image_tags.len(), MAX_IMAGE_TAGS);
    }

    #[test]
    fn duplicate_stream_index_keeps_the_first_stream() {
        // 同一个 Index 出现两次会让下游按 index 选轨时歧义（可能选到字幕而不是音轨）。
        let raw: Vec<RawMediaStream> = serde_json::from_str(
            r#"[{"Type":"Audio","Index":1,"Codec":"aac","Language":"eng"},
                {"Type":"Subtitle","Index":1,"Codec":"subrip"},
                {"Type":"Audio","Index":2,"Codec":"eac3"},
                {"Type":"Audio","Index":2,"Codec":"ac3"}]"#,
        )
        .unwrap();
        let streams = map_streams(&raw, &base());
        assert_eq!(streams.len(), 2, "重复 index 应当被去重");
        assert_eq!(streams[0].index, 1);
        assert_eq!(streams[0].kind, StreamKind::Audio, "保留第一条");
        assert_eq!(streams[0].codec, "aac");
        assert_eq!(streams[1].index, 2);
        assert_eq!(streams[1].codec, "eac3");
    }
}
