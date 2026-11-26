// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 各子命令的实现。
//!
//! 每个方法返回 [`CommandOutput`]：一份给人看的文本 + 一份 `--json` 用的结构。
//! 数据来源全部是 `nova-provider`（会话恢复由 `ProviderConfig::with_user_id` 负责），
//! 本地 SQLite 只做两件事：记住服务器 / 凭证键名，以及缓存条目与资源
//! （`items --cached`、`info` 优先读缓存）。

use std::io::Read;
use std::time::Duration;

use nova_core::{AuthState, ItemId, PlayMethod, PlaybackProgress};
use nova_provider::{
    DeviceInfo, Flavor, ItemQuery, MediaProvider, Provider, ProviderConfig, ProviderError,
};
use nova_secrets::SecretString;
use serde_json::json;

use crate::App;
use crate::args::LoginArgs;
use crate::error::CliError;
use crate::output::{
    CommandOutput, duration_text, item_json, item_row, kind_text, library_json, method_json,
    method_text, or_dash, or_empty_tag, source_json, stream_line, table, yes_no,
};

/// 一条数据是从哪来的：本地缓存还是服务端。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// 本地 SQLite 缓存。
    Cache,
    /// 本次从服务端取的（并且已经写回缓存）。
    Server,
}

impl Origin {
    fn text(self) -> &'static str {
        match self {
            Self::Cache => "本地缓存",
            Self::Server => "服务端",
        }
    }

    fn json(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::Server => "server",
        }
    }
}

/// `nova items` 不带 `--limit` 时每页取多少条。
///
/// 与桌面端刷新每页（`nova_app::refresh::ITEMS_PER_PAGE`）一致：以前不带 `--limit`
/// 时把服务端返回的整库（真实数据目录曾被撑到 4 万行，含 `parent_id = NULL`
/// 的季 / 集孤儿行）原样写进 nova.db。缓存写入量必须有上限，而"翻完整个库"
/// 是 App 刷新的活（它会在完整拉完后清理服务端已删除的条目）。
pub const DEFAULT_ITEMS_LIMIT: u32 = 200;

impl App {
    // -----------------------------------------------------------------
    // login
    // -----------------------------------------------------------------

    /// `nova login`：先校验地址，再读密码，最后交给 [`Self::login_with_password`]。
    ///
    /// 顺序是刻意的：地址不合规、或明文连接没被显式允许时，**不应该先问密码**。
    pub async fn login(&self, options: LoginArgs) -> Result<CommandOutput, CliError> {
        let base_url = prepare_login(&options)?;
        let password = read_password(options.password_stdin)?;
        self.complete_login(options, &base_url, password).await
    }

    /// 与 [`Self::login`] 相同，但密码由调用方给出。
    ///
    /// 命令行永远走 [`Self::login`]（密码只从标准输入进来）；拆出这个方法是为了让
    /// 集成测试能在同一进程里跑完整的 `login` 流程，而不必伪造标准输入。
    pub async fn login_with_password(
        &self,
        options: LoginArgs,
        password: SecretString,
    ) -> Result<CommandOutput, CliError> {
        let base_url = prepare_login(&options)?;
        self.complete_login(options, &base_url, password).await
    }

