//! 缓存内全文检索：只扫已缓存章节的正文（可再生数据），不触发任何上游请求。
//!
//! `search_cached_content` 逐批推进：async 侧做预算检查与正文加载（DB 读），
//! 剥标签 + 正则命中 + 摘要构建的 CPU 段整批移交 blocking 线程
//! （`scan_content_batch`）。资源预算：5000 章 / 50MB / 10 秒，超限如实
//! 上报 truncated；全书检索并发由 `CACHED_SEARCH_PERMITS` 闸门限到 2 个。

use super::BookService;
use crate::error::error::AppError;
use crate::storage::db::content_repo::EffectiveVersionRow;
use std::collections::HashMap;
use std::sync::Arc;

/// 单章的命中摘要数上限：一章命中太多时前三条足够判断要不要跳过去。
const MAX_SNIPPETS_PER_CHAPTER: usize = 3;

/// 命中数达到上限而中断时，是否还有未扫的章节。
/// 分母必须是全书可检索章节总数（`total_in_book`），而不是按 5000 章上限
/// 裁剪后的扫描清单长度：全书超过上限时，最后一次命中可能恰好落在扫描
/// 清单的末尾，但上限之外还有章节没看过。
fn hit_limit_leaves_more(position: usize, total_in_book: usize) -> bool {
    position + 1 < total_in_book
}

/// 一批已加载正文的 CPU 检索段（剥标签 + 正则命中 + 摘要构建），
/// 由 `search_cached_content` 整批移交 blocking 线程执行。
///
/// 命中按章顺序追加；`limit_position` 是命中限额停下的章在扫描清单里的
/// 全局位置（未达限额为 None），`examined` 是本批实际过了一遍的章节数
/// （供调用方把「加载了但没来得及检索」的章节从 cached_chapters 里扣掉）。
struct BatchScanOutcome {
    hits: Vec<CachedSearchHit>,
    limit_position: Option<usize>,
    examined: usize,
}

