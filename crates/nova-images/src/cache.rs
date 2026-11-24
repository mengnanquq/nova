// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 磁盘缓存本体：[`ImageCache`]。
//!
//! # 布局
//!
//! ```text
//! <dir>/<hash 前两位>/<hash>.img
//! ```
//!
//! - `<hash>` 是 `sha256(url ‖ 0x00 ‖ tag)` 的十六进制（无 tag 或空 tag 时是 `sha256(url)`）。
//!   用 SHA-256 而不是 `DefaultHasher`：后者跨进程、跨版本都不保证稳定，
//!   重启一次就等于清空缓存。分隔符 `0x00` 防止 `(url="ab", tag="c")` 与
//!   `(url="a", tag="bc")` 拼出同一串输入。
//! - 两级分桶是为了不把几十万个文件塞进一个目录：`ext4` 的 `dir_index` 能撑住，
//!   但 `ls`、备份、杀毒扫描都会明显变慢。
//! - `.img` 里存的是**下载到的原始编码字节**（PNG/JPEG/WebP），不是解码后的 RGBA。
//!   理由：解码结果大 5–20 倍（一张 1000×1500 的海报 RGBA 约 6 MB，JPEG 不到 300 KB），
//!   而 `max_width` 是按调用场景变的（列表缩略图 vs 详情页大图），
//!   存编码字节可以让同一个文件服务两种宽度。
//!
//! # 一致性
//!
//! 先写同目录临时文件、`fsync`、再 `rename` 到最终路径：`rename` 在同一个文件系统内
//! 是原子的，所以任何时刻磁盘上的 `<hash>.img` 要么不存在，要么是完整文件。
//! 写失败会删掉临时文件——半个文件如果留在最终路径上，之后每次都会"命中"一张坏图。
//!
//! 索引（`image_cache` 表）在文件落盘**之后**才写：反过来的话，中途崩溃会留下
//! "索引说有、磁盘上没有"的记录。反过来（文件在、索引没写）只是浪费一次下载。
//!
//! # 坏缓存
//!
//! 缓存文件是"可以被别人弄坏"的东西（磁盘满、手工清理、上一版程序留下的垃圾）。
//! 读缓存时如果解码失败，就删掉文件当未命中，重新下载一次；第二次还失败才报错。
//! 整个过程不 panic：一张坏图不应该让整个界面崩掉。
//!
//! # 并发
//!
//! 同一 key 的并发请求只下载一次：每个 key 一把异步锁（single-flight），拿到锁后
//! 再看一次缓存（"双重检查"），第 N 个到达的请求直接吃第 1 个写下的文件。
//! 选它而不是 `mpsc` 广播或 `futures::future::Shared`：
//!
//! - 不需要为此加 `futures` 依赖，`tokio::sync::Mutex` 已在依赖里；
//! - 按 key 分锁，不同图片互不阻塞（换成一把全局锁，列表一次加载 50 张海报就退化成串行）；
//! - 锁只保护"检查缓存 + 下载 + 落盘"，失败不会被记住——下一次请求会正常重试。
//!
//! 锁表存的是 `Weak`：最后一个持有者把 `Arc` 放下时条目自动失效，插入新条目时
//! 顺手清掉死条目。于是常驻内存只与"当前正在下载的图片数"成正比，而不是与历史上
//! 看过的图片数成正比；请求被取消（future 被 drop）也不会留下永远持锁的幽灵。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use nova_storage::{Database, unix_now};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::decode::{DecodedImage, decode};
use crate::error::{ImageError, endpoint_of};
use crate::fetch::Fetcher;

/// 分桶目录名的长度（缓存键的前两个十六进制字符）。
const BUCKET_PREFIX_LEN: usize = 2;

/// 缓存文件扩展名。
const FILE_EXT: &str = "img";

/// 锁表条目超过这个数量时清理已经失效的条目。
const FLIGHT_PRUNE_THRESHOLD: usize = 64;

/// 一次图片请求。
///
/// `Debug` 里只有脱敏地址：结构体本身可能被上层顺手打进日志。
pub struct ImageRequest<'a> {
    /// 图片地址（可能带令牌的查询串）。
    pub url: &'a str,
    /// 版本标记（`ImageSet::poster_tag` / `backdrop_tag`）。
    ///
    /// **必须**在服务端换图后跟着变，否则缓存会一直命中旧图。为 `None`（或空串）
    /// 时缓存键只由 URL 决定。
    pub tag: Option<&'a str>,
    /// 期望的最大宽度（像素）。`None` 表示保持原始尺寸。
    ///
    /// 只缩不放：目标宽度大于原图时按原图返回。
    pub max_width: Option<u32>,
}

