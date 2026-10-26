//! 图片管道：书源给的图片地址统一收口到后端。
//!
//! 书源里的图片地址普遍是**带时效签名的短命 URL**（番茄图床的 `x-expires` /
//! `x-signature`，签名还随宿主会话轮换），而且格式不一定是浏览器能渲染的
//! （番茄封面是 HEIC，Chrome 直接报错）。把地址直接交给前端有两个后果：
//! 存进书架的那份很快就变死链，浏览器也解不了 HEIC。
//!
//! 因此这里做一层收口：
//!
//! - **前端只拿图片 id**：`id = md5(去掉查询串的地址)`，同一张图永远同一个 id，
//!   前端请求 `/reader3/image/<id>`，不接触书源地址；
//! - **首次取图时抓上游并落盘**：HEIC 转成 JPEG，其余格式原样缓存；
//! - **之后长期命中本地缓存**：签名过期不影响已经抓下来的图；
//! - **按宽度派生 + webp 协商**：`?w=` 缩放（只降不升）与 Accept 头协商的有损
//!   webp 都落成派生文件 `<id>.w<宽>.<webp|jpg>`，与原图同目录同一套容量记账；
//!   GIF（动画）/WebP/AVIF/SVG/ICO 原样透传，不做任何转换；
//! - **映射表**：`<storage>/cache/image/<id>.json` 是记录（上游地址 + 书籍上下文），
//!   `<id>.bin` 是图片本体。目录本身就是「id → 记录」的映射表，不需要额外的库表。

use crate::crawler::{
    fetcher::{read_body_limited, MAX_RESPONSE_BYTES},
    http_client::HttpClient,
};
use crate::error::error::AppError;
use crate::storage::cache::file_cache::enforce_flat_dir_capacity;
use crate::util::atomic_write::write_atomic;
use crate::util::hash::md5_hex;
use crate::util::keyed_mutex::KeyedMutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

/// 前端取图用的路径前缀。
pub const IMAGE_ROUTE_PREFIX: &str = "/reader3/image/";

/// 映射表与图片本体的落盘目录（相对 storage）。
const IMAGE_DIR: &str = "cache/image";

/// JPEG 转码质量：封面/插图这类展示用图，85 已经看不出损失。
const JPEG_QUALITY: u8 = 85;

/// 有损 webp 编码质量（Accept 协商命中时）：80 在省流与画质间取平衡。
const WEBP_QUALITY: f32 = 80.0;

/// libwebp 的单边尺寸上限（`WEBP_MAX_DIMENSION`）。webp crate 0.3 的 `encode()`
/// 对超限是 unwrap panic（`VP8_ENC_ERROR_BAD_DIMENSION`），必须在调用前判掉，
/// 让条漫长图走调用方的原图兜底，而不是 panic 穿透 `spawn_blocking` 变 500。
const WEBP_MAX_DIMENSION: u32 = 16383;

/// 派生图解码的单边尺寸上限（`decode_limits`）。
///
/// 压缩图的内存爆炸半径由上游说了算：几十 MB 的 PNG/BMP 可以展开成数 GB 的
/// 像素缓冲，多个 `?w=` 请求叠加时每份解码还同时在 `spawn_blocking` 里活着。
/// 单边 16384 已经远超阅读器渲染场景。
const DECODE_MAX_DIMENSION: u32 = 16384;

/// 派生图解码期间允许的总分配上限（`decode_limits`）。
const DECODE_MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

/// 派生图（`?w=` 缩放）请求宽度的上下限。
const VARIANT_MIN_WIDTH: u32 = 64;
const VARIANT_MAX_WIDTH: u32 = 2048;

/// 可以缩放/转码的原图类型。GIF（动画）、WebP、AVIF、SVG、ICO 原样透传：
/// 动画重编码会丢帧，矢量放大反而失真。
const CONVERTIBLE_CONTENT_TYPES: &[&str] = &["image/jpeg", "image/png", "image/bmp"];

/// 取图单飞锁表上限：正常路径用完即清，上限只为兜住取消/panic 遗留的条目。
const IMAGE_LOAD_LOCK_MAX_ENTRIES: usize = 256;

/// 一条图片映射记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecord {
    pub id: String,
    /// 用途：`cover` / `chapter` / `review`。
    pub kind: String,
    /// 最近一次已知的上游地址（含签名）。
    pub source_url: String,
    /// 登记时所属的书籍（`book_<n>`）：按书清理与回源自愈都靠它。
    #[serde(default)]
    pub book_id: Option<String>,
    /// 落盘后的类型（HEIC 已转成 JPEG）。
    pub content_type: String,
    pub ext: String,
    pub updated_at: i64,
    /// 最近一次抓取失败的时间戳，便于排查「为什么一直是占位图」。
    #[serde(default)]
    pub failed_at: Option<i64>,
}

pub struct ImageService {
    http: HttpClient,
    storage_dir: PathBuf,
    /// 图片缓存目录容量上限（0 = 不限），与封面缓存共用同一个配置。
    limit_bytes: u64,
    /// 取图单飞：`id -> 按键互斥锁`。并发首取同一张图只有一个真正抓上游
    /// + HEIC 转码，其余等键锁后复用落盘结果。KeyedMutex 限条数兜底，
    ///   满员逐出跳过在用锁（详见 `util::keyed_mutex` 模块文档）。
    load_locks: KeyedMutex,
}

impl ImageService {
    pub fn new(http: HttpClient, storage_dir: impl Into<PathBuf>, limit_bytes: u64) -> Self {
        Self {
            http,
            storage_dir: storage_dir.into(),
            limit_bytes,
            load_locks: KeyedMutex::new("image_load", IMAGE_LOAD_LOCK_MAX_ENTRIES),
        }
    }

    /// 图片身份：去掉查询串与片段后的地址的 md5。
    ///
    /// 查询串里全是签名与归因参数（`x-expires` / `x-signature` / `lk3s`），
    /// 每次取都会变；去掉它同一张图才是同一个 id，缓存也才能长期命中。
    pub fn image_key(url: &str) -> String {
        md5_hex(&strip_query(url))
    }

    /// 前端取图地址。
    pub fn route(id: &str) -> String {
        format!("{IMAGE_ROUTE_PREFIX}{id}")
    }

    /// 从本站取图地址里取回 id（`/reader3/image/<id>` 或完整 URL）。
    pub fn id_from_route(value: &str) -> Option<&str> {
        let rest = value.split_once(IMAGE_ROUTE_PREFIX)?.1;
        let id = rest.split(['?', '#', '/']).next()?.trim();
        (!id.is_empty()).then_some(id)
    }

    fn dir(&self) -> PathBuf {
        self.storage_dir.join(IMAGE_DIR)
    }

    fn body_path(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.bin"))
    }

