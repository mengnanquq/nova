// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! [`MediaProvider`](crate::MediaProvider) 的入参与返回值。
//!
//! 这些类型只描述"向服务端问什么"与"服务端回来了什么形状的一页"，
//! 具体字段全部落到 [`nova_core`] 的统一模型上。

use std::fmt;

use nova_core::{ItemId, MediaKind, SourceId};

/// 条目列表的排序字段。
///
/// 只列出浏览界面真正会用到的几种；服务端支持更多，但它们属于"由界面决定"的细节，
/// 需要时再往这里加，而不是先把整个 API 抄一遍。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortBy {
    /// 按排序标题（`SortName`）。
    SortName,
    /// 按入库时间。
    DateCreated,
    /// 按最近播放时间。
    DatePlayed,
    /// 按首播日期。
    PremiereDate,
    /// 按评分。
    CommunityRating,
    /// 随机（"随便看看"）。
    Random,
}

impl SortBy {
    /// 服务端查询参数 `SortBy` 的取值。
    pub fn as_api(self) -> &'static str {
        match self {
            Self::SortName => "SortName",
            Self::DateCreated => "DateCreated",
            Self::DatePlayed => "DatePlayed",
            Self::PremiereDate => "PremiereDate",
            Self::CommunityRating => "CommunityRating",
            Self::Random => "Random",
        }
    }
}

/// 条目查询条件。
///
/// 所有字段都是可选的：默认是"从服务端根目录开始、不限类型、按服务端默认顺序"。
/// 分页沿用服务端的 `StartIndex` / `Limit` 语义：`limit` 为 `None` 表示不分页。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemQuery {
    /// 父条目（媒体库、剧集、季）。`None` 表示不按父条目过滤。
    pub parent: Option<ItemId>,
    /// 只要这些类型；空表示不限类型。
    pub kinds: Vec<MediaKind>,
    /// 搜索关键字。
    pub search: Option<String>,
    /// 排序字段。
    pub sort: Option<SortBy>,
    /// 起始偏移（0 起）。
    pub start: Option<u32>,
    /// 最多返回多少条。
    pub limit: Option<u32>,
}

impl ItemQuery {
    /// 空查询。
    pub fn new() -> Self {
        Self::default()
    }

    /// 只查某个父条目（媒体库 / 剧集 / 季）下的条目。
    pub fn in_parent(parent: ItemId) -> Self {
        Self {
            parent: Some(parent),
            ..Self::default()
        }
    }

    /// 限定条目类型。
    pub fn with_kinds(mut self, kinds: impl IntoIterator<Item = MediaKind>) -> Self {
        self.kinds = kinds.into_iter().collect();
        self
    }

    /// 搜索关键字。
    pub fn with_search(mut self, term: impl Into<String>) -> Self {
        self.search = Some(term.into());
        self
    }

    /// 指定排序。
    pub fn with_sort(mut self, sort: SortBy) -> Self {
        self.sort = Some(sort);
        self
    }

    /// 分页。
    pub fn paged(mut self, start: u32, limit: u32) -> Self {
        self.start = Some(start);
        self.limit = Some(limit);
        self
    }
}

/// 一页结果。
///
/// `total` 是服务端报告的**过滤后总数**（`TotalRecordCount`），用于滚动条与"共 N 条"，
/// 不等于 `items.len()`。服务端没给总数时退化为本页条数。
#[derive(Clone, Debug, PartialEq)]
pub struct Page<T> {
    /// 本页条目。
    pub items: Vec<T>,
    /// 服务端报告的条目总数。
    pub total: u32,
    /// 本页起始偏移（回显请求里的 `start`，未指定则为 0）。
    pub start: u32,
    /// 本页最多条数（回显请求里的 `limit`，未指定则为 0，表示"服务端决定"）。
    pub limit: u32,
}

impl<T> Page<T> {
    /// 是否还有下一页。
    pub fn has_more(&self) -> bool {
        self.start
            .saturating_add(u32::try_from(self.items.len()).unwrap_or(u32::MAX))
            < self.total
    }
}