impl<'a> ImageRequest<'a> {
    /// 只带地址的请求：无版本标记、不缩放。
    pub fn new(url: &'a str) -> Self {
        Self {
            url,
            tag: None,
            max_width: None,
        }
    }

    /// 设置版本标记（服务端 `ImageTags`）。
    pub fn with_tag(mut self, tag: Option<&'a str>) -> Self {
        self.tag = tag;
        self
    }

    /// 设置最大宽度。
    pub fn with_max_width(mut self, max_width: u32) -> Self {
        self.max_width = Some(max_width);
        self
    }
}

impl std::fmt::Debug for ImageRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageRequest")
            .field("url", &endpoint_of(self.url))
            .field("tag", &self.tag)
            .field("max_width", &self.max_width)
            .finish()
    }
}

/// 计算缓存键：`sha256(url ‖ 0x00 ‖ tag)` 的十六进制（小写）；无 tag（或 tag 为空）时是 `sha256(url)`。
///
/// 空 tag 当作 `None`：服务端没给版本标记时会返回空串，让"空串"和"没有"算出两个键
/// 只会白白多下一份图。
///
/// 公开出来是为了让调用方（或测试）能自己定位缓存文件；键本身不含任何秘密，
/// 从键也推不回 URL。
pub fn cache_key(url: &str, tag: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    if let Some(tag) = tag.filter(|tag| !tag.is_empty()) {
        // 分隔符：没有它，`(url="ab", tag="c")` 与 `(url="a", tag="bc")` 会撞键。
        hasher.update([0u8]);
        hasher.update(tag.as_bytes());
    }
    to_hex(&hasher.finalize())
}

/// 图片磁盘缓存。
///
/// 生命周期比单次请求长：目录、数据库句柄、预算都在这里，界面层可以把它放进
/// `Arc` 到处共享。所有方法都是 `&self`，可以多线程并发调用。
pub struct ImageCache {
    dir: PathBuf,
    db: Arc<Database>,
    budget_bytes: u64,
    /// 每个正在处理的 key 一把锁（`Weak`，见模块文档的"并发"一节）。
    flights: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

impl ImageCache {
    /// 打开（必要时创建）缓存目录。
    ///
    /// `budget_bytes` 只在 [`ImageCache::evict`] 里用到——本 crate 不会背着调用方
    /// 偷偷删文件，什么时候清理是上层的策略问题。
    ///
    /// 只创建根目录；分桶子目录在首次向该桶写入时创建（一个全新缓存没必要先建 256 个空目录）。
    pub fn new(dir: PathBuf, db: Arc<Database>, budget_bytes: u64) -> Result<Self, ImageError> {
        std::fs::create_dir_all(&dir).map_err(|source| ImageError::io(&dir, source))?;
        Ok(Self {
            dir,
            db,
            budget_bytes,
            flights: Mutex::new(HashMap::new()),
        })
    }

    /// 缓存根目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 淘汰预算（字节）。
    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// 某个缓存键对应的文件路径。
    pub fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(rel_path(key))
    }

    /// 取缓存；没有就下载、解码、落盘后再返回。
    ///
    /// 命中时只刷新 LRU 时间戳，不发请求。`max_width` 不影响缓存键：同一个文件
    /// 可以在一次调用里缩到 200、下一次缩到 600。
    ///
    /// 注意解码是 CPU 密集的同步操作：一次加载几十张图时，调用方应当分批，
    /// 或者在自己的阻塞任务里调用，而不是在一个 async 任务里循环。
    pub async fn get_or_fetch(
        &self,
        req: &ImageRequest<'_>,
        fetcher: &dyn Fetcher,
    ) -> Result<DecodedImage, ImageError> {
        let endpoint = endpoint_of(req.url);
        let key = cache_key(req.url, req.tag);

        // 快路径：已有缓存，不加锁、不 await 下载。
        if let Some(image) = self.cached(&key, &endpoint, req.max_width)? {
            return Ok(image);
        }

        // 慢路径：同一 key 排队，只有第一个真正下载。
        let flight = self.flight(&key);
        let guard = flight.lock().await;
        let result = self.fill(&key, req, fetcher, &endpoint).await;
        // 释放队列：`guard` 与 `flight` 都在离开作用域时 drop，锁表条目随之失效。
        drop(guard);
        drop(flight);
        result
    }