    /// 派生图（按宽度缩放 / webp 转码）的落盘路径，与原图同目录。
    fn variant_path(&self, id: &str, width: u32, webp: bool) -> PathBuf {
        self.dir().join(format!(
            "{id}.w{width}.{}",
            if webp { "webp" } else { "jpg" }
        ))
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.json"))
    }

    /// 读一条映射记录。
    pub async fn record(&self, id: &str) -> Option<ImageRecord> {
        let data = fs::read(self.record_path(id)).await.ok()?;
        serde_json::from_slice(&data).ok()
    }

    async fn save_record(&self, record: &ImageRecord) {
        let Ok(data) = serde_json::to_vec(record) else {
            return;
        };
        // 原子落盘：并发读到的要么是旧记录、要么是完整新记录；临时文件名
        // 带进程号与进程内序号，同一 id 并发 register/update_source_url
        // 不会互踩同一个 json.tmp。记录写盘失败只丢一次更新（下次抓取
        // 会重建），不影响主流程，故沿用忽略错误的约定
        let _ = write_atomic(&self.record_path(&record.id), &data).await;
    }

    /// 登记一张图，返回它的 id。
    ///
    /// 上游地址没变就不重复写盘；`book_id` 是所属书籍（按书清理、回源自愈用），
    /// 缺省时沿用旧记录里的值。
    pub async fn register(&self, kind: &str, url: &str, book_id: Option<&str>) -> Option<String> {
        let url = url.trim();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return None;
        }
        let id = Self::image_key(url);
        let existing = self.record(&id).await;
        let unchanged = existing
            .as_ref()
            .is_some_and(|record| record.source_url == url);
        if unchanged {
            return Some(id);
        }
        let record = ImageRecord {
            id: id.clone(),
            kind: kind.to_string(),
            source_url: url.to_string(),
            // book_id 归一为 canonical：别名（book_007）登记下的记录在按书
            // 清理（canonical）时删不到，会成孤儿
            book_id: book_id
                .and_then(crate::storage::db::repo::canonical_book_id)
                .or(existing.as_ref().and_then(|r| r.book_id.clone())),
            content_type: existing
                .as_ref()
                .map(|r| r.content_type.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            ext: existing
                .as_ref()
                .map(|r| r.ext.clone())
                .unwrap_or_else(|| "bin".to_string()),
            updated_at: now_secs(),
            failed_at: None,
        };
        self.save_record(&record).await;
        Some(id)
    }

    /// 上游地址失效后回源拿到的新地址，写回映射表。
    pub async fn update_source_url(&self, id: &str, url: &str) {
        let Some(mut record) = self.record(id).await else {
            return;
        };
        if record.source_url == url {
            return;
        }
        record.source_url = url.to_string();
        record.updated_at = now_secs();
        record.failed_at = None;
        self.save_record(&record).await;
    }

    /// 把响应里书籍的 `coverUrl` 换成本站取图地址，并登记映射。
    ///
    /// 只动 `coverUrl`：`customCoverUrl` 是用户自己设的，不该被改写。
    pub async fn rewrite_cover_urls(&self, value: &mut Value) {
        let mut targets = Vec::new();
        collect_cover_targets(value, "", &mut targets);
        for target in targets {
            let Some(id) = self
                .register("cover", &target.url, target.book_id.as_deref())
                .await
            else {
                continue;
            };
            if let Some(slot) = value.pointer_mut(&target.pointer) {
                *slot = Value::String(Self::route(&id));
            }
        }
    }

    /// 把响应里评论配图（`images` 字符串数组）收口成单张本站取图地址。
    ///
    /// 站点常为同一张图给多个格式变体（番茄同时给 HEIC 与 JPEG），这里替客户端
    /// 挑一张能渲染的：管道会把 HEIC 转成 JPEG，所以只要留一张就够，
    /// 留多张反而会在前端渲染出重复的图。
    pub async fn rewrite_image_lists(&self, value: &mut Value, kind: &str, book_id: Option<&str>) {
        let mut targets = Vec::new();
        collect_string_arrays(value, "", "images", &mut targets);
        for target in targets {
            let Some(picked) = pick_renderable(&target.urls) else {
                continue;
            };
            let Some(id) = self.register(kind, picked, book_id).await else {
                continue;
            };
            if let Some(slot) = value.pointer_mut(&target.pointer) {
                *slot = Value::Array(vec![Value::String(Self::route(&id))]);
            }
        }
    }