/// 媒体库（服务端上的一个顶层视图）。
///
/// 目前只暴露电影库与剧集库：`nova_core::MediaKind` 还没有音乐 / 照片类型，
/// 把它们硬塞进 `Movie` 会让界面显示错误的图标与详情页。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    /// 库标识（服务端作用域）。
    pub id: ItemId,
    /// 显示名。
    pub name: String,
    /// 库里装的是什么：电影库为 [`MediaKind::Movie`]，剧集库为 [`MediaKind::Series`]。
    pub kind: MediaKind,
    /// 服务端原始 `CollectionType`（`movies` / `tvshows` / `music` …）。
    ///
    /// `kind` 是归一化后的结果，信息有损（音乐库、照片库都拿不到 `kind`）；
    /// 界面做筛选、拼子库查询时需要原值。
    pub collection_type: Option<String>,
    /// 条目数量；服务端在列表接口上通常不提供，此时为 `None`。
    pub item_count: Option<u32>,
}

/// 图片种类。取值为服务端 `Images/{Type}` 路径段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKind {
    /// 竖版海报。
    Poster,
    /// 横版背景图。
    Backdrop,
    /// 标题 logo。
    Logo,
    /// 缩略图 / 截图。
    Thumb,
}

impl ImageKind {
    /// 服务端路径里的类型名。
    pub fn as_api(self) -> &'static str {
        match self {
            Self::Poster => "Primary",
            Self::Backdrop => "Backdrop",
            Self::Logo => "Logo",
            Self::Thumb => "Thumb",
        }
    }
}

/// 一次图片请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRequest {
    /// 条目。
    pub item: ItemId,
    /// 图片种类。
    pub kind: ImageKind,
    /// 期望宽度（服务端按此缩放，保持宽高比）。
    pub width: Option<u32>,
    /// 图片版本标记（`ImageTags` 里的值），用于让缓存随服务端图片更新而失效。
    pub tag: Option<String>,
}

impl ImageRequest {
    /// 构造请求。
    pub fn new(item: ItemId, kind: ImageKind) -> Self {
        Self {
            item,
            kind,
            width: None,
            tag: None,
        }
    }

    /// 指定宽度。
    pub fn with_width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }

    /// 指定版本标记。
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }
}

/// 一个可播放的直链。
///
/// **地址查询串里带着访问令牌**（播放器只接受一条裸地址，无法附加认证头），
/// 所以这个类型的 `Debug` 与 `Display` 都会把查询串替换成 `?<redacted>`。
/// 取真实地址必须显式调用 [`StreamTarget::url`]。
#[derive(Clone, PartialEq, Eq)]
pub struct StreamTarget {
    url: String,
    item_id: ItemId,
    source_id: SourceId,
    plaintext: bool,
}

impl StreamTarget {
    /// 由适配层构造。
    pub(crate) fn new(url: String, item_id: ItemId, source_id: SourceId, plaintext: bool) -> Self {
        Self {
            url,
            item_id,
            source_id,
            plaintext,
        }
    }

    /// 真实播放地址。
    ///
    /// 这是本 crate 里唯一会返回含令牌字符串的方法：**不要**把它写进日志、
    /// 错误信息或崩溃报告。需要记录时用 `{:?}` 打印整个结构体（查询串会被抹掉）。
    pub fn url(&self) -> &str {
        &self.url
    }

    /// 条目。
    pub fn item_id(&self) -> &ItemId {
        &self.item_id
    }

    /// 资源。
    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// 服务器地址是否为明文 HTTP（局域网自建场景），界面应据此提示用户。
    pub fn is_plaintext(&self) -> bool {
        self.plaintext
    }
}

impl fmt::Debug for StreamTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamTarget")
            .field("item_id", &self.item_id)
            .field("source_id", &self.source_id)
            .field("plaintext", &self.plaintext)
            .field("url", &format_args!("{}", redact_query(&self.url)))
            .finish()
    }
}

impl fmt::Display for StreamTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact_query(&self.url))
    }
}