    /// 按 LRU 淘汰到预算以内，返回**实际删除**的字节数。
    ///
    /// 索引里的记录由 [`Database::evict_images_to_budget`] 在一个事务里删掉，
    /// 本方法只负责删文件：
    ///
    /// - 文件不存在不算错误（可能被用户手工清过）；
    /// - 单个文件删不掉（权限、被占用）只记 warn 并继续，不中断整轮淘汰——
    ///   索引已经删了，为这一个文件回滚整轮只会让缓存永远超预算。
    pub fn evict(&self) -> Result<u64, ImageError> {
        let evicted = self.db.evict_images_to_budget(self.budget_bytes)?;
        let mut freed = 0u64;
        for rel_path in evicted {
            let path = self.dir.join(&rel_path);
            let bytes = match std::fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(path = %path.display(), "待淘汰的缓存文件已不存在");
                    continue;
                }
                Err(source) => {
                    tracing::warn!(path = %path.display(), %source, "读取待淘汰缓存文件失败");
                    continue;
                }
            };
            match std::fs::remove_file(&path) {
                Ok(()) => freed = freed.saturating_add(bytes),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    tracing::warn!(path = %path.display(), %source, "删除缓存文件失败");
                }
            }
        }
        Ok(freed)
    }

    /// 当前缓存占用的总字节数（以索引为准）。
    pub fn cached_bytes(&self) -> Result<u64, ImageError> {
        Ok(self.db.image_cache_bytes()?)
    }

    /// 读缓存并解码；返回 `Ok(None)` 表示"当未命中处理"。
    ///
    /// 坏文件在这里被删除，让随后的下载重写它。
    fn cached(
        &self,
        key: &str,
        endpoint: &str,
        max_width: Option<u32>,
    ) -> Result<Option<DecodedImage>, ImageError> {
        let Some(record) = self.db.cached_image(key)? else {
            return Ok(None);
        };
        let path = self.dir.join(&record.rel_path);

        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(key = short_key(key), "索引有记录但文件缺失，按未命中处理");
                return Ok(None);
            }
            Err(source) => return Err(ImageError::io(&path, source)),
        };

        match decode(&bytes, endpoint, max_width) {
            Ok(image) => {
                // 刷新 LRU：不刷新的话"常用的图"和"很久以前看过一次的图"分不出来。
                if !self.db.touch_image(key, unix_now())? {
                    tracing::debug!(key = short_key(key), "缓存记录在读取期间被淘汰");
                }
                Ok(Some(image))
            }
            Err(error) => {
                // 坏缓存自愈：删文件 → 当未命中 → 上层重取一次。
                tracing::warn!(
                    key = short_key(key),
                    error = %error,
                    "缓存文件无法解码，删除后重新获取"
                );
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        tracing::warn!(path = %path.display(), %source, "删除损坏的缓存文件失败");
                    }
                }
                Ok(None)
            }
        }
    }

    /// 拿到锁之后的"再检查一次 + 下载 + 落盘"。
    async fn fill(
        &self,
        key: &str,
        req: &ImageRequest<'_>,
        fetcher: &dyn Fetcher,
        endpoint: &str,
    ) -> Result<DecodedImage, ImageError> {
        // 双重检查：排队期间前一个请求可能已经把文件写好了。
        if let Some(image) = self.cached(key, endpoint, req.max_width)? {
            return Ok(image);
        }

        tracing::debug!(key = short_key(key), "下载图片");
        let bytes = fetcher.fetch(req.url).await?;
        if bytes.is_empty() {
            return Err(ImageError::EmptyBody {
                endpoint: endpoint.to_owned(),
            });
        }

        // 先解码再落盘：不是图片的响应体绝不进缓存，否则污染会一直存在。
        let image = decode(&bytes, endpoint, req.max_width)?;
        self.store(key, &bytes)?;
        Ok(image)
    }

    /// 原子写入缓存文件并登记索引。
    fn store(&self, key: &str, bytes: &[u8]) -> Result<(), ImageError> {
        let rel_path = rel_path(key);
        let path = self.dir.join(&rel_path);
        let bucket = match path.parent() {
            Some(parent) => parent.to_path_buf(),
            None => self.dir.clone(),
        };
        std::fs::create_dir_all(&bucket).map_err(|source| ImageError::io(&bucket, source))?;

        write_atomic(&path, &bucket, key, bytes)?;
        self.db.record_image(key, &rel_path, bytes.len() as u64)?;
        Ok(())
    }

    /// 取（或创建）某个 key 的 single-flight 锁。
    ///
    /// 锁表是同步 `Mutex`：临界区只有一次 `HashMap` 查找/插入，不 await，
    /// 因此不会挡住 tokio 的工作线程。真正的等待发生在返回的异步锁上。
    fn flight(&self, key: &str) -> Arc<AsyncMutex<()>> {
        // 恶意/异常情况下 map 被毒化也照常用：里面只是一堆锁句柄，没有需要保持一致的数据。
        let mut flights = match self.flights.lock() {
            Ok(flights) => flights,
            Err(poisoned) => poisoned.into_inner(),
        };

        if let Some(flight) = flights.get(key).and_then(Weak::upgrade) {
            return flight;
        }

        // 插入前顺手清理死条目（`Weak` 已经升不起来的）。阈值很小：同时下载的图片
        // 数量本来就只有几张到几十张，链表级的 `retain` 比每次新建都查一遍更划算。
        if flights.len() >= FLIGHT_PRUNE_THRESHOLD {
            flights.retain(|_, flight| flight.strong_count() > 0);
        }

        let flight = Arc::new(AsyncMutex::new(()));
        flights.insert(key.to_owned(), Arc::downgrade(&flight));
        flight
    }
}