    /// 按书删除其全部图片（记录 + 本体 + 派生图），返回删除的图片数。
    ///
    /// 目录即映射表：逐条读记录比对 book_id。只在删书/按书清缓存时调用，
    /// 频率极低，全目录扫描可接受。book_id 先归一为 canonical（别名/非法
    /// 输入不命中任何记录——登记侧同样归一，两端口径一致）。
    pub async fn remove_by_book_id(&self, book_id: &str) -> u64 {
        let Some(book_id) = crate::storage::db::repo::canonical_book_id(book_id) else {
            return 0;
        };
        let dir = self.dir();
        let Ok(mut entries) = fs::read_dir(&dir).await else {
            return 0;
        };
        let mut removed = 0u64;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(data) = fs::read(&path).await else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<ImageRecord>(&data) else {
                continue;
            };
            if record.book_id.as_deref() != Some(book_id.as_str()) {
                continue;
            }
            let _ = fs::remove_file(&path).await;
            let _ = fs::remove_file(self.body_path(&record.id)).await;
            self.remove_variants(&record.id).await;
            removed += 1;
        }
        removed
    }

    /// 删除某个 id 的全部派生图（`<id>.w<宽>.<webp|jpg>`），返回删除的文件数。
    ///
    /// 派生图是「原图字节 + 宽度 + 格式」的纯函数，原图换了它们就全部过期；
    /// 与其每次命中都校验 mtime/哈希（成本高），不如只在两个明确事件清理：
    /// **原图重抓落盘**（[`Self::fetch_and_store`]）与**按书删除**
    /// （[`Self::remove_by_book_id`]），之后变体请求按需重建。只认
    /// `<id>.w` 前缀 + webp/jpg 扩展名：记录 `.json`、本体 `.bin`、
    /// 别的 id 与原子写的 `.tmp` 孤儿都不碰。
    async fn remove_variants(&self, id: &str) -> u64 {
        let prefix = format!("{id}.w");
        let Ok(mut entries) = fs::read_dir(self.dir()).await else {
            return 0;
        };
        let mut removed = 0u64;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(&prefix) {
                continue;
            }
            if !matches!(
                entry.path().extension().and_then(|e| e.to_str()),
                Some("webp") | Some("jpg")
            ) {
                continue;
            }
            if fs::remove_file(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::debug!("已清理图片派生缓存 id={id} count={removed}");
        }
        removed
    }

    /// 取图：缓存命中直接返回，否则抓上游（必要时转码）后落盘。
    ///
    /// 并发首取同一张图按 id 单飞：等键锁的请求拿到锁后先再看一次 body
    /// 文件，等锁期间首个请求可能已经落盘，直接复用不再打上游。
    pub async fn load(&self, id: &str) -> Result<(Vec<u8>, String), AppError> {
        let Some(mut record) = self.record(id).await else {
            return Err(AppError::NotFound("图片不存在".to_string()));
        };
        if let Ok(bytes) = fs::read(self.body_path(id)).await {
            return Ok((bytes, record.content_type));
        }
        let lock = self.acquire_load_lock(id);
        let _key_guard = lock.lock().await;
        // 二次检查：首个请求可能刚把图落盘。此时记录也可能被改写
        // （如 HEIC 转码成 JPEG 后 content_type 已更新），等待者必须
        // 重读记录，否则会用过期的 image/heic 配转码后的 JPEG 字节。
        if let Ok(bytes) = fs::read(self.body_path(id)).await {
            drop(_key_guard);
            self.release_load_lock(id, &lock);
            let content_type = match self.record(id).await {
                Some(latest) => latest.content_type,
                None => record.content_type,
            };
            return Ok((bytes, content_type));
        }
        let result = self.fetch_and_store(id, &mut record).await;
        drop(_key_guard);
        self.release_load_lock(id, &lock);
        result
    }

    /// 取图变体：`?w=` 可选按宽度缩小（只降不升），并按请求头 Accept 协商
    /// 输出有损 webp（质量 80），否则 JPEG（质量 85）。
    ///
    /// - 先走 [`Self::load`] 确保原图已在本地（抓上游 / HEIC 转码 / 按 id 单飞
    ///   全部复用同一条链路）；
    /// - 只有 JPEG/PNG/BMP 会转换；GIF（动画）/WebP/AVIF/SVG/ICO 原样返回；
    /// - 请求宽度 clamp 到 `[64, 2048]`，且 >= 原图宽度时不放大；
    /// - 无 `width` 且不要 webp 时直接返回原图，不产生派生缓存；
    /// - 派生文件 `<id>.w<实际宽度>.<webp|jpg>` 与原图同目录、同一套容量记账，
    ///   并按 `v:<id>|<宽>|<格式>` 单独单飞（前缀避免与原图的 id 键冲突）。
    pub async fn load_variant(
        &self,
        id: &str,
        width: Option<u32>,
        accept_webp: bool,
    ) -> Result<(Vec<u8>, String), AppError> {
        // 先确保原图就位；失败（含上游抓取失败）直接上抛，由 handler 决定是否回源自愈
        let (original, content_type) = self.load(id).await?;
        if !CONVERTIBLE_CONTENT_TYPES.contains(&content_type.as_str()) {
            return Ok((original, content_type));
        }
        // 快路径：无 width 且不要 webp，原图直出，连尺寸都不用解析
        let requested = width.map(|w| w.clamp(VARIANT_MIN_WIDTH, VARIANT_MAX_WIDTH));
        if requested.is_none() && !accept_webp {
            return Ok((original, content_type));
        }
        // 只解析文件头拿原图宽度；解不出尺寸（内容损坏/格式对不上）就保守原图直出
        let Ok(orig_width) = read_width(&original) else {
            return Ok((original, content_type));
        };
        // 只降不升：请求宽度 >= 原图宽度时不缩放（但仍可按 Accept 转 webp）
        let scale_down = requested.is_some_and(|w| w < orig_width);
        if !scale_down && !accept_webp {
            return Ok((original, content_type));
        }
        let target_width = if scale_down {
            requested.unwrap_or(orig_width)
        } else {
            orig_width
        };
        let variant = self.variant_path(id, target_width, accept_webp);
        let lock_key = format!(
            "v:{id}|{target_width}|{}",
            if accept_webp { "webp" } else { "jpg" }
        );
        let lock = self.acquire_load_lock(&lock_key);
        let _key_guard = lock.lock().await;
        // 二次检查：等锁期间别的请求可能已把同一派生图落盘
        if let Ok(bytes) = fs::read(&variant).await {
            drop(_key_guard);
            self.release_load_lock(&lock_key, &lock);
            return Ok((bytes, variant_content_type(accept_webp)));
        }
        let encoded = {
            // 解码 + 缩放 + 编码是 CPU 活，不占 async worker；克隆一份进闭包，
            // 原图留在原地，转码失败时还能原图兜底
            let source = original.clone();
            tokio::task::spawn_blocking(move || encode_variant(&source, target_width, accept_webp))
                .await
        };
        let (bytes, out_type) = match encoded {
            Ok(Ok(pair)) => pair,
            Ok(Err(err)) => {
                // 派生图生成失败不致命：原图仍然完整可用，只是没省成流
                tracing::warn!("图片变体生成失败 id={id} w={target_width} err={err}");
                drop(_key_guard);
                self.release_load_lock(&lock_key, &lock);
                return Ok((original, content_type));
            }
            Err(e) => {
                drop(_key_guard);
                self.release_load_lock(&lock_key, &lock);
                return Err(AppError::Internal(e.into()));
            }
        };
        // 落盘失败只影响下次命中，不影响本次响应；容量记账与原图同一套
        if let Err(e) = write_atomic(&variant, &bytes).await {
            tracing::warn!("图片变体落盘失败 id={id} w={target_width} err={e:?}");
        } else {
            enforce_flat_dir_capacity(&self.dir(), self.limit_bytes, bytes.len() as u64).await;
        }
        drop(_key_guard);
        self.release_load_lock(&lock_key, &lock);
        Ok((bytes, out_type))
    }

    /// 抓上游 + 转码 + 落盘 + 更新记录（在键锁内执行）。
    async fn fetch_and_store(
        &self,
        id: &str,
        record: &mut ImageRecord,
    ) -> Result<(Vec<u8>, String), AppError> {
        let res = self.fetch(&record.source_url).await.map_err(|err| {
            tracing::warn!(
                "图片抓取失败 id={} url={} err={:?}",
                id,
                // 上游地址带时效签名（x-signature 等），日志只留脱敏后的地址
                crate::crawler::url_guard::redact_url_for_log(&record.source_url),
                err
            );
            AppError::NotFound("图片抓取失败".to_string())
        })?;
        // HEIC 解码 + JPEG 编码是百毫秒级的 CPU 活，不能占着 async worker
        let (bytes, content_type, ext) = tokio::task::spawn_blocking(move || transcode(res))
            .await
            .map_err(|e| AppError::Internal(e.into()))??;
        // 本体 tmp + rename 原子落盘：并发读侧要么看到旧图、要么看到完整
        // 新图，不会读到半截文件（`<id>.bin.<n>.tmp` 与目标同目录）
        let path = self.body_path(id);
        write_atomic(&path, &bytes)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        // 新原图已落盘：旧原图派生的变体全部过期，一并清掉（后续按需重建）。
        // 必须在写成功之后——写失败时旧原图仍是当前内容，旧变体还有效。
        self.remove_variants(id).await;
        enforce_flat_dir_capacity(&self.dir(), self.limit_bytes, bytes.len() as u64).await;

        record.content_type = content_type.clone();
        record.ext = ext;
        record.updated_at = now_secs();
        record.failed_at = None;
        self.save_record(record).await;
        Ok((bytes, content_type))
    }

    /// 登记并返回 id 的取图键锁：已存在则复用，否则新建。
    fn acquire_load_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.load_locks.acquire(id)
    }

    /// 用完清理：表里还是这把锁、且没有别的等待者持有克隆时摘掉条目。
    fn release_load_lock(&self, id: &str, lock: &Arc<tokio::sync::Mutex<()>>) {
        self.load_locks.release(id, lock);
    }

    async fn fetch(&self, url: &str) -> Result<FetchedImage, AppError> {
        // 出站守卫：上游地址来自书源/用户输入，不过守卫就能借服务端探内网
        // （`ALLOW_PRIVATE_NETWORK=false` 时这里曾是唯一漏检的出站口）
        let url = crate::crawler::url_guard::ensure_outbound_url_str_allowed(url)
            .await
            .map_err(AppError::Blocked)?;
        // 与封面抓取同一套头：有些站点按 UA / Referer 做防盗链
        let referer = url
            .host_str()
            .map(|host| format!("{}://{}", url.scheme(), host));
        let http = self.http.client_for("public").map_err(AppError::Internal)?;
        let mut req = http
            .get(url.as_str())
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .header(
                "Accept",
                "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            );
        if let Some(referer) = referer {
            req = req.header("Referer", referer);
        }
        let res = req.send().await.map_err(|e| AppError::Internal(e.into()))?;
        if !res.status().is_success() {
            return Err(AppError::NotFound(format!(
                "上游返回 {}",
                res.status().as_u16()
            )));
        }
        let content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        let bytes = read_body_limited(res, MAX_RESPONSE_BYTES)
            .await
            .map_err(AppError::Internal)?;
        Ok(FetchedImage {
            bytes: bytes.to_vec(),
            content_type,
            url: url.to_string(),
        })
    }
}