/// 把地址的查询串（以及片段）替换成 `<redacted>`。
///
/// 播放直链里的 `api_key` 就在查询串里；片段里也不允许出现任何东西。
pub(crate) fn redact_query(url: &str) -> String {
    let head = url.split(['?', '#']).next().unwrap_or(url);
    if head.len() == url.len() {
        head.to_owned()
    } else {
        format!("{head}?<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> StreamTarget {
        StreamTarget::new(
            url.to_owned(),
            ItemId::new("item-1"),
            SourceId::new("src-1"),
            true,
        )
    }

    #[test]
    fn stream_target_debug_and_display_redact_the_query() {
        let target =
            target("http://nas.local:8096/Videos/i/stream?Static=true&api_key=SECRET-TOKEN");
        let debug = format!("{target:?}");
        assert!(!debug.contains("SECRET-TOKEN"), "Debug 泄漏令牌：{debug}");
        assert!(!debug.contains("api_key"), "Debug 泄漏查询串：{debug}");
        assert!(debug.contains("http://nas.local:8096/Videos/i/stream?<redacted>"));
        assert!(debug.contains("item-1"));
        assert!(debug.contains("plaintext: true"));

        let display = target.to_string();
        assert!(!display.contains("SECRET-TOKEN"));
        assert_eq!(display, "http://nas.local:8096/Videos/i/stream?<redacted>");

        // 真实地址仍然可取到。
        assert!(target.url().contains("SECRET-TOKEN"));
        assert_eq!(target.item_id().as_str(), "item-1");
        assert_eq!(target.source_id().as_str(), "src-1");
        assert!(target.is_plaintext());
    }

    #[test]
    fn redaction_handles_missing_query_and_fragment() {
        assert_eq!(redact_query("http://a/b"), "http://a/b");
        assert_eq!(redact_query("http://a/b?x=1"), "http://a/b?<redacted>");
        assert_eq!(redact_query("http://a/b#frag"), "http://a/b?<redacted>");
        assert_eq!(redact_query("http://a/b?x=1#frag"), "http://a/b?<redacted>");
    }

    #[test]
    fn item_query_builders() {
        let query = ItemQuery::in_parent(ItemId::new("lib"))
            .with_kinds([MediaKind::Movie])
            .with_search("blade")
            .with_sort(SortBy::PremiereDate)
            .paged(40, 20);
        assert_eq!(query.parent, Some(ItemId::new("lib")));
        assert_eq!(query.kinds, vec![MediaKind::Movie]);
        assert_eq!(query.search.as_deref(), Some("blade"));
        assert_eq!(query.sort, Some(SortBy::PremiereDate));
        assert_eq!(query.start, Some(40));
        assert_eq!(query.limit, Some(20));
        assert_eq!(ItemQuery::new(), ItemQuery::default());
    }

    #[test]
    fn page_has_more_accounts_for_offset_and_total() {
        let page = Page {
            items: vec![1, 2, 3],
            total: 10,
            start: 0,
            limit: 3,
        };
        assert!(page.has_more());

        let last = Page {
            items: vec![9, 10],
            total: 10,
            start: 8,
            limit: 3,
        };
        assert!(!last.has_more());

        // 服务端把 total 报成 0 时不要谎称还有下一页。
        let unknown = Page {
            items: vec![1],
            total: 0,
            start: 0,
            limit: 0,
        };
        assert!(!unknown.has_more());
    }

    #[test]
    fn api_names() {
        assert_eq!(ImageKind::Poster.as_api(), "Primary");
        assert_eq!(ImageKind::Backdrop.as_api(), "Backdrop");
        assert_eq!(ImageKind::Thumb.as_api(), "Thumb");
        assert_eq!(SortBy::SortName.as_api(), "SortName");
        assert_eq!(SortBy::Random.as_api(), "Random");
    }

    #[test]
    fn image_request_builders() {
        let request = ImageRequest::new(ItemId::new("i"), ImageKind::Poster)
            .with_width(600)
            .with_tag("abc");
        assert_eq!(request.width, Some(600));
        assert_eq!(request.tag.as_deref(), Some("abc"));
        assert_eq!(request.kind, ImageKind::Poster);
    }
}
