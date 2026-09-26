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
//! - **映射表**：`<storage>/cache/image/<id>.json` 是记录（上游地址 + 书籍上下文），
//!   `<id>.bin` 是图片本体。目录本身就是「id → 记录」的映射表，不需要额外的库表。

use crate::crawler::{
    fetcher::{read_body_limited, MAX_RESPONSE_BYTES},
    http_client::HttpClient,
};
use crate::error::error::AppError;
use crate::storage::cache::file_cache::enforce_flat_dir_capacity;
use crate::util::hash::md5_hex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

/// 前端取图用的路径前缀。
pub const IMAGE_ROUTE_PREFIX: &str = "/reader3/image/";

/// 映射表与图片本体的落盘目录（相对 storage）。
const IMAGE_DIR: &str = "cache/image";

/// JPEG 转码质量：封面/插图这类展示用图，85 已经看不出损失。
const JPEG_QUALITY: u8 = 85;

/// 一条图片映射记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecord {
    pub id: String,
    /// 用途，目前只有 `cover`；后续章节配图、评论配图可以复用同一套。
    pub kind: String,
    /// 最近一次已知的上游地址（含签名）。
    pub source_url: String,
    /// 登记时的书籍上下文，用于上游地址失效时回源刷新。
    #[serde(default)]
    pub book_url: Option<String>,
    #[serde(default)]
    pub book_source_url: Option<String>,
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
}

impl ImageService {
    pub fn new(http: HttpClient, storage_dir: impl Into<PathBuf>, limit_bytes: u64) -> Self {
        Self {
            http,
            storage_dir: storage_dir.into(),
            limit_bytes,
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

    fn record_path(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.json"))
    }

    /// 读一条映射记录。
    pub async fn record(&self, id: &str) -> Option<ImageRecord> {
        let data = fs::read(self.record_path(id)).await.ok()?;
        serde_json::from_slice(&data).ok()
    }

    async fn save_record(&self, record: &ImageRecord) {
        let dir = self.dir();
        if fs::create_dir_all(&dir).await.is_err() {
            return;
        }
        let Ok(data) = serde_json::to_vec(record) else {
            return;
        };
        // 先写临时文件再改名：并发读到的要么是旧记录、要么是新记录，不会是半截
        let path = self.record_path(&record.id);
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, &data).await.is_ok() {
            let _ = fs::rename(&tmp, &path).await;
        }
    }

    /// 登记一张图，返回它的 id。
    ///
    /// 上游地址没变就不重复写盘；`book_url` / `book_source_url` 用于日后
    /// 地址失效时回源刷新（见 `update_source_url`）。
    pub async fn register(
        &self,
        kind: &str,
        url: &str,
        book_url: Option<&str>,
        book_source_url: Option<&str>,
    ) -> Option<String> {
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
            book_url: book_url
                .map(str::to_string)
                .or(existing.as_ref().and_then(|r| r.book_url.clone())),
            book_source_url: book_source_url
                .map(str::to_string)
                .or(existing.as_ref().and_then(|r| r.book_source_url.clone())),
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
            let book_url = target.book_url.as_deref();
            let source_url = target.book_source_url.as_deref();
            let Some(id) = self
                .register("cover", &target.url, book_url, source_url)
                .await
            else {
                continue;
            };
            if let Some(slot) = value.pointer_mut(&target.pointer) {
                *slot = Value::String(Self::route(&id));
            }
        }
    }

    /// 取图：缓存命中直接返回，否则抓上游（必要时转码）后落盘。
    pub async fn load(&self, id: &str) -> Result<(Vec<u8>, String), AppError> {
        let Some(mut record) = self.record(id).await else {
            return Err(AppError::NotFound("图片不存在".to_string()));
        };
        if let Ok(bytes) = fs::read(self.body_path(id)).await {
            return Ok((bytes, record.content_type));
        }
        let res = self.fetch(&record.source_url).await.map_err(|err| {
            tracing::warn!(
                "图片抓取失败 id={} url={} err={:?}",
                id,
                record.source_url,
                err
            );
            AppError::NotFound("图片抓取失败".to_string())
        })?;
        let (bytes, content_type, ext) = transcode(res)?;
        let dir = self.dir();
        fs::create_dir_all(&dir)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        let path = self.body_path(id);
        fs::write(&path, &bytes)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        enforce_flat_dir_capacity(&dir, self.limit_bytes, bytes.len() as u64).await;

        record.content_type = content_type.clone();
        record.ext = ext;
        record.updated_at = now_secs();
        record.failed_at = None;
        self.save_record(&record).await;
        Ok((bytes, content_type))
    }

    async fn fetch(&self, url: &str) -> Result<FetchedImage, AppError> {
        // 与封面抓取同一套头：有些站点按 UA / Referer 做防盗链
        let referer = url::Url::parse(url).ok().and_then(|parsed| {
            let host = parsed.host_str()?;
            Some(format!("{}://{}", parsed.scheme(), host))
        });
        let http = self.http.client_for("public").map_err(AppError::Internal)?;
        let mut req = http
            .get(url)
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
        tracing::warn!("HEIC 转码失败 url={} err={err}", image.url);
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

/// 待改写的封面：JSON 指针 + 上游地址 + 书籍上下文。
struct CoverTarget {
    pointer: String,
    url: String,
    book_url: Option<String>,
    book_source_url: Option<String>,
}

/// 同步遍历响应，收集需要改写的 `coverUrl`。
///
/// 书籍上下文（`bookUrl` / `origin`）取同一层对象的兄弟字段——书架、搜索、
/// 探索返回的书籍对象都是这个形状。
fn collect_cover_targets(value: &Value, pointer: &str, out: &mut Vec<CoverTarget>) {
    match value {
        Value::Object(map) => {
            let book_url = map
                .get("bookUrl")
                .and_then(Value::as_str)
                .map(str::to_string);
            let book_source_url = map
                .get("origin")
                .or_else(|| map.get("bookSourceUrl"))
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
                            book_url: book_url.clone(),
                            book_source_url: book_source_url.clone(),
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
    fn cover_targets_carry_book_context() {
        let mut value = json!({
            "books": [{
                "bookUrl": "http://src/info?book_id=1",
                "origin": "http://src/",
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
        assert_eq!(first.book_url.as_deref(), Some("http://src/info?book_id=1"));
        assert_eq!(first.book_source_url.as_deref(), Some("http://src/"));

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
}