fn scan_content_batch(
    batch: Vec<(usize, EffectiveVersionRow, String)>,
    re: Arc<regex::Regex>,
    chapter_meta: Arc<HashMap<String, (i32, String)>>,
    hit_budget: usize,
) -> BatchScanOutcome {
    let mut hits = Vec::new();
    let total = batch.len();
    for (i, (position, version, body)) in batch.into_iter().enumerate() {
        // 正文可能是 HTML：剥掉标签再匹配，摘要才不带尖括号
        let text = crate::util::text::strip_html_tags(&body);
        let mut snippets = Vec::new();
        for m in re.find_iter(&text) {
            snippets.push(build_search_snippet(&text, m.start(), m.end()));
            if snippets.len() >= MAX_SNIPPETS_PER_CHAPTER {
                break;
            }
        }
        if snippets.is_empty() {
            continue;
        }
        // 目录是章节标题/序号的最新快照；版本里的快照可能停在抓取那一刻。
        let (chapter_index, chapter_title) = chapter_meta
            .get(version.chapter_url.as_str())
            .cloned()
            .unwrap_or_else(|| (version.chapter_index, version.chapter_title.clone()));
        hits.push(CachedSearchHit {
            chapter_index,
            chapter_title,
            chapter_url: version.chapter_url.clone(),
            snippets,
            edited: version.kind != "upstream",
        });
        if hits.len() >= hit_budget {
            return BatchScanOutcome {
                hits,
                limit_position: Some(position),
                examined: i + 1,
            };
        }
    }
    BatchScanOutcome {
        hits,
        limit_position: None,
        examined: total,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSearchHit {
    pub chapter_index: i32,
    pub chapter_title: String,
    pub chapter_url: String,
    pub snippets: Vec<String>,
    /// 命中内容来自编辑版（而非上游缓存）：前端在条目上标「已编辑」
    pub edited: bool,
}

/// 全书缓存检索的并发闸门：一次检索读入上限 50MB，无闸门时并发请求
/// 线性放大内存占用。单用户场景 2 个并发足够（阅读器搜索 + 重试）。
static CACHED_SEARCH_PERMITS: once_cell::sync::Lazy<tokio::sync::Semaphore> =
    once_cell::sync::Lazy::new(|| tokio::sync::Semaphore::new(2));

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSearchOutcome {
    pub results: Vec<CachedSearchHit>,
    /// 实际参与检索（有正文缓存文件）的章节数
    pub cached_chapters: usize,
    /// 目录里的可检索章节总数（不含分卷行与无 URL 章）
    pub total_chapters: usize,
    /// 命中达到上限被截断：后面可能还有结果
    pub truncated: bool,
}

/// 命中处前后各取约 40 字符做摘要（按 char 边界切，不劈开多字节字符）。
fn build_search_snippet(text: &str, match_start: usize, match_end: usize) -> String {
    const CTX_CHARS: usize = 40;
    let start = text[..match_start]
        .char_indices()
        .rev()
        .take(CTX_CHARS)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = text[match_end..]
        .char_indices()
        .take(CTX_CHARS)
        .last()
        .map(|(i, c)| match_end + i + c.len_utf8())
        .unwrap_or(text.len());
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(text[start..end].trim());
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

impl BookService {
    /// 缓存内全文检索：只扫已缓存章节的正文文件（可再生数据），不触发任何
    /// 上游请求。本地书不走这套缓存，自然不在检索范围。`query` 是纯文本
    /// 子串匹配（大小写不敏感），不做正则语法——用户输错正则不该炸掉搜索。
    ///
    /// 资源预算：最多扫 5000 章 / 50MB / 10 秒，超限 truncated 截断并如实
    /// 上报覆盖度。章节文件按剩余预算限量读入（单章几十 MB 不会先把内存
    /// 翻倍再判超限）。已编辑章节读编辑版（与阅读器显示一致），命中带
    /// `edited` 标记。同时最多 2 个检索在跑（每个的读入上限都是 50MB）。
    pub async fn search_cached_content(
        &self,
        user_ns: &str,
        book_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<CachedSearchOutcome, AppError> {
        const MAX_SCAN_CHAPTERS: usize = 5000;
        const MAX_SCAN_BYTES: u64 = 50 * 1024 * 1024;
        const MAX_SCAN_DURATION: std::time::Duration = std::time::Duration::from_secs(10);
        /// 单批送进 blocking 线程的章节数：剥标签 + 正则是纯 CPU，
        /// 逐章在 async worker 上跑会饿死同 worker 上的其他任务。
        const SCAN_BLOCKING_BATCH: usize = 100;

        let _permit = CACHED_SEARCH_PERMITS
            .acquire()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("检索并发闸门已关闭: {e}")))?;

        // 目录用来报「全书可检索章节总数」；正文全部来自正文池里当前有效的版本。
        let chapters = self
            .load_chapter_list_cache(user_ns, book_id)
            .await?
            .unwrap_or_default();
        let total_in_book = chapters
            .iter()
            .filter(|chapter| !chapter.is_volume && !chapter.url.is_empty())
            .count();
        let versions = self
            .content
            .list_effective_versions(user_ns, book_id)
            .await?;
        let total_cached = versions.len();
        let total_for_coverage = if total_in_book > 0 {
            total_in_book
        } else {
            total_cached
        };
        // 章节数预算：超出部分按截断处理（coverage 的 total 仍报真实总数）
        let scan_list: Vec<EffectiveVersionRow> =
            versions.into_iter().take(MAX_SCAN_CHAPTERS).collect();
        let re = Arc::new(
            regex::Regex::new(&format!("(?i){}", regex::escape(query)))
                .map_err(|e| AppError::BadRequest(format!("搜索词不可用: {e}")))?,
        );
        // 目录元数据改 owned：blocking 任务闭包要求 'static
        let chapter_meta: HashMap<String, (i32, String)> = chapters
            .iter()
            .map(|chapter| (chapter.url.clone(), (chapter.index, chapter.title.clone())))
            .collect();
        let chapter_meta = Arc::new(chapter_meta);

        let mut outcome = CachedSearchOutcome {
            results: Vec::new(),
            cached_chapters: 0,
            total_chapters: total_for_coverage,
            truncated: total_for_coverage > MAX_SCAN_CHAPTERS || total_cached > MAX_SCAN_CHAPTERS,
        };
        let started = std::time::Instant::now();
        let mut scanned_bytes = 0u64;
        // 已送进加载循环的章节数（含加载失败的）：给批内章节标全局位置
        let mut scanned_offset = 0usize;
        // 逐批推进：async 侧做预算检查与正文加载，CPU 段（剥标签 + 正则 +
        // 摘要）整批移交 blocking 线程，结果按章顺序原样接回。
        for batch in scan_list.chunks(SCAN_BLOCKING_BATCH) {
            // 耗时预算：超时按已扫部分截断返回（批间检查，批粒度足够——
            // 预算本身是 10s 量级的软上限，且字节预算把单批 CPU 量封了顶）
            if started.elapsed() > MAX_SCAN_DURATION {
                outcome.truncated = true;
                break;
            }
            // 预算淘汰与正文加载留在 async 侧（DB 读是异步的）
            let mut loaded: Vec<(usize, EffectiveVersionRow, String)> =
                Vec::with_capacity(batch.len());
            let mut budget_exhausted = false;
            for (position, version) in batch.iter().enumerate() {
                // 字节预算：循环起点上后面一定还有未扫章节，剩余为 0 即截断
                let remaining = MAX_SCAN_BYTES.saturating_sub(scanned_bytes);
                if remaining == 0 {
                    outcome.truncated = true;
                    budget_exhausted = true;
                    break;
                }
                // 半截内容不搜：截断点附近的命中会丢，如实报截断
                if version.raw_size as u64 > remaining {
                    outcome.truncated = true;
                    budget_exhausted = true;
                    break;
                }
                let Ok(body) = self.content.load_content(version.content_id).await else {
                    continue;
                };
                outcome.cached_chapters += 1;
                scanned_bytes += body.len() as u64;
                // 记录章在扫描清单里的全局位置：命中限额的截断判定要用
                let global_position = scanned_offset + position;
                loaded.push((global_position, version.clone(), body));
            }
            scanned_offset += batch.len();
            if budget_exhausted {
                break;
            }
            if loaded.is_empty() {
                continue;
            }
            let hit_budget = max_results.saturating_sub(outcome.results.len());
            let loaded_count = loaded.len();
            let re = Arc::clone(&re);
            let meta = Arc::clone(&chapter_meta);
            let batch_outcome = tokio::task::spawn_blocking(move || {
                scan_content_batch(loaded, re, meta, hit_budget)
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("缓存检索任务失败: {e}")))?;
            outcome.results.extend(batch_outcome.hits);
            if let Some(position) = batch_outcome.limit_position {
                // 命中上限中断：分母用全书可检索章节总数，不是截断后的扫描
                // 清单长度——5000 章上限之外还可能有缓存章节
                outcome.truncated = hit_limit_leaves_more(position, total_for_coverage);
                // 批内加载了但没来得及检索的章节不计入 cached_chapters，
                // 保持逐章实现「实际参与检索」的口径
                outcome.cached_chapters -= loaded_count - batch_outcome.examined;
                break;
            }
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    /// 命中上限中断的截断判定：分母是全书章节总数，不是 5000 章扫描上限
    /// 裁剪后的长度。全书 5001 章、最后一章命中时仍要报截断（旧实现会
    /// 把初始的 truncated=true 覆盖成 false）。
    #[test]
    fn hit_limit_boundary_uses_the_real_book_total() {
        assert!(super::hit_limit_leaves_more(4999, 5001), "上限之外还有章节");
        assert!(
            !super::hit_limit_leaves_more(4999, 5000),
            "恰好全书最后一章不应误报"
        );
        assert!(!super::hit_limit_leaves_more(2, 3));
        assert!(super::hit_limit_leaves_more(0, 2));
    }
}
