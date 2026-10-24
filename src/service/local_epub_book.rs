use crate::error::error::AppError;
use crate::model::{book::Book, book_chapter::BookChapter};
use quick_xml::events::{attributes::Attribute, BytesStart, Event};
use quick_xml::Reader;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use tokio::fs;

pub const LOCAL_EPUB_ORIGIN: &str = "local-epub";
pub const LOCAL_EPUB_ORIGIN_NAME: &str = "本地 EPUB";
pub const MAX_EPUB_UPLOAD_BYTES: usize = 80 * 1024 * 1024;
const MAX_EPUB_UNPACKED_BYTES: u64 = 300 * 1024 * 1024;
const MAX_EPUB_FILE_COUNT: usize = 3_000;
const LOCAL_BOOK_DIR: &str = "local_books";
const LOCAL_EPUB_HASH_LEN: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEpubChapter {
    title: String,
    url: String,
    index: i32,
    file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEpubAsset {
    path: String,
    content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEpubIndex {
    book_url: String,
    name: String,
    author: String,
    file_name: String,
    byte_len: usize,
    char_len: usize,
    cover_url: Option<String>,
    chapters: Vec<StoredEpubChapter>,
    assets: Vec<StoredEpubAsset>,
}

#[derive(Debug, Clone)]
struct EpubManifestItem {
    path: String,
    media_type: String,
    properties: String,
}

#[derive(Debug, Clone, Default)]
struct EpubPackage {
    title: Option<String>,
    author: Option<String>,
    manifest: HashMap<String, EpubManifestItem>,
    spine: Vec<String>,
    cover_id: Option<String>,
    nav_id: Option<String>,
    ncx_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct NavPointState {
    label: String,
}

#[derive(Debug, Clone)]
pub struct LocalEpubAsset {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

#[derive(Clone)]
pub struct LocalEpubBookService {
    storage_dir: PathBuf,
}

pub fn is_local_epub_origin(value: &str) -> bool {
    value.trim() == LOCAL_EPUB_ORIGIN
}

pub fn is_local_epub_url(value: &str) -> bool {
    value.trim().starts_with("local-epub:")
}

pub fn build_epub_chapter_url(book_url: &str, index: usize) -> String {
    format!("{}#{}", book_url.trim_end_matches('#'), index)
}

impl LocalEpubBookService {
    pub fn new(storage_dir: impl AsRef<Path>) -> Self {
        Self {
            storage_dir: storage_dir.as_ref().to_path_buf(),
        }
    }

    pub async fn import_epub_book(
        &self,
        user_ns: &str,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<Book, AppError> {
        validate_epub_upload(file_name, bytes.len())?;
        let safe_file_name = sanitize_epub_file_name(file_name);
        let hash = md5_bytes_hex(
            format!("{}:{}:", user_ns, safe_file_name).as_bytes(),
            &bytes,
        );
        let book_url = format!("{}:{}", LOCAL_EPUB_ORIGIN, hash);
        let book_dir = self.book_dir(user_ns, &book_url)?;
        let byte_len = bytes.len();

        // 解压+清洗+落盘全是同步工作，整体移进阻塞线程：流式逐条目处理，
        // 峰值内存 ≈ 原始包 + 单个条目，而不是「整包解压 + 原文」双份驻留。
        let index = {
            let book_dir = book_dir.clone();
            let book_url = book_url.clone();
            let safe_file_name = safe_file_name.clone();
            tokio::task::spawn_blocking(move || {
                import_epub_blocking(&bytes, &book_dir, &book_url, &safe_file_name, byte_len)
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("EPUB 解析任务失败: {e}")))?
        }?;

        Ok(book_from_index(index))
    }

    pub async fn get_book_info(&self, user_ns: &str, book_url: &str) -> Result<Book, AppError> {
        let index = self.read_index(user_ns, book_url).await?;
        Ok(book_from_index(index))
    }

    pub async fn get_chapter_list(
        &self,
        user_ns: &str,
        book_url: &str,
    ) -> Result<Vec<BookChapter>, AppError> {
        let index = self.read_index(user_ns, book_url).await?;
        Ok(index
            .chapters
            .into_iter()
            .map(|chapter| BookChapter {
                title: chapter.title,
                url: chapter.url,
                index: chapter.index,
                ..BookChapter::default()
            })
            .collect())
    }

    pub async fn get_content(&self, user_ns: &str, chapter_url: &str) -> Result<String, AppError> {
        let (book_url, requested_index) = parse_chapter_url(chapter_url)?;
        let index = self.read_index(user_ns, &book_url).await?;
        let chapter = index
            .chapters
            .iter()
            .find(|chapter| chapter.index == requested_index)
            .ok_or_else(|| AppError::BadRequest("章节不存在".to_string()))?;
        fs::read_to_string(
            self.book_dir(user_ns, &book_url)?
                .join("chapters")
                .join(&chapter.file_name),
        )
        .await
        .map_err(map_local_epub_read_error)
    }

    pub async fn get_asset(
        &self,
        user_ns: &str,
        book_url: &str,
        path: &str,
    ) -> Result<LocalEpubAsset, AppError> {
        let index = self.read_index(user_ns, book_url).await?;
        let path = normalize_strict_zip_path(path)
            .ok_or_else(|| AppError::BadRequest("EPUB 资源地址无效".to_string()))?;
        let asset = index
            .assets
            .iter()
            .find(|asset| asset.path == path)
            .ok_or_else(|| AppError::BadRequest("EPUB 资源不存在".to_string()))?;
        let bytes = fs::read(self.book_dir(user_ns, book_url)?.join("assets").join(&path))
            .await
            .map_err(map_local_epub_read_error)?;
        Ok(LocalEpubAsset {
            bytes,
            content_type: asset.content_type.clone(),
        })
    }

    pub async fn delete_book_files(&self, user_ns: &str, book_url: &str) -> Result<bool, AppError> {
        let book_dir = self.book_dir(user_ns, book_url)?;
        match fs::remove_dir_all(book_dir).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(AppError::Internal(err.into())),
        }
    }

    fn local_root(&self, user_ns: &str) -> PathBuf {
        self.storage_dir
            .join("data")
            .join(user_ns)
            .join(LOCAL_BOOK_DIR)
    }

    fn book_dir(&self, user_ns: &str, book_url: &str) -> Result<PathBuf, AppError> {
        let hash = local_epub_hash_from_url(book_url)?;
        Ok(self.local_root(user_ns).join(hash))
    }

    async fn read_index(&self, user_ns: &str, book_url: &str) -> Result<StoredEpubIndex, AppError> {
        let path = self.book_dir(user_ns, book_url)?.join("manifest.json");
        let data = fs::read_to_string(path)
            .await
            .map_err(map_local_epub_read_error)?;
        serde_json::from_str(&data).map_err(|e| AppError::BadRequest(e.to_string()))
    }
}

fn validate_epub_upload(file_name: &str, byte_len: usize) -> Result<(), AppError> {
    let safe = sanitize_epub_file_name(file_name);
    if !safe.to_lowercase().ends_with(".epub") {
        return Err(AppError::BadRequest("仅支持上传 .epub 文件".to_string()));
    }
    if byte_len == 0 {
        return Err(AppError::BadRequest("EPUB 文件不能为空".to_string()));
    }
    if byte_len > MAX_EPUB_UPLOAD_BYTES {
        return Err(AppError::BadRequest("EPUB 文件不能超过 80MB".to_string()));
    }
    Ok(())
}

fn sanitize_epub_file_name(file_name: &str) -> String {
    let name = Path::new(file_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("book.epub")
        .trim()
        .to_string();
    if name.is_empty() {
        "book.epub".to_string()
    } else {
        name
    }
}

fn book_name_from_epub_file_name(file_name: &str) -> String {
    let safe = sanitize_epub_file_name(file_name);
    Path::new(&safe)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("本地 EPUB")
        .to_string()
}

/// 流式读取 zip 单个条目：按实际解压输出扣预算（zip 头里的声明大小是
/// 攻击者可控值，「声明 1 字节、实际膨胀数 GB」靠 `take(remaining + 1)` 检出）。
/// 条目不存在返回 Ok(None)。
fn read_epub_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    path: &str,
    budget: &mut u64,
) -> Result<Option<Vec<u8>>, AppError> {
    let mut file = match archive.by_name(path) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err(AppError::BadRequest("EPUB 文件损坏".to_string())),
    };
    if !file.is_file() {
        return Ok(None);
    }
    if file.enclosed_name().is_none() {
        return Err(AppError::BadRequest("EPUB 包含非法路径".to_string()));
    }
    let remaining = budget.saturating_add(1);
    let mut data = Vec::new();
    (&mut file)
        .take(remaining)
        .read_to_end(&mut data)
        .map_err(|_| AppError::BadRequest("EPUB 文件损坏".to_string()))?;
    if data.len() as u64 > *budget {
        return Err(AppError::BadRequest("EPUB 解压后体积过大".to_string()));
    }
    *budget -= data.len() as u64;
    Ok(Some(data))
}

/// import 的阻塞段：流式解 zip、清洗、直接落盘（std::fs，在 spawn_blocking 里跑）。
fn import_epub_blocking(
    bytes: &[u8],
    book_dir: &Path,
    book_url: &str,
    safe_file_name: &str,
    byte_len: usize,
) -> Result<StoredEpubIndex, AppError> {
    let cursor: Cursor<&[u8]> = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|_| AppError::BadRequest("EPUB 文件损坏".to_string()))?;
    if archive.len() > MAX_EPUB_FILE_COUNT {
        return Err(AppError::BadRequest("EPUB 文件数量过多".to_string()));
    }
    // 全条目名字扫描：流式读取只碰 manifest 引用的条目，恶意条目不会被写出，
    // 但包含路径穿越条目的包直接整体拒收（纵深防御）
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|_| AppError::BadRequest("EPUB 文件损坏".to_string()))?;
        if !file.is_file() {
            continue;
        }
        let Some(enclosed) = file.enclosed_name() else {
            return Err(AppError::BadRequest("EPUB 包含非法路径".to_string()));
        };
        if normalize_pathbuf(&enclosed).is_none() {
            return Err(AppError::BadRequest("EPUB 包含非法路径".to_string()));
        }
    }
    let mut budget = MAX_EPUB_UNPACKED_BYTES;

    let container = read_epub_entry(&mut archive, "META-INF/container.xml", &mut budget)?
        .ok_or_else(|| AppError::BadRequest("EPUB 缺少 container.xml".to_string()))?;
    let rootfile = parse_container_rootfile(&decode_utf8_lossy(&container))?;
    let opf = read_epub_entry(&mut archive, &rootfile, &mut budget)?
        .ok_or_else(|| AppError::BadRequest("EPUB 缺少 OPF".to_string()))?;
    let opf_base = parent_zip_dir(&rootfile);
    let package = parse_opf(&decode_utf8_lossy(&opf), &opf_base)?;

    let title_map = load_title_map_streamed(&mut archive, &package, &mut budget);
    let asset_paths = collect_asset_paths(&package);

    // 目录重建（同一本书重复导入覆盖）
    if book_dir.exists() {
        std::fs::remove_dir_all(book_dir).map_err(|e| AppError::Internal(e.into()))?;
    }
    std::fs::create_dir_all(book_dir.join("chapters")).map_err(|e| AppError::Internal(e.into()))?;
    std::fs::create_dir_all(book_dir.join("assets")).map_err(|e| AppError::Internal(e.into()))?;
    std::fs::write(book_dir.join("original.epub"), bytes)
        .map_err(|e| AppError::Internal(e.into()))?;

    // 配图逐条目落盘
    let mut stored_assets = Vec::new();
    for path in &asset_paths {
        if let Some(data) = read_epub_entry(&mut archive, path, &mut budget)? {
            let content_type = content_type_for_path(path);
            let target = safe_child_path(&book_dir.join("assets"), path)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| AppError::Internal(e.into()))?;
            }
            std::fs::write(target, data).map_err(|e| AppError::Internal(e.into()))?;
            stored_assets.push(StoredEpubAsset {
                path: path.clone(),
                content_type,
            });
        }
    }

    // 章节按 spine 顺序流式处理：读一条、清洗、写一条
    let mut chapters = Vec::new();
    let mut char_len = 0usize;
    let mut seen_chapter_paths = HashSet::new();
    for idref in &package.spine {
        let Some(item) = package.manifest.get(idref) else {
            continue;
        };
        if !is_html_media_type(&item.media_type) || !seen_chapter_paths.insert(item.path.clone()) {
            continue;
        }
        let Some(raw) = read_epub_entry(&mut archive, &item.path, &mut budget)? else {
            continue;
        };
        let html = decode_utf8_lossy(&raw);
        let title = title_map
            .get(&item.path)
            .cloned()
            .or_else(|| extract_html_title(&html))
            .unwrap_or_else(|| format!("第 {} 章", chapters.len() + 1));
        let content = sanitize_chapter_html(&html, &item.path, book_url, &asset_paths);
        char_len += plain_text_len(&content);
        let index = chapters.len();
        let file_name = format!("{index}.html");
        std::fs::write(book_dir.join("chapters").join(&file_name), content)
            .map_err(|e| AppError::Internal(e.into()))?;
        chapters.push(StoredEpubChapter {
            title,
            url: build_epub_chapter_url(book_url, index),
            index: index as i32,
            file_name,
        });
    }

    if chapters.is_empty() {
        return Err(AppError::BadRequest("EPUB 未找到可阅读章节".to_string()));
    }

    let cover_url = resolve_cover_path(&package)
        .filter(|path| asset_paths.contains(path))
        .map(|path| build_asset_url(book_url, &path));
    let name = package
        .title
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| book_name_from_epub_file_name(safe_file_name));
    let author = package
        .author
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "本地导入".to_string());
    let index = StoredEpubIndex {
        book_url: book_url.to_string(),
        name,
        author,
        file_name: safe_file_name.to_string(),
        byte_len,
        char_len,
        cover_url,
        chapters,
        assets: stored_assets,
    };
    let data = serde_json::to_string_pretty(&index).map_err(|e| AppError::Internal(e.into()))?;
    std::fs::write(book_dir.join("manifest.json"), data)
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(index)
}