    async fn complete_login(
        &self,
        options: LoginArgs,
        base_url: &str,
        password: SecretString,
    ) -> Result<CommandOutput, CliError> {
        // 同一地址复用已有的服务器 id：凭证键名与服务端会话都保持稳定。
        let existing = self
            .db
            .servers()?
            .into_iter()
            .find(|server| server.info.base_url.eq_ignore_ascii_case(base_url));
        let server_id = existing
            .as_ref()
            .map(|server| server.info.id.clone())
            .unwrap_or_else(|| crate::derive_server_id(base_url));

        let config = ProviderConfig::new(server_id.clone(), base_url).with_device(DeviceInfo::new(
            "Nova CLI",
            format!("nova-cli-{}", server_id.as_str()),
        ));

        // `public_info()` 不需要令牌，先用它判断对面是 Jellyfin 还是 Emby
        // （`ProductName`）；只有确认是 Emby 才需要用另一个 flavor 重建 provider。
        let probe = Provider::new(Flavor::Jellyfin, config.clone(), self.credentials.clone())
            .map_err(crate::error::provider)?;
        let product_kind = match probe.public_info().await {
            Ok(info) => info.kind,
            Err(error) => {
                tracing::debug!(%error, "公开信息不可用，先按 Jellyfin 处理");
                nova_core::ServerKind::Jellyfin
            }
        };
        let flavor = match product_kind {
            nova_core::ServerKind::Emby => Flavor::Emby,
            nova_core::ServerKind::Jellyfin => Flavor::Jellyfin,
        };
        let provider = if flavor == Flavor::Jellyfin {
            probe
        } else {
            Provider::new(flavor, config, self.credentials.clone())
                .map_err(crate::error::provider)?
        };

        provider
            .authenticate(&options.user, &password)
            .await
            .map_err(crate::error::provider)?;
        // 认证后 provider 里已经有服务端名、版本与用户标识（`authenticate()` 内部
        // 顺带问了 `/System/Info/Public`）。
        let info = provider.server_info();

        // 登录时顺手列一次媒体库：既验证令牌真的能用，也把 `collection_type`
        // 一并展示出来（`login` 是唯一能"一次看到账面上有什么"的地方）。
        let libraries = match provider.libraries().await {
            Ok(libraries) => Some(libraries),
            Err(error) => {
                tracing::warn!(error = %error, "登录成功，但列媒体库失败");
                None
            }
        };

        self.persist_info(&provider)?;

        let credential_key = provider.credential_key().to_owned();
        let user_name = info.auth.user_name().unwrap_or(&options.user).to_owned();

        let mut text = String::new();
        text.push_str("已登录\n");
        text.push_str(&format!(
            "  服务器    : {} ({}{})\n",
            info.name,
            info.kind.display_name(),
            info.version
                .as_deref()
                .map(|version| format!(" {version}"))
                .unwrap_or_default()
        ));
        text.push_str(&format!(
            "  地址      : {}{}\n",
            info.base_url,
            if info.is_plaintext() {
                "   ← 明文 HTTP（密码经未加密连接传输）"
            } else {
                ""
            }
        ));
        text.push_str(&format!(
            "  用户      : {user_name}（用户标识 {}）\n",
            or_dash(info.user_id.as_deref())
        ));
        text.push_str(&format!(
            "  服务器 id : {}（`--server` 的取值）\n",
            info.id.as_str()
        ));
        text.push_str(&format!(
            "  凭证      : {credential_key} → {}\n",
            if self.persistent {
                "系统密钥环（持久化，后续命令不必重新登录）"
            } else {
                "进程内存储（不持久化）"
            }
        ));
        text.push_str(&format!("  数据库    : {}\n", self.db_path().display()));
        match &libraries {
            Some(libraries) if libraries.is_empty() => {
                text.push_str("  媒体库    : 没有电影库或剧集库（服务器上还没建？）\n");
            }
            Some(libraries) => {
                text.push_str(&format!("  媒体库    : {} 个\n", libraries.len()));
                for library in libraries {
                    text.push_str(&format!(
                        "    {}  collection_type={}  {}  {} 条  id={}\n",
                        library.name,
                        or_dash(library.collection_type.as_deref()),
                        kind_text(library.kind),
                        or_dash(library.item_count),
                        library.id.as_str(),
                    ));
                }
            }
            None => text.push_str("  媒体库    : 列取失败（登录本身已成功）\n"),
        }
        if let Some(note) = self.credential_note() {
            text.push_str(&format!("  注意      : {note}\n"));
        }

        let output = json!({
            "server": {
                "id": info.id.as_str(),
                "name": info.name,
                "kind": info.kind.display_name(),
                "version": info.version,
                "base_url": info.base_url,
                "plaintext": info.is_plaintext(),
                "user_name": user_name,
                "user_id": info.user_id,
                "credential_key": credential_key,
            },
            "credentials": {
                "persistent": self.persistent,
                "note": self.credential_note(),
            },
            "database": self.db_path().display().to_string(),
            "libraries": libraries
                .as_ref()
                .map(|libraries| libraries.iter().map(library_json).collect::<Vec<_>>()),
        });
        Ok(CommandOutput::new(text, output))
    }