struct FetchedImage {
    bytes: Vec<u8>,
    content_type: Option<String>,
    url: String,
}

/// 需要转码时把 HEIC/HEIF 转成 JPEG，其余格式原样返回。
fn transcode(image: FetchedImage) -> Result<(Vec<u8>, String, String), AppError> {
    let upstream = image
        .content_type
        .as_deref()
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase());
    let ext = file_ext(&image.url);
    let is_heic = matches!(upstream.as_deref(), Some("image/heic") | Some("image/heif"))
        || matches!(ext.as_deref(), Some("heic") | Some("heif"));
    if !is_heic {
        let content_type = safe_content_type(upstream.as_deref(), ext.as_deref());
        let ext = ext.unwrap_or_else(|| "bin".to_string());
        return Ok((image.bytes, content_type, ext));
    }

    // 浏览器普遍解不了 HEIC（Chrome 直接 img.onerror），统一转 JPEG
    let rgb = decode_heic(&image.bytes).map_err(|err| {
        tracing::warn!(
            "HEIC 转码失败 url={} err={err}",
            crate::crawler::url_guard::redact_url_for_log(&image.url)
        );
        AppError::NotFound("图片转码失败".to_string())
    })?;
    let (data, width, height) = rgb;
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
        .encode(&data, width, height, jpeg_encoder::ColorType::Rgb)
        .map_err(|err| AppError::Internal(anyhow::anyhow!("JPEG 编码失败: {err}")))?;
    Ok((out, "image/jpeg".to_string(), "jpg".to_string()))
}

/// 解码 HEIC，返回 `(RGB 像素, 宽, 高)`。
fn decode_heic(bytes: &[u8]) -> Result<(Vec<u8>, u16, u16), String> {
    let options = heic_rs::DecodeOptions::default().with_layout(heic_rs::PixelLayout::Rgb8);
    let image = heic_rs::decode(bytes, &options).map_err(|err| format!("{err:?}"))?;
    if image.width == 0
        || image.height == 0
        || image.width > u16::MAX as u32
        || image.height > u16::MAX as u32
    {
        return Err(format!("尺寸异常 {}x{}", image.width, image.height));
    }
    let expected = image.width as usize * image.height as usize * 3;
    if image.data.len() < expected {
        return Err(format!("像素数据不完整：{} < {expected}", image.data.len()));
    }
    Ok((image.data, image.width as u16, image.height as u16))
}

/// 只解析文件头拿图片宽度（不解码像素）。
fn read_width(bytes: &[u8]) -> Result<u32, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, _) = reader.into_dimensions().map_err(|e| e.to_string())?;
    Ok(width)
}

/// 生产解码上限：单边 16384、解码期间总分配 256MiB。
/// `Limits` 是 `#[non_exhaustive]`，只能从 `default()` 起步逐字段覆盖。
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(DECODE_MAX_DIMENSION);
    limits.max_image_height = Some(DECODE_MAX_DIMENSION);
    limits.max_alloc = Some(DECODE_MAX_ALLOC_BYTES);
    limits
}

/// 按内容猜格式并带 `Limits` 解码；超限与解码失败统一 `Err(String)`。
///
/// 与 `image::load_from_memory` 的差别只在上限：后者用 crate 默认值
/// （总分配 512MiB、无单边尺寸限制），这里收紧为 [`decode_limits`] 一类的
/// 显式 `Limits`，由调用方决定传生产常量还是测试用的更小值。
fn decode_with_limits(bytes: &[u8], limits: image::Limits) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    reader.limits(limits);
    let reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
    reader.decode().map_err(|e| e.to_string())
}

/// 派生图内容类型。
fn variant_content_type(webp: bool) -> String {
    if webp {
        "image/webp".to_string()
    } else {
        "image/jpeg".to_string()
    }
}