impl std::fmt::Debug for ImageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageCache")
            .field("dir", &self.dir)
            .field("budget_bytes", &self.budget_bytes)
            .finish_non_exhaustive()
    }
}

/// 相对路径：`<前两位>/<hash>.img`。
fn rel_path(key: &str) -> String {
    format!("{}/{key}.{FILE_EXT}", bucket_of(key))
}

/// 分桶目录名；键短于两位时（理论上不会发生）整个键当目录名。
fn bucket_of(key: &str) -> &str {
    key.get(..BUCKET_PREFIX_LEN).unwrap_or(key)
}

/// 日志里只打键的前 8 个字符：够把同一个 key 的多条日志串起来，又不至于刷屏。
fn short_key(key: &str) -> &str {
    key.get(..8).unwrap_or(key)
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// 先写临时文件、同步、再 `rename` 到最终路径。
fn write_atomic(path: &Path, bucket: &Path, key: &str, bytes: &[u8]) -> Result<(), ImageError> {
    let tmp = bucket.join(format!(".{key}.{}.tmp", tmp_suffix()));
    let written = write_then_rename(&tmp, path, bytes);
    if written.is_err() {
        // 写了一半或 rename 失败的临时文件必须消失：它既不是有效缓存，
        // 留着也只是把磁盘占满。
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                tracing::warn!(path = %tmp.display(), %source, "删除临时缓存文件失败");
            }
        }
    }
    written.map_err(|source| ImageError::io(path, source))
}

fn write_then_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    {
        let mut file = std::fs::File::create(tmp)?;
        file.write_all(bytes)?;
        // 掉电时 `rename` 可能先于数据落盘；不 fsync 的话，重启后会是 0 字节文件。
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

/// 临时文件名后缀：进程 id + 纳秒时间戳，避免不同进程/不同时刻互相覆盖。
fn tmp_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_depends_on_url_and_tag() {
        let plain = cache_key("https://host/a", None);
        assert_eq!(plain.len(), 64);
        assert!(plain.chars().all(|c| c.is_ascii_hexdigit()));

        let tagged = cache_key("https://host/a", Some("v1"));
        let retagged = cache_key("https://host/a", Some("v2"));
        assert_ne!(plain, tagged);
        assert_ne!(tagged, retagged);
        // 空 tag（服务端没给版本标记）与 None 等价。
        assert_eq!(cache_key("https://host/a", Some("")), plain);
    }

    #[test]
    fn key_is_stable_across_calls() {
        // 只有哈希函数稳定，跨进程缓存才有意义。
        assert_eq!(
            cache_key("https://host/a", Some("t")),
            cache_key("https://host/a", Some("t"))
        );
    }

    #[test]
    fn tag_separator_prevents_ambiguous_keys() {
        assert_ne!(
            cache_key("https://host/ab", Some("c")),
            cache_key("https://host/a", Some("bc"))
        );
    }

    #[test]
    fn layout_is_two_level_buckets() {
        let key = cache_key("https://host/a", None);
        let rel = rel_path(&key);
        assert_eq!(rel, format!("{}/{}.img", &key[..2], key));
        assert_eq!(bucket_of("ab"), "ab");
        assert_eq!(bucket_of("a"), "a");
    }
}