    // -----------------------------------------------------------------
    // servers
    // -----------------------------------------------------------------

    /// `nova servers`。
    pub fn servers(&self) -> Result<CommandOutput, CliError> {
        let servers = self.db.servers()?;
        let mut rows = Vec::with_capacity(servers.len());
        let mut json_servers = Vec::with_capacity(servers.len());
        for server in &servers {
            let user = Self::auth_text(&server.info.auth);
            let credential_key = match &server.info.auth {
                AuthState::Authorized { credential_key, .. } => Some(credential_key.clone()),
                _ => None,
            };
            let token_present = match &credential_key {
                Some(key) => self.credentials.get(key)?.is_some(),
                None => false,
            };
            rows.push(vec![
                server.info.id.as_str().to_owned(),
                server.info.name.clone(),
                server.info.kind.display_name().to_owned(),
                server.info.base_url.clone(),
                user.clone(),
                if server.info.is_plaintext() {
                    "明文".to_owned()
                } else {
                    "TLS".to_owned()
                },
                if token_present {
                    "有".to_owned()
                } else {
                    "无".to_owned()
                },
            ]);
            json_servers.push(json!({
                "id": server.info.id.as_str(),
                "name": server.info.name,
                "kind": server.info.kind.display_name(),
                "base_url": server.info.base_url,
                "version": server.info.version,
                "user_id": server.info.user_id,
                "user_name": server.info.auth.user_name(),
                "auth": user,
                "plaintext": server.info.is_plaintext(),
                "credential_key": credential_key,
                "token_available": token_present,
                "last_seen_at": server.last_seen_at,
            }));
        }

        let mut text = if servers.is_empty() {
            "本机还没有保存任何服务器。\n下一步：`nova login --url http://<主机>:8096 --user <用户名> --password-stdin`\n"
                .to_owned()
        } else {
            table(
                &["ID", "名称", "类型", "地址", "用户 / 状态", "连接", "令牌"],
                &rows,
            )
        };
        if let Some(note) = self.credential_note() {
            text.push_str(&format!("\n注意：{note}\n"));
            if !servers.is_empty() {
                text.push_str("  这就是上面\"令牌\"一列是\"无\"、后续命令需要重新登录的原因。\n");
            }
        }

        Ok(CommandOutput::new(
            text,
            json!({ "servers": json_servers, "credentials_persistent": self.persistent }),
        ))
    }

    // -----------------------------------------------------------------
    // libraries
    // -----------------------------------------------------------------

    /// `nova libraries`。
    pub async fn libraries(&self, server: Option<&str>) -> Result<CommandOutput, CliError> {
        let (stored, provider) = self.session(server)?;
        let libraries = provider.libraries().await.map_err(crate::error::provider)?;
        self.persist_info(&provider)?;

        let rows: Vec<Vec<String>> = libraries
            .iter()
            .map(|library| {
                vec![
                    library.id.as_str().to_owned(),
                    library.name.clone(),
                    or_dash(library.collection_type.as_deref()),
                    kind_text(library.kind).to_owned(),
                    or_dash(library.item_count),
                ]
            })
            .collect();

        let mut text = format!(
            "服务器 {}（{}）\n{}",
            stored.info.name,
            stored.info.base_url,
            if libraries.is_empty() {
                "没有媒体库（服务器上还没有建电影库或剧集库？）\n".to_owned()
            } else {
                table(&["ID", "名称", "collection_type", "类型", "条目数"], &rows)
            }
        );
        if libraries.is_empty() {
            text.push_str("提示：`nova items --library <id>` 需要媒体库 id\n");
        }

        Ok(CommandOutput::new(
            text,
            json!({
                "server": stored.info.id.as_str(),
                "libraries": libraries.iter().map(library_json).collect::<Vec<_>>(),
            }),
        ))
    }

    // -----------------------------------------------------------------
    // items
    // -----------------------------------------------------------------