fn parse_container_rootfile(xml: &str) -> Result<String, AppError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if local_name(e.name().as_ref()) == "rootfile" =>
            {
                if let Some(path) = attr_by_name(&e, "full-path") {
                    return normalize_strict_zip_path(&path)
                        .ok_or_else(|| AppError::BadRequest("EPUB rootfile 路径无效".to_string()));
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(AppError::BadRequest("EPUB container.xml 无效".to_string())),
            _ => {}
        }
        buf.clear();
    }
    Err(AppError::BadRequest("EPUB 缺少 rootfile".to_string()))
}

fn parse_opf(xml: &str, opf_base: &str) -> Result<EpubPackage, AppError> {
    let mut package = EpubPackage::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_meta: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = local_name(e.name().as_ref());
                match tag.as_str() {
                    "title" | "creator" => current_meta = Some(tag),
                    "item" | "itemref" | "meta" => parse_opf_element(&mut package, &e, opf_base),
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                parse_opf_element(&mut package, &e, opf_base);
            }
            Ok(Event::Text(e)) => {
                if let Some(kind) = current_meta.as_deref() {
                    let text = e.html_content().unwrap_or_default().trim().to_string();
                    if !text.is_empty() {
                        match kind {
                            "title" if package.title.is_none() => package.title = Some(text),
                            "creator" if package.author.is_none() => package.author = Some(text),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let tag = local_name(e.name().as_ref());
                if current_meta.as_deref() == Some(tag.as_str()) {
                    current_meta = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(AppError::BadRequest("EPUB OPF 文件无效".to_string())),
            _ => {}
        }
        buf.clear();
    }

    if package.manifest.is_empty() || package.spine.is_empty() {
        return Err(AppError::BadRequest(
            "EPUB 缺少 manifest 或 spine".to_string(),
        ));
    }
    Ok(package)
}

fn parse_opf_element(package: &mut EpubPackage, e: &BytesStart<'_>, opf_base: &str) {
    match local_name(e.name().as_ref()).as_str() {
        "item" => {
            let id = attr_by_name(e, "id").unwrap_or_default();
            let href = attr_by_name(e, "href").unwrap_or_default();
            let media_type = attr_by_name(e, "media-type").unwrap_or_default();
            let properties = attr_by_name(e, "properties").unwrap_or_default();
            if id.is_empty() || href.is_empty() {
                return;
            }
            let Some(path) = join_zip_path(opf_base, &href) else {
                return;
            };
            if media_type == "application/x-dtbncx+xml" {
                package.ncx_id = Some(id.clone());
            }
            if properties.split_whitespace().any(|value| value == "nav") {
                package.nav_id = Some(id.clone());
            }
            if properties
                .split_whitespace()
                .any(|value| value == "cover-image")
            {
                package.cover_id = Some(id.clone());
            }
            package.manifest.insert(
                id.clone(),
                EpubManifestItem {
                    path,
                    media_type,
                    properties,
                },
            );
        }
        "itemref" => {
            if let Some(idref) = attr_by_name(e, "idref").filter(|value| !value.is_empty()) {
                package.spine.push(idref);
            }
        }
        "meta" if attr_by_name(e, "name").as_deref() == Some("cover") => {
            package.cover_id = attr_by_name(e, "content");
        }
        _ => {}
    }
}

fn load_title_map_streamed(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    package: &EpubPackage,
    budget: &mut u64,
) -> HashMap<String, String> {
    if let Some(nav_id) = &package.nav_id {
        if let Some(item) = package.manifest.get(nav_id) {
            if let Ok(Some(data)) = read_epub_entry(archive, &item.path, budget) {
                let map = parse_nav_titles(&decode_utf8_lossy(&data), &parent_zip_dir(&item.path));
                if !map.is_empty() {
                    return map;
                }
            }
        }
    }

    if let Some(ncx_id) = &package.ncx_id {
        if let Some(item) = package.manifest.get(ncx_id) {
            if let Ok(Some(data)) = read_epub_entry(archive, &item.path, budget) {
                return parse_ncx_titles(&decode_utf8_lossy(&data), &parent_zip_dir(&item.path));
            }
        }
    }
    HashMap::new()
}

fn parse_ncx_titles(xml: &str, base: &str) -> HashMap<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<NavPointState> = Vec::new();
    let mut capture_text = false;
    let mut titles = HashMap::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match local_name(e.name().as_ref()).as_str() {
                "navPoint" => stack.push(NavPointState::default()),
                "text" if !stack.is_empty() => capture_text = true,
                "content" => {
                    if let (Some(src), Some(state)) = (attr_by_name(&e, "src"), stack.last()) {
                        if let Some(path) = join_zip_path(base, &src) {
                            let label = state.label.trim();
                            if !label.is_empty() {
                                titles.insert(path, label.to_string());
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(e)) if local_name(e.name().as_ref()) == "content" => {
                if let (Some(src), Some(state)) = (attr_by_name(&e, "src"), stack.last()) {
                    if let Some(path) = join_zip_path(base, &src) {
                        let label = state.label.trim();
                        if !label.is_empty() {
                            titles.insert(path, label.to_string());
                        }
                    }
                }
            }
            Ok(Event::Text(e)) if capture_text => {
                if let Some(state) = stack.last_mut() {
                    state
                        .label
                        .push_str(e.html_content().unwrap_or_default().as_ref());
                }
            }
            Ok(Event::End(e)) => match local_name(e.name().as_ref()).as_str() {
                "text" => capture_text = false,
                "navPoint" => {
                    stack.pop();
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    titles
}

fn parse_nav_titles(xml: &str, base: &str) -> HashMap<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_href: Option<String> = None;
    let mut current_text = String::new();
    let mut titles = HashMap::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if local_name(e.name().as_ref()) == "a" => {
                current_href = attr_by_name(&e, "href");
                current_text.clear();
            }
            Ok(Event::Text(e)) if current_href.is_some() => {
                current_text.push_str(e.html_content().unwrap_or_default().as_ref());
            }
            Ok(Event::End(e)) if local_name(e.name().as_ref()) == "a" => {
                if let Some(href) = current_href.take() {
                    if let Some(path) = join_zip_path(base, &href) {
                        let title = current_text.trim();
                        if !title.is_empty() {
                            titles.insert(path, title.to_string());
                        }
                    }
                }
                current_text.clear();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    titles
}

fn extract_html_title(html: &str) -> Option<String> {
    let mut reader = Reader::from_str(html);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut capture: Option<String> = None;
    let mut captured_text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = local_name(e.name().as_ref());
                if matches!(tag.as_str(), "title" | "h1" | "h2" | "h3") {
                    capture = Some(tag);
                    captured_text.clear();
                }
            }
            Ok(Event::Text(e)) if capture.is_some() => {
                captured_text.push_str(e.html_content().unwrap_or_default().as_ref());
            }
            Ok(Event::End(e)) => {
                let tag = local_name(e.name().as_ref());
                if capture.as_deref() == Some(tag.as_str()) {
                    let title = collapse_ws(&captured_text);
                    if !title.is_empty() {
                        return Some(title);
                    }
                    capture = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

fn sanitize_chapter_html(
    html: &str,
    chapter_path: &str,
    book_url: &str,
    asset_paths: &HashSet<String>,
) -> String {
    let content = sanitize_chapter_html_inner(html, chapter_path, book_url, asset_paths, true);
    if content.trim().is_empty() {
        sanitize_chapter_html_inner(html, chapter_path, book_url, asset_paths, false)
    } else {
        content
    }
}

fn sanitize_chapter_html_inner(
    html: &str,
    chapter_path: &str,
    book_url: &str,
    asset_paths: &HashSet<String>,
    body_only: bool,
) -> String {
    let mut reader = Reader::from_str(html);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = String::new();
    let mut in_body = !body_only;
    let mut skip_depth = 0usize;
    let base = parent_zip_dir(chapter_path);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = local_name(e.name().as_ref());
                if tag == "body" {
                    in_body = true;
                    buf.clear();
                    continue;
                }
                if should_skip_tag(&tag) {
                    skip_depth += 1;
                    buf.clear();
                    continue;
                }
                if in_body && skip_depth == 0 && is_allowed_html_tag(&tag) {
                    out.push('<');
                    out.push_str(&tag);
                    push_safe_attrs(&mut out, &tag, &e, &base, book_url, asset_paths);
                    out.push('>');
                }
            }
            Ok(Event::Empty(e)) => {
                let tag = local_name(e.name().as_ref());
                if in_body && skip_depth == 0 && is_allowed_html_tag(&tag) {
                    out.push('<');
                    out.push_str(&tag);
                    push_safe_attrs(&mut out, &tag, &e, &base, book_url, asset_paths);
                    out.push_str("/>");
                }
            }
            Ok(Event::End(e)) => {
                let tag = local_name(e.name().as_ref());
                if tag == "body" {
                    in_body = false;
                    buf.clear();
                    continue;
                }
                if should_skip_tag(&tag) && skip_depth > 0 {
                    skip_depth -= 1;
                    buf.clear();
                    continue;
                }
                if in_body
                    && skip_depth == 0
                    && is_allowed_html_tag(&tag)
                    && !is_void_html_tag(&tag)
                {
                    out.push_str("</");
                    out.push_str(&tag);
                    out.push('>');
                }
            }
            Ok(Event::Text(e)) if in_body && skip_depth == 0 => {
                out.push_str(&escape_html(e.html_content().unwrap_or_default().as_ref()));
            }
            Ok(Event::CData(e)) if in_body && skip_depth == 0 => {
                out.push_str(&escape_html(&decode_utf8_lossy(e.as_ref())));
            }
            Ok(Event::Eof) => break,
            Err(_) => {
                return strip_dangerous_html_fallback(html, chapter_path, book_url, asset_paths)
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

fn push_safe_attrs(
    out: &mut String,
    tag: &str,
    e: &BytesStart<'_>,
    base: &str,
    book_url: &str,
    asset_paths: &HashSet<String>,
) {
    for attr in e.attributes().flatten() {
        let key = local_name(attr.key.as_ref());
        if key.starts_with("on") {
            continue;
        }
        let Some(value) = attr_value(&attr) else {
            continue;
        };
        let safe_value = match key.as_str() {
            "src" if tag == "img" => {
                let Some(path) = join_zip_path(base, &value) else {
                    continue;
                };
                if !asset_paths.contains(&path) {
                    continue;
                }
                build_asset_url(book_url, &path)
            }
            "href" if tag == "a" => {
                if value.trim_start().starts_with('#') {
                    value
                } else {
                    continue;
                }
            }
            "alt" | "title" | "class" | "id" | "colspan" | "rowspan" => value,
            _ => continue,
        };
        out.push(' ');
        out.push_str(&key);
        out.push_str("=\"");
        out.push_str(&escape_attr(&safe_value));
        out.push('"');
    }
}

static SCRIPT_BLOCK_RE: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
    Regex::new(r"(?is)<\s*script[^>]*>.*?<\s*/\s*script\s*>").unwrap()
});
static STYLE_BLOCK_RE: once_cell::sync::Lazy<Regex> =
    once_cell::sync::Lazy::new(|| Regex::new(r"(?is)<\s*style[^>]*>.*?<\s*/\s*style\s*>").unwrap());
static IMG_SRC_RE: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
    Regex::new(r#"(?is)<img\b[^>]*\bsrc=["']([^"']+)["'][^>]*>"#).unwrap()
});

fn strip_dangerous_html_fallback(
    html: &str,
    chapter_path: &str,
    book_url: &str,
    asset_paths: &HashSet<String>,
) -> String {
    let without_scripts = SCRIPT_BLOCK_RE.replace_all(html, "");
    let without_scripts = STYLE_BLOCK_RE.replace_all(&without_scripts, "");
    let base = parent_zip_dir(chapter_path);
    IMG_SRC_RE
        .replace_all(&without_scripts, |captures: &regex::Captures<'_>| {
            let Some(src) = captures.get(1).map(|m| m.as_str()) else {
                return String::new();
            };
            let Some(path) = join_zip_path(&base, src) else {
                return String::new();
            };
            if !asset_paths.contains(&path) {
                return String::new();
            }
            format!(
                "<img src=\"{}\"/>",
                escape_attr(&build_asset_url(book_url, &path))
            )
        })
        .into_owned()
}

fn collect_asset_paths(package: &EpubPackage) -> HashSet<String> {
    let mut assets = HashSet::new();
    for item in package.manifest.values() {
        if item.media_type.starts_with("image/") || is_image_path(&item.path) {
            assets.insert(item.path.clone());
        }
    }
    assets
}

fn resolve_cover_path(package: &EpubPackage) -> Option<String> {
    package
        .cover_id
        .as_ref()
        .and_then(|id| package.manifest.get(id))
        .map(|item| item.path.clone())
        .or_else(|| {
            package
                .manifest
                .values()
                .find(|item| {
                    item.properties
                        .split_whitespace()
                        .any(|value| value == "cover-image")
                })
                .map(|item| item.path.clone())
        })
        .or_else(|| {
            package
                .manifest
                .values()
                .find(|item| {
                    item.path.to_lowercase().contains("cover") && is_image_path(&item.path)
                })
                .map(|item| item.path.clone())
        })
}

fn book_from_index(index: StoredEpubIndex) -> Book {
    Book {
        name: index.name,
        author: index.author,
        book_url: index.book_url.clone(),
        origin: LOCAL_EPUB_ORIGIN.to_string(),
        origin_name: Some(LOCAL_EPUB_ORIGIN_NAME.to_string()),
        cover_url: index.cover_url,
        toc_url: Some(index.book_url),
        can_update: Some(false),
        dur_chapter_index: Some(0),
        dur_chapter_pos: Some(0),
        total_chapter_num: Some(index.chapters.len() as i32),
        latest_chapter_title: index.chapters.last().map(|chapter| chapter.title.clone()),
        kind: Some("本地EPUB".to_string()),
        word_count: Some(format!("{}字", index.char_len)),
        ..Book::default()
    }
}

fn parse_chapter_url(chapter_url: &str) -> Result<(String, i32), AppError> {
    let (book_url, raw_index) = chapter_url
        .rsplit_once('#')
        .ok_or_else(|| AppError::BadRequest("章节地址无效".to_string()))?;
    if !is_local_epub_url(book_url) {
        return Err(AppError::BadRequest("章节地址无效".to_string()));
    }
    let index = raw_index
        .parse::<i32>()
        .map_err(|_| AppError::BadRequest("章节序号无效".to_string()))?;
    Ok((book_url.to_string(), index))
}

fn local_epub_hash_from_url(book_url: &str) -> Result<&str, AppError> {
    let hash = book_url
        .strip_prefix("local-epub:")
        .filter(|value| {
            value.len() == LOCAL_EPUB_HASH_LEN && value.chars().all(|ch| ch.is_ascii_hexdigit())
        })
        .ok_or_else(|| AppError::BadRequest("本地 EPUB 地址无效".to_string()))?;
    Ok(hash)
}

fn map_local_epub_read_error(err: std::io::Error) -> AppError {
    if err.kind() == std::io::ErrorKind::NotFound {
        AppError::BadRequest("本地 EPUB 不存在".to_string())
    } else {
        AppError::Internal(err.into())
    }
}

fn md5_bytes_hex(prefix: &[u8], bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(prefix);
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn normalize_pathbuf(path: &Path) -> Option<String> {
    let value = path.to_string_lossy().replace('\\', "/");
    normalize_strict_zip_path(&value)
}

fn normalize_strict_zip_path(path: &str) -> Option<String> {
    let value = path.replace('\\', "/");
    if value.starts_with('/') || value.contains('\0') {
        return None;
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        parts.push(part);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn join_zip_path(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("http:")
        || href.starts_with("https:")
        || href.starts_with("data:")
        || href.starts_with("javascript:")
    {
        return None;
    }
    let href = href.split('#').next().unwrap_or(href);
    let href = href.split('?').next().unwrap_or(href);
    let decoded = urlencoding::decode(href).unwrap_or(Cow::Borrowed(href));
    let raw = if base.is_empty() {
        decoded.into_owned()
    } else {
        format!("{base}/{decoded}")
    };
    normalize_joined_zip_path(&raw)
}

fn normalize_joined_zip_path(path: &str) -> Option<String> {
    let value = path.replace('\\', "/");
    if value.starts_with('/') || value.contains('\0') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in value.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop()?;
        } else {
            parts.push(part);
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn safe_child_path(root: &Path, relative: &str) -> Result<PathBuf, AppError> {
    let path = normalize_strict_zip_path(relative)
        .ok_or_else(|| AppError::BadRequest("EPUB 资源路径无效".to_string()))?;
    let mut out = root.to_path_buf();
    for part in path.split('/') {
        out.push(part);
    }
    Ok(out)
}

fn parent_zip_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_default()
}

fn local_name(name: &[u8]) -> String {
    let value = std::str::from_utf8(name).unwrap_or_default();
    value
        .rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(value)
        .to_string()
}

fn attr_by_name(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|attr| local_name(attr.key.as_ref()) == name)
        .and_then(|attr| attr_value(&attr))
}

fn attr_value(attr: &Attribute<'_>) -> Option<String> {
    let value = std::str::from_utf8(attr.value.as_ref()).ok()?;
    Some(unescape_xml_attr(value))
}

fn is_html_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/xhtml+xml" | "text/html" | "application/xml" | "text/xml"
    )
}

fn is_image_path(path: &str) -> bool {
    matches!(
        path.rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp"
    )
}

fn content_type_for_path(path: &str) -> String {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// EPUB SVG 资产的沙箱 CSP：SVG 经 /reader3/localEpubAsset 以
/// image/svg+xml **同源**直出，直接打开时内嵌脚本会在主站源下执行。
/// 关掉全部资源加载与脚本，只保留 `<img>` 渲染与内联样式；不能用
/// Content-Disposition: attachment 替代——那会破坏 EPUB 插画的展示。
pub const SVG_ASSET_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// 该 Content-Type 的资产是否需要补 [`SVG_ASSET_CSP`]。
pub fn needs_sandbox_csp(content_type: &str) -> bool {
    content_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/svg+xml")
}

fn build_asset_url(book_url: &str, path: &str) -> String {
    format!(
        "/reader3/localEpubAsset?bookUrl={}&path={}",
        urlencoding::encode(book_url),
        urlencoding::encode(path)
    )
}

fn is_allowed_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "article"
            | "aside"
            | "b"
            | "blockquote"
            | "br"
            | "caption"
            | "code"
            | "div"
            | "em"
            | "figcaption"
            | "figure"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "ol"
            | "p"
            | "pre"
            | "rp"
            | "rt"
            | "ruby"
            | "s"
            | "section"
            | "span"
            | "strong"
            | "sub"
            | "sup"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "u"
            | "ul"
    )
}

fn is_void_html_tag(tag: &str) -> bool {
    matches!(tag, "br" | "hr" | "img")
}

fn should_skip_tag(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "head")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_html(value).replace('"', "&quot;")
}

fn unescape_xml_attr(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn collapse_ws(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn plain_text_len(html: &str) -> usize {
    static TAG_STRIP_RE: once_cell::sync::Lazy<Regex> =
        once_cell::sync::Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
    TAG_STRIP_RE
        .replace_all(html, "")
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn svg_assets_are_the_only_ones_needing_sandbox_csp() {
        assert!(needs_sandbox_csp("image/svg+xml"));
        assert!(needs_sandbox_csp("image/svg+xml; charset=utf-8"));
        assert!(needs_sandbox_csp("IMAGE/SVG+XML"));
        assert!(!needs_sandbox_csp("image/png"));
        assert!(!needs_sandbox_csp("application/xhtml+xml"));
        assert!(!needs_sandbox_csp(""));
        // CSP 语义：脚本与资源加载全部关死，仅保留内联样式
        assert_eq!(
            SVG_ASSET_CSP,
            "default-src 'none'; style-src 'unsafe-inline'"
        );
    }

    fn build_zip_with_entry(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(name, options).unwrap();
            writer.write_all(payload).unwrap();
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    /// 把 zip 首个条目的「声明未压缩大小」篡改为 1 字节（本地头 + 中央目录各一处），
    /// 模拟「声明很小、实际膨胀极大」的解压炸弹。
    fn forge_declared_size(bytes: &mut [u8]) {
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
        // local file header：uncompressed size 在偏移 22（LE u32）
        bytes[22..26].copy_from_slice(&1u32.to_le_bytes());
        // central directory：uncompressed size 在签名后偏移 24
        let pos = bytes
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("central directory not found");
        bytes[pos + 24..pos + 28].copy_from_slice(&1u32.to_le_bytes());
    }

    #[test]
    fn streamed_entry_read_rejects_output_over_limit_regardless_of_declared_size() {
        let payload = vec![0u8; (MAX_EPUB_UNPACKED_BYTES + 1024) as usize];
        let honest = build_zip_with_entry("OEBPS/big.xhtml", &payload);
        drop(payload);

        // 对照：声明值诚实的超大条目必须被拒，且由本实现的输出预算拦下
        // （zip 9 对声明值诚实的条目不做限制，超限只能靠这里）
        let mut archive = zip::ZipArchive::new(Cursor::new(&honest[..])).unwrap();
        let mut budget = MAX_EPUB_UNPACKED_BYTES;
        let err = read_epub_entry(&mut archive, "OEBPS/big.xhtml", &mut budget)
            .expect_err("oversized entry must be rejected");
        assert!(
            matches!(err, AppError::BadRequest(ref msg) if msg.contains("体积过大")),
            "unexpected error: {err:?}"
        );

        // 关键场景：声明大小被篡改为 1 字节。按声明值累计会放行；
        // 按实际解压输出限流的实现必须仍然拒绝。
        // zip 9 起，读层自身会在「实际输出超过声明大小」时立即报 InvalidData
        // （映射为「EPUB 文件损坏」），因此拒绝可能来自 zip 的声明大小校验，
        // 也可能来自本实现的输出预算（「体积过大」）；两者都算拒绝成功。
        let mut forged = honest.clone();
        forge_declared_size(&mut forged);
        let mut archive = zip::ZipArchive::new(Cursor::new(&forged[..])).unwrap();
        let mut budget = MAX_EPUB_UNPACKED_BYTES;
        let err = read_epub_entry(&mut archive, "OEBPS/big.xhtml", &mut budget)
            .expect_err("forged bomb must be rejected");
        assert!(
            matches!(err, AppError::BadRequest(ref msg) if msg.contains("体积过大") || msg.contains("文件损坏")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn streamed_entry_read_accepts_small_archive() {
        let bytes = build_zip_with_entry("OEBPS/a.xhtml", b"<html>ok</html>");
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes[..])).unwrap();
        let mut budget = MAX_EPUB_UNPACKED_BYTES;
        let data = read_epub_entry(&mut archive, "OEBPS/a.xhtml", &mut budget).unwrap();
        assert_eq!(data.as_deref(), Some(&b"<html>ok</html>"[..]));
        // 不存在的条目是 Ok(None)
        assert!(
            read_epub_entry(&mut archive, "OEBPS/missing.xhtml", &mut budget)
                .unwrap()
                .is_none()
        );
    }
}