/// 解码原图 →（可选）Triangle 缩放 → RGBA → 有损 webp（质量 80）/ JPEG（质量 85）。
///
/// 只处理 JPEG/PNG/BMP（调用方已按 content_type 过滤）；解码与编码都在
/// `spawn_blocking` 里执行。解码带尺寸/内存上限（`decode_limits`）：超限、
/// 坏数据与普通解码失败一样走 `Err`，调用方回退原图直出，不 500。
fn encode_variant(
    original: &[u8],
    target_width: u32,
    accept_webp: bool,
) -> Result<(Vec<u8>, String), String> {
    let img = decode_with_limits(original, decode_limits())?;
    let img = if target_width < img.width() {
        let ratio = target_width as f64 / img.width() as f64;
        let target_height = ((img.height() as f64 * ratio).round() as u32).max(1);
        image::DynamicImage::ImageRgba8(image::imageops::resize(
            &img.to_rgba8(),
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        ))
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    let pixels = rgba.as_raw();
    if accept_webp {
        // 单边超 16383 会被 libwebp 拒绝（webp crate 对此是 unwrap panic），
        // 提前判掉走 Err → 调用方原图兜底；条漫长图（高度轻松超限）不至于整张 500
        if width > WEBP_MAX_DIMENSION || height > WEBP_MAX_DIMENSION {
            return Err(format!("尺寸超出 webp 编码上限 {width}x{height}"));
        }
        let encoded = webp::Encoder::from_rgba(pixels, width, height).encode(WEBP_QUALITY);
        Ok((encoded.to_vec(), variant_content_type(true)))
    } else {
        // JPEG 路径只在缩小时走到（宽度 ≤ 2048），但极端长图的高度仍可能超限
        if width > u16::MAX as u32 || height > u16::MAX as u32 {
            return Err(format!("尺寸超出 JPEG 编码上限 {width}x{height}"));
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
            .encode(
                pixels,
                width as u16,
                height as u16,
                jpeg_encoder::ColorType::Rgba,
            )
            .map_err(|e| e.to_string())?;
        Ok((out, variant_content_type(false)))
    }
}

/// 去掉 URL 的查询串与片段。
fn strip_query(url: &str) -> String {
    let url = url.split('#').next().unwrap_or(url);
    url.split('?').next().unwrap_or(url).trim().to_string()
}

fn file_ext(url: &str) -> Option<String> {
    let path = strip_query(url);
    let name = path.rsplit('/').next()?;
    let pos = name.rfind('.')?;
    let ext = name[pos + 1..].trim().to_ascii_lowercase();
    (!ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(ext)
}

/// 与封面一致的白名单：不认识的类型一律 `application/octet-stream`，
/// 免得把 `text/html`、`image/svg+xml` 这类能当脚本跑的内容原样吐给浏览器。
fn safe_content_type(upstream: Option<&str>, ext: Option<&str>) -> String {
    const ALLOWED: &[&str] = &[
        "image/jpeg",
        "image/png",
        "image/webp",
        "image/gif",
        "image/avif",
        "image/bmp",
        "image/x-icon",
        "image/vnd.microsoft.icon",
    ];
    if let Some(ct) = upstream {
        if ALLOWED.contains(&ct) {
            return ct.to_string();
        }
    }
    let by_ext = match ext.unwrap_or_default() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" | "awebp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    by_ext.to_string()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 待改写的封面：JSON 指针 + 上游地址 + 所属书籍。
struct CoverTarget {
    pointer: String,
    url: String,
    book_id: Option<String>,
}

/// 同步遍历响应，收集需要改写的 `coverUrl`。
///
/// 所属书籍取同一层对象的兄弟字段 `bookId`——书架、搜索、探索返回的
/// 书籍对象都是这个形状。
fn collect_cover_targets(value: &Value, pointer: &str, out: &mut Vec<CoverTarget>) {
    match value {
        Value::Object(map) => {
            let book_id = map
                .get("bookId")
                .and_then(Value::as_str)
                .map(str::to_string);
            for (key, child) in map {
                let child_pointer = format!("{pointer}/{}", escape_pointer(key));
                match child {
                    Value::String(url)
                        if key == "coverUrl"
                            && (url.starts_with("http://") || url.starts_with("https://")) =>
                    {
                        out.push(CoverTarget {
                            pointer: child_pointer,
                            url: url.clone(),
                            book_id: book_id.clone(),
                        });
                    }
                    Value::Object(_) | Value::Array(_) => {
                        collect_cover_targets(child, &child_pointer, out)
                    }
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let child_pointer = format!("{pointer}/{index}");
                if matches!(child, Value::Object(_) | Value::Array(_)) {
                    collect_cover_targets(child, &child_pointer, out);
                }
            }
        }
        _ => {}
    }
}

/// RFC 6901 指针转义。
fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// 待改写的图片数组：JSON 指针 + 候选地址。
struct ImageListTarget {
    pointer: String,
    urls: Vec<String>,
}

/// 收集形如 `"images": ["https://…", …]` 的字符串数组（评论配图就是这个形状）。
///
/// 章节配图接口里的 `images` 是对象数组，形状不同，不会被这里命中——
/// 那条路在 handler 里按类型直接改写。
fn collect_string_arrays(
    value: &Value,
    pointer: &str,
    key_filter: &str,
    out: &mut Vec<ImageListTarget>,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_pointer = format!("{pointer}/{}", escape_pointer(key));
                match child {
                    Value::Array(items) if key == key_filter => {
                        let urls: Vec<String> = items
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
                            .map(str::to_string)
                            .collect();
                        if !urls.is_empty() {
                            out.push(ImageListTarget {
                                pointer: child_pointer,
                                urls,
                            });
                        }
                    }
                    Value::Object(_) | Value::Array(_) => {
                        collect_string_arrays(child, &child_pointer, key_filter, out)
                    }
                    _ => {}
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let child_pointer = format!("{pointer}/{index}");
                if matches!(child, Value::Object(_) | Value::Array(_)) {
                    collect_string_arrays(child, &child_pointer, key_filter, out);
                }
            }
        }
        _ => {}
    }
}

/// 浏览器能直接渲染的位图扩展名（与前端 `RENDERABLE_IMAGE_EXTENSIONS` 一致）。
const RENDERABLE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "awebp", "avif", "bmp", "ico",
];

/// 从同一张图的多个格式变体里挑一个能渲染的；都不认识就取第一个
/// （HEIC 会由管道转成 JPEG，所以「取第一个」也是可用的兜底）。
fn pick_renderable(urls: &[String]) -> Option<&String> {
    urls.iter()
        .find(|url| {
            file_ext(url)
                .map(|ext| RENDERABLE_EXTS.contains(&ext.as_str()))
                .unwrap_or(false)
        })
        .or_else(|| urls.first())
}

/// 图片缓存目录（供清理接口使用）。
pub fn image_cache_dir(storage_dir: &Path) -> PathBuf {
    storage_dir.join(IMAGE_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_key_ignores_signature() {
        let a = ImageService::image_key(
            "https://p3-reading-sign.fqnovelpic.com/novel-pic/abc~tplv-x.heic?lk3s=1&x-expires=2&x-signature=3",
        );
        let b = ImageService::image_key(
            "https://p3-reading-sign.fqnovelpic.com/novel-pic/abc~tplv-x.heic?lk3s=9&x-expires=8&x-signature=7",
        );
        assert_eq!(a, b, "同一张图不同签名应当是同一个 id");
        let c = ImageService::image_key(
            "https://p3-reading-sign.fqnovelpic.com/novel-pic/other~tplv-x.heic",
        );
        assert_ne!(a, c, "不同路径应当是不同 id");
    }

    /// 登记与按书删除两端都把 book_id 归一为 canonical：别名登记的记录
    /// 按 canonical 删除删得到；非法 id 是 no-op。
    #[tokio::test]
    async fn book_id_is_canonicalized_on_register_and_remove() {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-img-alias-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        let service = ImageService::new(
            crate::crawler::http_client::HttpClient::new(5, None).unwrap(),
            dir.to_str().unwrap(),
            0,
        );
        let id = service
            .register("cover", "https://img.example/x.jpg", Some("book_07"))
            .await
            .unwrap();
        assert_eq!(
            service.record(&id).await.unwrap().book_id.as_deref(),
            Some("book_7"),
            "别名登记应存 canonical id"
        );
        assert_eq!(service.remove_by_book_id("../..").await, 0);
        assert_eq!(service.remove_by_book_id("book_7").await, 1);
        assert!(service.record(&id).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同一 id 并发写记录：最终落盘的必须是某一方的完整 JSON，
    /// 互踩同一个固定 tmp 文件会产生截断/混杂内容（解析失败）。
    #[tokio::test]
    async fn concurrent_save_record_lands_intact() {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-img-conc-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        let service = Arc::new(ImageService::new(
            crate::crawler::http_client::HttpClient::new(5, None).unwrap(),
            dir.to_str().unwrap(),
            0,
        ));
        let id = "0123456789abcdef0123456789abcdef".to_string();
        let mut handles = Vec::new();
        for i in 0..16i64 {
            let service = Arc::clone(&service);
            let id = id.clone();
            handles.push(tokio::spawn(async move {
                let record = ImageRecord {
                    id,
                    kind: "cover".to_string(),
                    source_url: format!("https://img.example/x-{i}.jpg"),
                    book_id: Some("book_1".to_string()),
                    content_type: "image/jpeg".to_string(),
                    ext: "jpg".to_string(),
                    updated_at: i,
                    failed_at: None,
                };
                service.save_record(&record).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let record = service.record(&id).await.expect("记录必须是完整 JSON");
        assert_eq!(record.id, id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn route_round_trip() {
        let id = "0123456789abcdef0123456789abcdef";
        let route = ImageService::route(id);
        assert_eq!(route, "/reader3/image/0123456789abcdef0123456789abcdef");
        assert_eq!(ImageService::id_from_route(&route), Some(id));
        assert_eq!(
            ImageService::id_from_route("/reader3/image/abc?accessToken=t"),
            Some("abc")
        );
        assert_eq!(ImageService::id_from_route("/reader3/cover?path=x"), None);
    }

    #[test]
    fn content_type_whitelist_blocks_html() {
        assert_eq!(safe_content_type(Some("image/jpeg"), None), "image/jpeg");
        assert_eq!(
            safe_content_type(Some("text/html"), Some("html")),
            "application/octet-stream"
        );
        assert_eq!(
            safe_content_type(Some("image/svg+xml"), Some("svg")),
            "application/octet-stream"
        );
        // 上游没给类型时按扩展名猜
        assert_eq!(safe_content_type(None, Some("webp")), "image/webp");
    }

    #[test]
    fn non_heic_images_pass_through() {
        let image = FetchedImage {
            bytes: vec![1, 2, 3],
            content_type: Some("image/jpeg".to_string()),
            url: "https://host/a.jpg?sign=1".to_string(),
        };
        let (bytes, content_type, ext) = transcode(image).unwrap();
        assert_eq!(bytes, vec![1, 2, 3]);
        assert_eq!(content_type, "image/jpeg");
        assert_eq!(ext, "jpg");
    }

    #[test]
    fn heic_images_are_transcoded() {
        let fixture = include_bytes!("../../tests/fixtures/tiny.heic");
        let image = FetchedImage {
            bytes: fixture.to_vec(),
            content_type: Some("image/heic".to_string()),
            url: "https://host/a.heic?sign=1".to_string(),
        };
        let (bytes, content_type, ext) = transcode(image).unwrap();
        assert_eq!(content_type, "image/jpeg");
        assert_eq!(ext, "jpg");
        // JPEG 魔数：FF D8 开头、FF D9 结尾
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "应当输出 JPEG");
        assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn broken_heic_reports_failure() {
        let image = FetchedImage {
            bytes: b"not a heic".to_vec(),
            content_type: Some("image/heic".to_string()),
            url: "https://host/a.heic".to_string(),
        };
        assert!(transcode(image).is_err());
    }

    #[test]
    fn pick_renderable_prefers_renderable_variants() {
        // 番茄同时给 HEIC 与 JPEG：挑能渲染的那张
        let urls = vec![
            "https://host/a.heic?sign=1".to_string(),
            "https://host/a.jpeg?sign=2".to_string(),
        ];
        assert_eq!(pick_renderable(&urls).unwrap(), &urls[1]);

        // 只有 HEIC 时退回第一个（管道会把它转成 JPEG）
        let heic_only = vec!["https://host/a.heic".to_string()];
        assert_eq!(pick_renderable(&heic_only).unwrap(), &heic_only[0]);

        assert!(pick_renderable(&[]).is_none());
    }

    #[test]
    fn string_arrays_are_collected_for_rewrite() {
        let value = json!({
            "data": { "items": [
                { "content": "评论", "images": ["https://host/a.heic", "https://host/a.jpeg"] },
                { "content": "没图", "images": [] }
            ]},
            "other": { "images": [{"url": "https://host/b.jpg"}] }
        });
        let mut targets = Vec::new();
        collect_string_arrays(&value, "", "images", &mut targets);
        assert_eq!(targets.len(), 1, "空数组与对象数组都不该被收集");
        assert_eq!(targets[0].pointer, "/data/items/0/images");
        assert_eq!(targets[0].urls.len(), 2);
    }

    #[test]
    fn cover_targets_carry_book_context() {
        let mut value = json!({
            "books": [{
                "bookId": "book_7",
                "bookUrl": "http://src/info?book_id=1",
                "coverUrl": "https://img.example/a.heic?sig=1",
                "customCoverUrl": "https://user.example/mine.png"
            }],
            "coverUrl": "https://img.example/b.jpg"
        });
        let mut targets = Vec::new();
        collect_cover_targets(&value, "", &mut targets);
        assert_eq!(targets.len(), 2, "customCoverUrl 不该被收集");
        let first = &targets[0];
        assert_eq!(first.pointer, "/books/0/coverUrl");
        assert_eq!(first.book_id.as_deref(), Some("book_7"));

        // 改写只动 coverUrl
        let pointer = targets[0].pointer.clone();
        *value.pointer_mut(&pointer).unwrap() = json!("/reader3/image/deadbeef");
        assert_eq!(
            value["books"][0]["coverUrl"],
            json!("/reader3/image/deadbeef")
        );
        assert_eq!(
            value["books"][0]["customCoverUrl"],
            json!("https://user.example/mine.png")
        );
    }

    // ---------- load_variant：按宽度派生 + webp 协商 ----------

    /// 生成一张着色 PNG（带横向渐变，缩放后肉眼可辨）。
    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([
                ((x * 255) / width.max(1)) as u8,
                (y % 256) as u8,
                0x80,
                0xFF,
            ])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// 建临时 ImageService，直接落一张已就位的图（record + body，不走网络）。
    async fn service_with_stored_image(
        content_type: &str,
        bytes: Vec<u8>,
    ) -> (ImageService, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-img-variant-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let service = ImageService::new(
            crate::crawler::http_client::HttpClient::new(5, None).unwrap(),
            dir.to_str().unwrap(),
            0,
        );
        let id = "0123456789abcdef0123456789abcdef".to_string();
        service
            .save_record(&ImageRecord {
                id: id.clone(),
                kind: "chapter".to_string(),
                source_url: "https://img.example/x.png".to_string(),
                book_id: None,
                content_type: content_type.to_string(),
                ext: "png".to_string(),
                updated_at: 0,
                failed_at: None,
            })
            .await;
        std::fs::write(service.body_path(&id), bytes).unwrap();
        (service, id, dir)
    }

    /// storage 根目录下的派生文件（<id>.w*.webp/jpg）名字列表。
    fn derived_files(storage_dir: &Path) -> Vec<String> {
        let dir = storage_dir.join(IMAGE_DIR);
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("0123456789abcdef0123456789abcdef.w"))
            .collect()
    }

    /// w=64 + Accept webp：输出可解码的有损 webp、宽 64，派生文件按实际宽度落盘。
    #[tokio::test]
    async fn load_variant_scales_and_encodes_webp() {
        let (service, id, dir) = service_with_stored_image("image/png", make_png(128, 128)).await;
        let (bytes, content_type) = service.load_variant(&id, Some(64), true).await.unwrap();
        assert_eq!(content_type, "image/webp");
        let decoded = webp::Decoder::new(&bytes)
            .decode()
            .expect("应当是可解码的 webp");
        assert_eq!((decoded.width(), decoded.height()), (64, 64));
        assert_eq!(
            derived_files(&dir),
            vec![format!("{id}.w64.webp")],
            "派生文件应按实际宽度落盘"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// w=64 不要 webp：转成 JPEG（质量 85），宽 64。
    #[tokio::test]
    async fn load_variant_scales_and_encodes_jpeg_without_webp() {
        let (service, id, dir) = service_with_stored_image("image/png", make_png(128, 128)).await;
        let (bytes, content_type) = service.load_variant(&id, Some(64), false).await.unwrap();
        assert_eq!(content_type, "image/jpeg");
        let decoded = image::load_from_memory(&bytes).expect("应当是可解码的 JPEG");
        assert_eq!(decoded.width(), 64);
        assert_eq!(
            derived_files(&dir),
            vec![format!("{id}.w64.jpg")],
            "不要 webp 时派生 JPEG"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 请求宽度 >= 原图宽度：不放大，原图直出，也不产生派生缓存。
    #[tokio::test]
    async fn load_variant_never_upscales() {
        let original = make_png(128, 128);
        let (service, id, dir) = service_with_stored_image("image/png", original.clone()).await;
        let (bytes, content_type) = service.load_variant(&id, Some(512), false).await.unwrap();
        assert_eq!(content_type, "image/png", "不缩放也不要 webp 时应原图直出");
        assert_eq!(bytes, original, "字节应与原图一致");
        assert!(derived_files(&dir).is_empty(), "不该产生派生缓存");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无 width 且不要 webp：原图直出的快路径，不解析尺寸、不落派生文件。
    #[tokio::test]
    async fn load_variant_without_width_or_webp_returns_original() {
        let original = make_png(128, 128);
        let (service, id, dir) = service_with_stored_image("image/png", original.clone()).await;
        let (bytes, content_type) = service.load_variant(&id, None, false).await.unwrap();
        assert_eq!(bytes, original);
        assert_eq!(content_type, "image/png");
        assert!(derived_files(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无 width 但要 webp：保持原始宽高转 webp（不缩放）。
    #[tokio::test]
    async fn load_variant_webp_full_size_without_width() {
        let (service, id, dir) = service_with_stored_image("image/png", make_png(96, 48)).await;
        let (bytes, content_type) = service.load_variant(&id, None, true).await.unwrap();
        assert_eq!(content_type, "image/webp");
        let decoded = webp::Decoder::new(&bytes).decode().expect("可解码 webp");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (96, 48),
            "不缩放，保持原始宽高"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GIF（动画）原样透传：不缩放、不转码，避免破坏动画。
    #[tokio::test]
    async fn load_variant_passes_gif_through() {
        let gif: Vec<u8> = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff,\
             !\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;"
            .to_vec();
        let (service, id, dir) = service_with_stored_image("image/gif", gif.clone()).await;
        let (bytes, content_type) = service.load_variant(&id, Some(64), true).await.unwrap();
        assert_eq!(content_type, "image/gif");
        assert_eq!(bytes, gif);
        assert!(derived_files(&dir).is_empty(), "透传不该产生派生缓存");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 宽度 clamp：过小的请求宽度（w=1）抬到下限 64。
    #[tokio::test]
    async fn load_variant_clamps_width_floor() {
        let (service, id, dir) = service_with_stored_image("image/png", make_png(128, 128)).await;
        let (bytes, content_type) = service.load_variant(&id, Some(1), false).await.unwrap();
        assert_eq!(content_type, "image/jpeg");
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 64, "过小的宽度应被 clamp 到 64");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 派生缓存命中：第二次请求直接读派生文件，不再重新编码。
    #[tokio::test]
    async fn load_variant_reuses_derived_cache() {
        let (service, id, dir) = service_with_stored_image("image/png", make_png(128, 128)).await;
        let _ = service.load_variant(&id, Some(64), true).await.unwrap();
        let marker = b"cached-bytes".to_vec();
        std::fs::write(dir.join(IMAGE_DIR).join(format!("{id}.w64.webp")), &marker).unwrap();
        let (bytes, content_type) = service.load_variant(&id, Some(64), true).await.unwrap();
        assert_eq!(bytes, marker, "第二次应直接读派生文件");
        assert_eq!(content_type, "image/webp");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- 派生图失效：按书删除 / 原图重抓时清理 ----------

    /// 建一个带空图片目录的临时 ImageService（不走网络）。
    async fn service_with_empty_dir(name: &str) -> (ImageService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-img-{name}-{}-{}",
            std::process::id(),
            crate::util::crypto::random_string(6)
        ));
        std::fs::create_dir_all(dir.join(IMAGE_DIR)).unwrap();
        let service = ImageService::new(
            crate::crawler::http_client::HttpClient::new(5, None).unwrap(),
            dir.to_str().unwrap(),
            0,
        );
        (service, dir)
    }

    fn fake_record(id: &str, book_id: &str) -> ImageRecord {
        ImageRecord {
            id: id.to_string(),
            kind: "cover".to_string(),
            source_url: format!("https://img.example/{id}.jpg"),
            book_id: Some(book_id.to_string()),
            content_type: "image/jpeg".to_string(),
            ext: "jpg".to_string(),
            updated_at: 0,
            failed_at: None,
        }
    }

    fn image_dir(storage_dir: &Path) -> PathBuf {
        storage_dir.join(IMAGE_DIR)
    }

    /// 按书删除：记录、本体、派生图三者一起消失；别家的图片原样保留。
    #[tokio::test]
    async fn remove_by_book_id_also_removes_variants() {
        let (service, dir) = service_with_empty_dir("purge-book").await;
        let id = "0123456789abcdef0123456789abcdef";
        let other = "ffffffffffffffffffffffffffffffff";
        service.save_record(&fake_record(id, "book_7")).await;
        service.save_record(&fake_record(other, "book_8")).await;
        std::fs::write(service.body_path(id), b"body").unwrap();
        std::fs::write(image_dir(&dir).join(format!("{id}.w64.webp")), b"v").unwrap();
        std::fs::write(image_dir(&dir).join(format!("{id}.w128.jpg")), b"v").unwrap();
        std::fs::write(service.body_path(other), b"body").unwrap();
        std::fs::write(image_dir(&dir).join(format!("{other}.w64.webp")), b"v").unwrap();

        assert_eq!(service.remove_by_book_id("book_7").await, 1);

        let names: Vec<String> = std::fs::read_dir(image_dir(&dir))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&format!("{other}.json")));
        assert!(names.contains(&format!("{other}.bin")));
        assert!(
            names.contains(&format!("{other}.w64.webp")),
            "别家的派生图不该被删"
        );
        assert_eq!(
            names.len(),
            3,
            "被删书的记录/本体/派生图应全部消失: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// remove_variants 只删本 id 的 `.w*.webp/jpg`：记录、本体、别的 id、
    /// 其他扩展名（含原子写留下的 .tmp 孤儿）都不碰。
    #[tokio::test]
    async fn remove_variants_only_deletes_own_webp_and_jpg() {
        let (service, dir) = service_with_empty_dir("purge-variant").await;
        let id = "0123456789abcdef0123456789abcdef";
        let keep = [
            format!("{id}.json"),
            format!("{id}.bin"),
            format!("{id}.w64.png"),          // 非 webp/jpg 扩展名不删
            format!("{id}.w64.webp.1.2.tmp"), // 原子写的 tmp 孤儿不删
            "ffffffffffffffffffffffffffffffff.w64.webp".to_string(), // 别的 id
            "ffffffffffffffffffffffffffffffff.bin".to_string(),
        ];
        for name in &keep {
            std::fs::write(image_dir(&dir).join(name), b"keep").unwrap();
        }
        let gone = [
            format!("{id}.w64.webp"),
            format!("{id}.w128.jpg"),
            format!("{id}.w2048.webp"),
        ];
        for name in &gone {
            std::fs::write(image_dir(&dir).join(name), b"gone").unwrap();
        }

        assert_eq!(service.remove_variants(id).await, 3);

        for name in &keep {
            assert!(image_dir(&dir).join(name).exists(), "{name} 不该被删");
        }
        for name in &gone {
            assert!(!image_dir(&dir).join(name).exists(), "{name} 应被删");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// webp 编码器的单边尺寸上限是 libwebp 的 `WEBP_MAX_DIMENSION`（16383），
    /// 而 webp crate 0.3 的 `encode()` 对超限是 unwrap panic——不提前判掉的话，
    /// panic 会穿透 `spawn_blocking` 变成 JoinError → 500。这里断言超限走
    /// `Err`（调用方回退原图）而不是 panic。
    #[test]
    fn encode_variant_rejects_webp_oversize_dimensions() {
        // 条漫长图：高度刚好超限（16384 > 16383）
        let tall = make_png(4, 16384);
        assert!(
            encode_variant(&tall, 4, true).is_err(),
            "高度超出 webp 上限应返回 Err 而非 panic"
        );
        // 宽扁图：不缩放路径（target_width == 原宽）下单边同样可能超限
        let wide = make_png(16384, 4);
        assert!(
            encode_variant(&wide, 16384, true).is_err(),
            "宽度超出 webp 上限应返回 Err 而非 panic"
        );
        // 对照：两边都在限内的小图正常编码
        assert!(encode_variant(&make_png(8, 8), 8, true).is_ok());
    }

    /// 服务级兜底：Chrome `<img>` 默认 Accept 带 `image/webp`，条漫长图
    /// （单边 > 16383）请求 webp 变体时必须回退原图，不能 500。
    #[tokio::test]
    async fn load_variant_falls_back_to_original_when_webp_dimension_exceeded() {
        let original = make_png(32, 16384);
        let (service, id, dir) = service_with_stored_image("image/png", original.clone()).await;
        let (bytes, content_type) = service
            .load_variant(&id, None, true)
            .await
            .expect("超限尺寸应回退原图而不是 Internal 错误");
        assert_eq!(bytes, original, "回退时应原图直出");
        assert_eq!(content_type, "image/png");
        assert!(derived_files(&dir).is_empty(), "编码失败不该落派生文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- 解码上限：单边尺寸与总分配（decode_with_limits） ----------

    /// 2x2 PNG 在生产上限（decode_limits）内正常解码。
    #[test]
    fn decode_with_limits_accepts_small_png() {
        let png = make_png(2, 2);
        let img = decode_with_limits(&png, decode_limits()).expect("2x2 PNG 应当正常解码");
        assert_eq!((img.width(), img.height()), (2, 2));
    }

    /// 同一张 2x2 PNG，把单边上限压到 1：解码被拒（Err），不能 panic。
    #[test]
    fn decode_with_limits_rejects_oversize_dimension() {
        let png = make_png(2, 2);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(1);
        assert!(
            decode_with_limits(&png, limits).is_err(),
            "宽度超出 max_image_width 应当返回 Err"
        );
    }

    /// max_alloc 覆盖解码期总分配：2x2 RGBA 需 16 字节，压到 8 就该被拒。
    #[test]
    fn decode_with_limits_rejects_oversize_alloc() {
        let png = make_png(2, 2);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(8);
        assert!(
            decode_with_limits(&png, limits).is_err(),
            "总分配超出 max_alloc 应当返回 Err"
        );
    }

    /// 非图片与截断字节都返回 Err（不 panic）：前者格式猜不出，后者
    /// 只剩 PNG 头 + IHDR、像素数据缺失。
    #[test]
    fn decode_with_limits_rejects_garbage_and_truncated_bytes() {
        assert!(decode_with_limits(b"not an image at all", decode_limits()).is_err());
        let full = make_png(8, 8);
        let header_only = &full[..8 + 25]; // PNG 签名 + IHDR 块
        assert!(
            decode_with_limits(header_only, decode_limits()).is_err(),
            "缺少 IDAT 的截断 PNG 应当返回 Err"
        );
    }
}