    /// `nova items`。`cached = true` 时只读本地库。
    pub async fn items(
        &self,
        server: Option<&str>,
        library: &str,
        search: Option<&str>,
        limit: Option<u32>,
        cached: bool,
    ) -> Result<CommandOutput, CliError> {
        if cached {
            return self.items_from_cache(server, library, search, limit);
        }

        let (stored, provider) = self.session(server)?;
        // 不指定 `--limit` 时按页取（见 [`DEFAULT_ITEMS_LIMIT`]），不再一次把整库
        // 拉回来写进缓存。
        let limit = limit.unwrap_or(DEFAULT_ITEMS_LIMIT);
        let mut query = ItemQuery::in_parent(ItemId::new(library.to_owned()));
        if let Some(term) = search {
            query = query.with_search(term);
        }
        query = query.paged(0, limit);

        let page = match provider.items(query).await {
            Ok(page) => page,
            Err(ProviderError::Http { status: 400 }) => {
                return Err(CliError::Config(format!(
                    "服务端不接受媒体库 id `{library}`（HTTP 400）：用 `nova libraries` 确认库 id"
                )));
            }
            Err(error) => return Err(crate::error::provider(error)),
        };
        // 缓存只看电影 / 剧集，父项写成这个库——与桌面端刷新共用同一条规则
        // （`nova_storage::library_items`），不另写一份归属逻辑。
        //
        // 缓存只是加速：写失败不该让"已经拉到的这一页"作废，但要让用户知道。
        let cacheable = nova_storage::library_items(page.items.clone(), &ItemId::new(library));
        if let Err(error) = self.cache_items(&stored, &cacheable) {
            tracing::warn!(%error, "条目写入本地缓存失败");
        }
        self.persist_info(&provider)?;

        let total = page.total;
        let mut text = format!(
            "媒体库 {library}：{total} 条（服务端；本页 {} 条，其中 {} 条电影 / 剧集已缓存到本地）{}{}\n",
            page.items.len(),
            cacheable.len(),
            search
                .map(|term| format!("（搜索 `{term}`）"))
                .unwrap_or_default(),
            if page.items.is_empty() {
                "，本次没有返回条目"
            } else {
                ""
            }
        );
        if page.items.is_empty() {
            text.push_str("提示：试试换 `--search` 关键字，或用 `nova libraries` 确认库 id\n");
        } else {
            text.push_str(&table(
                &["ID", "类型", "时长", "标题"],
                &page.items.iter().map(item_row).collect::<Vec<_>>(),
            ));
            text.push_str(
                "\n时长来自列表接口，真实服务端可能不提供（显示 `-`）；`nova info <id>` 会给出详情。\n",
            );
        }

        Ok(CommandOutput::new(
            text,
            json!({
                "server": stored.info.id.as_str(),
                "library": library,
                "search": search,
                "data_source": Origin::Server.json(),
                "total": total,
                "cached_in_page": cacheable.len(),
                "items": page.items.iter().map(item_json).collect::<Vec<_>>(),
            }),
        ))
    }

    fn items_from_cache(
        &self,
        server: Option<&str>,
        library: &str,
        search: Option<&str>,
        limit: Option<u32>,
    ) -> Result<CommandOutput, CliError> {
        let stored = self.select_server(server)?;
        let library_id = ItemId::new(library.to_owned());

        // 只读本地：这里一个网络请求都不发（断网可用）。
        let mut items = self.db.children(&stored.info.id, Some(&library_id))?;
        let mut note = None;
        if items.is_empty() {
            // 实测 Jellyfin 12.4：`Items` 列表响应不带 `ParentId`，缓存里这些条目的
            // `parent_id` 是 NULL，于是"按库查"会空。再按顶层取一次，避免把一个
            // 明明同步过的库显示成空库。
            let top_level = self.db.children(&stored.info.id, None)?;
            if !top_level.is_empty() {
                items = top_level;
                note = Some(
                    "缓存里的条目没有 ParentId（服务端列表响应不提供），已按顶层列出".to_owned(),
                );
            }
        }

        if let Some(term) = search.map(str::trim).filter(|term| !term.is_empty()) {
            let needle = term.to_lowercase();
            items.retain(|item| {
                item.title.to_lowercase().contains(&needle)
                    || item.sort_key().to_lowercase().contains(&needle)
            });
        }
        let matched = items.len();
        if let Some(limit) = limit {
            items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
        let cached_total = self.db.item_count(&stored.info.id)?;

        let mut text = format!(
            "媒体库 {library}：{matched} 条（本地缓存，未联网）{}{}\n",
            search
                .map(|term| format!("（搜索 `{term}`）"))
                .unwrap_or_default(),
            limit
                .map(|limit| format!("（最多 {limit} 条）"))
                .unwrap_or_default(),
        );
        if let Some(note) = &note {
            text.push_str(&format!("说明：{note}\n"));
        }
        if items.is_empty() {
            text.push_str(&format!(
                "本地缓存里没有这个库的条目（该服务器共缓存 {cached_total} 条）。\n\
                 先联网同步一次：`nova items --library {library}`（不带 `--cached`）。\n"
            ));
        } else {
            text.push_str(&table(
                &["ID", "类型", "时长", "标题"],
                &items.iter().map(item_row).collect::<Vec<_>>(),
            ));
        }

        Ok(CommandOutput::new(
            text,
            json!({
                "server": stored.info.id.as_str(),
                "library": library,
                "search": search,
                "data_source": Origin::Cache.json(),
                "total": matched,
                "cached_total": cached_total,
                "note": note,
                "items": items.iter().map(item_json).collect::<Vec<_>>(),
            }),
        ))
    }

    // -----------------------------------------------------------------
    // info
    // -----------------------------------------------------------------

    /// `nova info`：条目优先读本地缓存，未命中再走网络；资源同理。
    pub async fn info(&self, server: Option<&str>, item: &str) -> Result<CommandOutput, CliError> {
        let stored = self.select_server(server)?;
        let item_id = ItemId::new(item.to_owned());
        let mut notes: Vec<String> = Vec::new();

        let (media_item, item_origin) = match self.db.item(&stored.info.id, &item_id)? {
            Some(cached) => (cached, Origin::Cache),
            None => {
                let provider = self.checked_provider(&stored)?;
                let fetched = provider
                    .item(&item_id)
                    .await
                    .map_err(crate::error::provider)?;
                self.cache_items(&stored, std::slice::from_ref(&fetched))?;
                self.persist_info(&provider)?;
                (fetched, Origin::Server)
            }
        };

        let mut sources = self.db.sources(&stored.info.id, &item_id)?;
        let mut sources_origin = if sources.is_empty() {
            None
        } else {
            Some(Origin::Cache)
        };
        if sources.is_empty() {
            match self.checked_provider(&stored) {
                Ok(provider) => match provider.sources(&item_id).await {
                    Ok(list) => {
                        if !list.is_empty() {
                            self.cache_sources(&stored, &list)?;
                        }
                        sources = list;
                        sources_origin = Some(Origin::Server);
                        self.persist_info(&provider)?;
                    }
                    Err(error) if item_origin == Origin::Cache => {
                        notes.push(format!("资源未刷新（网络不可用）：{error}"));
                    }
                    Err(error) => return Err(crate::error::provider(error)),
                },
                Err(error) if item_origin == Origin::Cache => {
                    notes.push(format!("离线：{error}"));
                }
                Err(error) => return Err(error),
            }
        }

        let mut text = format!("条目 {}  {}\n", media_item.id, media_item.title);
        text.push_str(&format!("  数据来源: {}\n", item_origin.text()));
        text.push_str(&format!("  类型    : {}\n", kind_text(media_item.kind)));
        text.push_str(&format!("  年份    : {}\n", or_dash(media_item.year)));
        text.push_str(&format!(
            "  时长    : {}\n",
            duration_text(media_item.runtime.map(|runtime| runtime.as_secs()))
        ));
        text.push_str(&format!(
            "  观看    : {}{}\n",
            if media_item.user_data.played {
                "已看完"
            } else if !media_item.user_data.resume_position.is_zero() {
                "看到"
            } else {
                "未看"
            },
            if media_item.user_data.played || media_item.user_data.resume_position.is_zero() {
                String::new()
            } else {
                format!(
                    " {}",
                    duration_text(Some(media_item.user_data.resume_position.as_secs()))
                )
            }
        ));
        text.push_str(&format!(
            "  子条目  : {}\n",
            or_dash(media_item.child_count)
        ));
        text.push_str(&format!(
            "  图片    : 海报={}{}  背景={}\n",
            yes_no(media_item.images.poster.is_some()),
            or_empty_tag(media_item.images.poster_tag.as_deref()),
            yes_no(media_item.images.backdrop.is_some()),
        ));
        text.push_str(&format!(
            "  资源    : {} 个（来源：{}）\n",
            sources.len(),
            match sources_origin {
                Some(origin) => origin.text(),
                None => "无",
            }
        ));
        for (index, source) in sources.iter().enumerate() {
            text.push_str(&format!(
                "    [{index}] {}  container={}  size={}  bitrate={}  时长={}\n",
                source.id,
                or_dash(source.container.as_deref()),
                source
                    .size
                    .map(|size| format!("{:.1} MiB", size as f64 / 1024.0 / 1024.0))
                    .unwrap_or_else(|| "-".to_owned()),
                source
                    .bitrate
                    .map(|rate| format!("{:.1} Mb/s", rate as f64 / 1_000_000.0))
                    .unwrap_or_else(|| "-".to_owned()),
                duration_text(source.runtime.map(|runtime| runtime.as_secs())),
            ));
            for stream in &source.streams {
                text.push_str(&format!("        {}\n", stream_line(stream)));
            }
        }
        if sources.is_empty() {
            text.push_str(
                "    该条目没有可播放资源（剧集 / 季 / 文件夹这类容器要先展开子条目）。\n",
            );
        }
        for note in &notes {
            text.push_str(&format!("  注意    : {note}\n"));
        }

        Ok(CommandOutput::new(
            text,
            json!({
                "server": stored.info.id.as_str(),
                "item": item_json(&media_item),
                "item_source": item_origin.json(),
                "sources": sources.iter().map(source_json).collect::<Vec<_>>(),
                "sources_source": sources_origin.map(Origin::json),
                "notes": notes,
            }),
        ))
    }

    // -----------------------------------------------------------------
    // play
    // -----------------------------------------------------------------

    /// `nova play`：资源优先读缓存（离线也能拼出地址），没有再联网。
    pub async fn play(
        &self,
        server: Option<&str>,
        item: &str,
        verify: bool,
    ) -> Result<CommandOutput, CliError> {
        let stored = self.select_server(server)?;
        let item_id = ItemId::new(item.to_owned());
        let provider = self.checked_provider(&stored)?;

        let mut sources = self.db.sources(&stored.info.id, &item_id)?;
        let mut sources_origin = if sources.is_empty() {
            None
        } else {
            Some(Origin::Cache)
        };
        if sources.is_empty() {
            sources = provider
                .sources(&item_id)
                .await
                .map_err(crate::error::provider)?;
            sources_origin = Some(Origin::Server);
            if !sources.is_empty() {
                // 资源表有外键 `item_id → items(id)`：先把条目落到库里。
                if self.db.item(&stored.info.id, &item_id)?.is_none() {
                    let fetched = provider
                        .item(&item_id)
                        .await
                        .map_err(crate::error::provider)?;
                    self.cache_items(&stored, std::slice::from_ref(&fetched))?;
                }
                self.cache_sources(&stored, &sources)?;
            }
        }
        self.persist_info(&provider)?;

        let source = sources.first().ok_or_else(|| CliError::NotFound {
            what: format!("条目 {item} 的可播放资源"),
        })?;
        let source_id = source.id.clone();

        // 关键一步：播放地址由 provider 生成（令牌在查询串里，Debug/Display 已脱敏）。
        let target = provider
            .stream_target(&item_id, &source_id)
            .await
            .map_err(crate::error::provider)?;

        let probe = if verify {
            Some(probe_stream(target.url()).await?)
        } else {
            None
        };
        let title = self
            .db
            .item(&stored.info.id, &item_id)?
            .map(|item| item.title);

        let mut text = format!("可播放地址：{target}\n");
        text.push_str(&format!(
            "  条目    : {item}{}\n",
            title
                .as_deref()
                .map(|title| format!("（{title}）"))
                .unwrap_or_default()
        ));
        text.push_str(&format!(
            "  资源    : {} {} {} {}（来源：{}）\n",
            source.id,
            or_dash(source.container.as_deref()),
            or_dash(
                source
                    .video_stream()
                    .and_then(|stream| stream.video.as_ref())
                    .map(|video| format!("{}x{}", video.width, video.height))
                    .as_deref()
            ),
            duration_text(source.runtime.map(|runtime| runtime.as_secs())),
            match sources_origin {
                Some(origin) => origin.text(),
                None => "无",
            }
        ));
        text.push_str(&format!(
            "  连接    : {}\n",
            if target.is_plaintext() {
                "明文 HTTP（局域网自建服务器）"
            } else {
                "TLS"
            }
        ));
        if let Some(probe) = &probe {
            let content_type = probe.content_type.as_deref().unwrap_or("-");
            text.push_str(&format!(
                "  Range   : {} {} {} 字节{}\n",
                probe.status,
                content_type,
                probe.bytes,
                if probe.status == 200 || probe.status == 206 {
                    ""
                } else {
                    "  ← 不是 200/206，地址可能已失效"
                }
            ));
        }
        text.push_str("  地址里的查询串含令牌，已脱敏；`--show-url` 才会打印真实地址。\n");

        Ok(CommandOutput::new(
            text,
            json!({
                "server": stored.info.id.as_str(),
                "item": item,
                "source": source_json(source),
                "sources_source": sources_origin.map(Origin::json),
                "display_url": target.to_string(),
                "url": target.url(),
                "plaintext": target.is_plaintext(),
                "probe": probe.as_ref().map(|probe| json!({
                    "status": probe.status,
                    "content_type": probe.content_type,
                    "bytes": probe.bytes,
                })),
            }),
        ))
    }

    // -----------------------------------------------------------------
    // progress
    // -----------------------------------------------------------------

    /// `nova progress`：上报播放进度（可带暂停与播放方式）。
    pub async fn progress(
        &self,
        server: Option<&str>,
        item: &str,
        position: f64,
        paused: bool,
        method: PlayMethod,
    ) -> Result<CommandOutput, CliError> {
        let stored = self.select_server(server)?;
        let item_id = ItemId::new(item.to_owned());
        let provider = self.checked_provider(&stored)?;

        let mut sources = self.db.sources(&stored.info.id, &item_id)?;
        if sources.is_empty() {
            sources = provider
                .sources(&item_id)
                .await
                .map_err(crate::error::provider)?;
            if !sources.is_empty() {
                if self.db.item(&stored.info.id, &item_id)?.is_none() {
                    let fetched = provider
                        .item(&item_id)
                        .await
                        .map_err(crate::error::provider)?;
                    self.cache_items(&stored, std::slice::from_ref(&fetched))?;
                }
                self.cache_sources(&stored, &sources)?;
            }
        }
        self.persist_info(&provider)?;

        let source = sources.first().ok_or_else(|| CliError::NotFound {
            what: format!("条目 {item} 的可播放资源"),
        })?;

        let position = Duration::from_secs_f64(position);
        let summary = self.db.item(&stored.info.id, &item_id)?;
        let duration = summary
            .as_ref()
            .and_then(|item| item.runtime)
            .or(source.runtime);

        // "是否看完"用本地偏好里的阈值判断（和播放器将来用同一套规则）。
        let preferences = self.db.preferences()?;
        let finished = duration
            .is_some_and(|total| total.saturating_sub(position) <= preferences.finished_threshold);
        // 暂停的进度不能同时算"看完"：那会让 provider 立刻补一条 Stopped。
        let played = finished && !paused;

        let progress = PlaybackProgress {
            item_id: item_id.clone(),
            source_id: source.id.clone(),
            position,
            duration,
            played,
            paused,
            method,
        };
        provider
            .report_progress(&progress)
            .await
            .map_err(crate::error::provider)?;

        let mut text = String::from("已上报进度\n");
        text.push_str(&format!("  条目    : {item}\n"));
        text.push_str(&format!("  资源    : {}\n", source.id));
        text.push_str(&format!(
            "  位置    : {} / {}\n",
            duration_text(Some(position.as_secs())),
            duration_text(duration.map(|duration| duration.as_secs()))
        ));
        text.push_str(&format!(
            "  状态    : {}（{}）\n",
            if paused { "已暂停" } else { "播放中" },
            method_text(method)
        ));
        text.push_str(&format!(
            "  看完    : {}{}\n",
            if played { "是" } else { "否" },
            if played {
                "（已额外提交一次\"停止播放\"）"
            } else {
                ""
            }
        ));
        text.push_str(&format!(
            "  服务端  : 接受（Sessions/Playing/Progress{}）\n",
            if played {
                " + Sessions/Playing/Stopped"
            } else {
                ""
            }
        ));

        Ok(CommandOutput::new(
            text,
            json!({
                "server": stored.info.id.as_str(),
                "item": item,
                "source": source.id.as_str(),
                "position_secs": position.as_secs(),
                "duration_secs": duration.map(|duration| duration.as_secs()),
                "paused": paused,
                "method": method_json(method),
                "played": played,
                "ok": true,
            }),
        ))
    }
}

// ---------------------------------------------------------------------------
// 登录辅助
// ---------------------------------------------------------------------------

/// 校验地址并处理明文连接的确认（会打印警告），返回规范化后的基址。
///
/// 这个开关**只**影响"密码能不能走明文"，与 TLS 证书校验无关：
/// provider 里没有任何跳过证书校验的入口，这里也不提供。
fn prepare_login(options: &LoginArgs) -> Result<String, CliError> {
    let base = nova_provider::parse_base_url(&options.url).map_err(crate::error::provider)?;
    let base_url = base.as_str().trim_end_matches('/').to_owned();

    if !base.scheme().eq_ignore_ascii_case("http") {
        return Ok(base_url);
    }
    if !options.insecure_plaintext {
        return Err(CliError::PlaintextRefused { url: base_url });
    }
    eprintln!(
        "{}",
        concat!(
            "警告：正在把密码发往明文 http:// 连接（--insecure-plaintext）。\n",
            "　　　同一网络上的其他人可以看到这次登录；只在自己信任的局域网里这么做。\n",
            "　　　注意：这个开关不涉及 TLS 证书校验——https:// 的自签名证书仍然会失败。",
        )
    );
    Ok(base_url)
}

/// 读密码：`--password-stdin` 读整个标准输入，否则从终端读一行。
fn read_password(password_stdin: bool) -> Result<SecretString, CliError> {
    let mut buffer = String::new();
    if !password_stdin {
        eprintln!("从标准输入读取密码（终端是否回显由终端决定；脚本里请用 --password-stdin）：");
    }
    std::io::stdin().read_to_string(&mut buffer)?;
    // 只去掉行尾，密码里允许有空格。
    let password = buffer.trim_end_matches(['\n', '\r']).to_owned();
    if password.is_empty() {
        return Err(CliError::Usage(
            "密码为空：用 `--password-stdin` 从管道喂入，或直接在终端输入".to_owned(),
        ));
    }
    Ok(SecretString::new(password))
}

// ---------------------------------------------------------------------------
// `--verify`：对播放地址发一次 Range 请求
// ---------------------------------------------------------------------------

struct ProbeReport {
    status: u16,
    content_type: Option<String>,
    bytes: usize,
}

/// 只读到 4 KiB 就停：服务端若忽略 `Range` 把整个文件推过来，也不会把 20MB 读进内存。
async fn probe_stream(url: &str) -> Result<ProbeReport, CliError> {
    const PROBE_LIMIT: usize = 4096;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| CliError::Network(format!("无法初始化 HTTP 客户端：{error}")))?;
    let mut response = http
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-1023")
        .send()
        .await
        .map_err(probe_error)?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let mut bytes = 0_usize;
    while bytes < PROBE_LIMIT {
        let chunk = response.chunk().await.map_err(probe_error)?;
        let Some(chunk) = chunk else {
            break;
        };
        bytes += chunk.len();
    }

    Ok(ProbeReport {
        status,
        content_type,
        bytes: bytes.min(PROBE_LIMIT),
    })
}

/// `reqwest::Error` 的 `Display` 带完整地址（播放地址里有令牌），摘掉再取文本。
fn probe_error(source: reqwest::Error) -> CliError {
    let message = if source.is_timeout() {
        "请求超时"
    } else if source.is_connect() {
        "无法连接服务器"
    } else if source.is_decode() {
        "响应体解码失败"
    } else {
        "请求失败"
    };
    tracing::debug!(error = %source.without_url(), "Range 请求失败");
    CliError::Network(message.to_owned())
}
