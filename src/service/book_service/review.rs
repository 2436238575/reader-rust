//! 章评 / 段评 / 章节配图：评论 URL 求值、分页抓取、段落定位与 7 天缓存。
//!
//! 评论地址藏在章节正文响应里（见 `chapter_source_body`），所有评论接口
//! 都先取正文响应再求值 URL 模板。空页不进缓存：上游偶发失败与「真的没有
//! 评论」在响应上难以区分，缓存空页等于把一次抖动放大成一整周。章节配图
//! 刻意不缓存——图片地址带时效签名，存下来就是死链。

use super::BookService;
use crate::error::error::AppError;
use crate::model::book_source::BookSource;
use crate::model::chapter_image::ChapterImages;
use crate::model::review::{
    ParaReviewCount, ParaReviewIndex, ReviewPage, ReviewResponse, ReviewSort,
};
use crate::parser::rule_engine::RuleEngine;
use std::collections::HashMap;

/// 评论每页条数；站点通常另有上限（番茄是 50）。
pub const REVIEW_PAGE_SIZE: i32 = 20;

/// 「只看作者」扫描的上限：25 页 × 50 条 = 一章最多扫 1250 条评论。
/// 上游没有作者过滤参数，只能逐页翻；给上限是防止上万条的爆火章节
/// 把请求打穿（25 个串行请求本身已经够慢了）。
const AUTHOR_SCAN_MAX_PAGES: i32 = 25;
const AUTHOR_SCAN_PAGE_SIZE: i32 = 50;

/// 「作者相关」的评论：作者本人发的、作者赞过的、或有内联回复来自作者/被作者赞过。
///
/// 「作者回复过」只能看到内联回复预览里的那几条（站点的完整回复串不在
/// 列表接口里），这是数据源头的限制。
fn is_author_related(item: &crate::model::review::ReviewItem) -> bool {
    item.author
        || item.author_digg
        || item
            .replies
            .iter()
            .any(|reply| reply.author || reply.author_digg)
}

/// 书源的评论 URL 模板是否用到了 `{{sort}}`。
///
/// 用了说明排序由站点自己做；没用的话「最新」只能由客户端对已加载的条目重排。
fn rule_consumes_sort(rule: Option<&str>) -> bool {
    // `{{sort}}` 与带映射的 `{{sort === ...}}` 都算；前缀匹配而非全文比对，
    // 是因为站点取值往往不是 hot/time 本身（番茄的段评枚举叫 Hot/TimeDesc）
    rule.map(|rule| rule.contains("{{sort") || rule.contains("@get:{sort}"))
        .unwrap_or(false)
}

/// 按字符截断，用于段落定位锚点。
fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

impl BookService {
    /// 评论 URL 模板可用的占位符。
    #[allow(clippy::too_many_arguments)]
    fn review_ctx(
        &self,
        book_url: &str,
        chapter_url: &str,
        page: i32,
        count: i32,
        sort: ReviewSort,
        para_index: Option<i32>,
        variant: &str,
    ) -> HashMap<String, String> {
        let mut ctx = HashMap::new();
        ctx.insert("page".to_string(), page.max(1).to_string());
        ctx.insert("count".to_string(), count.to_string());
        ctx.insert("bookUrl".to_string(), book_url.to_string());
        ctx.insert("chapterUrl".to_string(), chapter_url.to_string());
        // 当前客户端显示的正文版本：书源评论 URL 可按需用它区分
        ctx.insert("variant".to_string(), variant.to_string());
        // 站点取值各不相同（番茄的段评排序是 `Hot`/`TimeDesc` 枚举），
        // 规则里可以用 JS 映射：`&sort={{sort === 'hot' ? 'Hot' : 'TimeDesc'}}`
        ctx.insert("sort".to_string(), sort.as_ctx_value().to_string());
        if let Some(index) = para_index {
            ctx.insert("paraIndex".to_string(), index.to_string());
        }
        ctx
    }

    /// 章评：某一章的评论列表。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_chapter_reviews(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        page: i32,
        count: i32,
        sort: ReviewSort,
        variant: &str,
        refresh: bool,
    ) -> Result<ReviewResponse<ReviewPage>, AppError> {
        if !self.parser.has_chapter_review_rule(source) {
            return Ok(ReviewResponse::new(false, ReviewPage::empty(page)));
        }
        let server_sort = rule_consumes_sort(
            source
                .rule_review
                .as_ref()
                .and_then(|rule| rule.review_url.as_deref()),
        );
        let cache_key = format!(
            "chapter|{chapter_url}|{page}|{count}|{}|{variant}",
            sort.as_ctx_value()
        );
        let author_marks = self.parser.has_chapter_author_rules(source);
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, book_id, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached)
                    .with_server_sort(server_sort)
                    .with_author_marks(author_marks));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let ctx = self.review_ctx(book_url, chapter_url, page, count, sort, None, variant);
        let url = self
            .parse_body_blocking(user_ns, source, &body, &base, &ctx, |p, s, b, u, ctx| {
                p.chapter_review_url(s, b, u, ctx)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("书源的章评地址解析失败".to_string()))?;
        let result = self
            .fetch_review_page(user_ns, source, &url, page, |p, s, b, u| {
                p.chapter_reviews(s, b, u)
            })
            .await?;
        // 空页不进缓存：上游偶发失败（限流、超时）也长这样，缓存 7 天等于
        // 把一次抖动放大成一整周「没有评论」
        if !result.items.is_empty() {
            self.store_review_cache(user_ns, book_id, &cache_key, &result)
                .await;
        }
        Ok(ReviewResponse::new(true, result)
            .with_server_sort(server_sort)
            .with_author_marks(author_marks))
    }

    /// 段评概览：本章哪些段落有段评、各有多少条。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_para_review_index(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        variant: &str,
        refresh: bool,
    ) -> Result<ReviewResponse<ParaReviewIndex>, AppError> {
        if !self.parser.has_para_review_rule(source) {
            return Ok(ReviewResponse::new(false, ParaReviewIndex::default()));
        }
        let cache_key = format!("para-index|{chapter_url}|{variant}");
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, book_id, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let ctx = self.review_ctx(
            book_url,
            chapter_url,
            1,
            REVIEW_PAGE_SIZE,
            ReviewSort::Hot,
            None,
            variant,
        );
        let url = self
            .parse_body_blocking(user_ns, source, &body, &base, &ctx, |p, s, b, u, ctx| {
                p.para_review_index_url(s, b, u, ctx)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("书源的段评概览地址解析失败".to_string()))?;
        let res = self
            .fetch_source_url(user_ns, source, &url, &source.book_source_url)
            .await?;
        let mut index = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| ParaReviewIndex {
                paras: p.para_review_index(s, b, u),
            })
            .await?;
        // 段号会因用户的书源替换规则、繁简转换而漂移，补上段落原文做兜底定位。
        self.fill_para_texts(user_ns, source, &body, &base, &mut index.paras)
            .await;
        if !index.paras.is_empty() {
            self.store_review_cache(user_ns, book_id, &cache_key, &index)
                .await;
        }
        Ok(ReviewResponse::new(true, index))
    }

    /// 章节配图：给「配图另走一个接口」的站点用。
    ///
    /// 刻意不做缓存：图片地址普遍带时效签名（番茄的 `x-expires`），存下来过一阵
    /// 就是一堆打不开的死链；配图接口本身很小，每次开章取一次更省心。
    /// 章节正文响应走 [`Self::chapter_source_body`]，紧接着正文请求时命中内存暂存，
    /// 不额外多抓一次正文。
    pub async fn get_chapter_images(
        &self,
        user_ns: &str,
        source: &BookSource,
        chapter_url: &str,
    ) -> Result<ChapterImages, AppError> {
        if !self.parser.has_content_image_rule(source) {
            return Ok(ChapterImages::disabled());
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let url = self
            .parse_body_blocking(
                user_ns,
                source,
                &body,
                &base,
                &HashMap::new(),
                |p, s, b, u, _ctx| p.content_image_url(s, b, u),
            )
            .await?;
        // 规则求值不出地址（比如书源里写死了别的取法）不算错误：这一章就是没有配图
        let Some(url) = url else {
            return Ok(ChapterImages::new(Vec::new()));
        };
        let res = self
            .fetch_source_url(user_ns, source, &url, &source.book_source_url)
            .await?;
        let images = self
            .parse_response_blocking(user_ns, source, &res, |p, s, b, u| {
                p.content_images(s, b, u)
            })
            .await?;
        Ok(ChapterImages::new(images))
    }

    /// 段评：某一段的评论列表。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_para_reviews(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        para_index: i32,
        page: i32,
        count: i32,
        sort: ReviewSort,
        variant: &str,
        refresh: bool,
    ) -> Result<ReviewResponse<ReviewPage>, AppError> {
        if !self.parser.has_para_review_rule(source) {
            return Ok(ReviewResponse::new(false, ReviewPage::empty(page)));
        }
        let server_sort = rule_consumes_sort(
            source
                .rule_para_review
                .as_ref()
                .and_then(|rule| rule.review_url.as_deref()),
        );
        let author_marks = self.parser.has_para_author_rules(source);
        let cache_key = format!(
            "para|{chapter_url}|{para_index}|{page}|{count}|{}|{variant}",
            sort.as_ctx_value()
        );
        if !refresh {
            if let Some(cached) = self.load_review_cache(user_ns, book_id, &cache_key).await {
                return Ok(ReviewResponse::new(true, cached)
                    .with_server_sort(server_sort)
                    .with_author_marks(author_marks));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let ctx = self.review_ctx(
            book_url,
            chapter_url,
            page,
            count,
            sort,
            Some(para_index),
            variant,
        );
        let url = self
            .parse_body_blocking(user_ns, source, &body, &base, &ctx, |p, s, b, u, ctx| {
                p.para_review_url(s, b, u, ctx)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("书源的段评地址解析失败".to_string()))?;
        let result = self
            .fetch_review_page(user_ns, source, &url, page, |p, s, b, u| {
                p.para_reviews(s, b, u)
            })
            .await?;
        // 空页不进缓存，理由同章评
        if !result.items.is_empty() {
            self.store_review_cache(user_ns, book_id, &cache_key, &result)
                .await;
        }
        Ok(ReviewResponse::new(true, result)
            .with_server_sort(server_sort)
            .with_author_marks(author_marks))
    }

    /// 「只看作者」：扫描本章（`para_index` 为 `Some` 时是某一段）的全部评论页，
    /// 挑出作者评论过、赞过或回复过的**整条**评论（回复完整保留）。
    ///
    /// 上游没有作者过滤参数，只能逐页扫，且**热度序与时间序各扫一遍**——实测
    /// 番茄两种排序返回的评论集合并不一致（作者标记只出现在其中一种的深处），
    /// 按 `comment_id` 去重合并。每种序最多 25 页 × 50 条。扫描结果——包括
    /// 「没有作者相关评论」的空结果——进 7 天评论缓存：一次完整扫描最多 50 个
    /// 上游请求，不能接受每次打开面板都重扫；`refresh=1` 仍可强刷。与单页评论
    /// 的「空页不进缓存」相反，这里的空结果是扫完所有页得出的结论，不是上游
    /// 抖动——中途任何一页失败都会整体报错、不写缓存。
    #[allow(clippy::too_many_arguments)]
    pub async fn get_author_reviews(
        &self,
        user_ns: &str,
        source: &BookSource,
        book_id: &str,
        book_url: &str,
        chapter_url: &str,
        para_index: Option<i32>,
        variant: &str,
        refresh: bool,
    ) -> Result<ReviewResponse<ReviewPage>, AppError> {
        let (capable, author_marks) = match para_index {
            Some(_) => (
                self.parser.has_para_review_rule(source),
                self.parser.has_para_author_rules(source),
            ),
            None => (
                self.parser.has_chapter_review_rule(source),
                self.parser.has_chapter_author_rules(source),
            ),
        };
        // 没配作者标记规则时扫描注定颗粒无收，直接报不支持，前端藏入口
        if !capable || !author_marks {
            return Ok(ReviewResponse::new(false, ReviewPage::empty(1)));
        }
        let cache_key = match para_index {
            Some(index) => format!("para-author|{chapter_url}|{index}|{variant}"),
            None => format!("chapter-author|{chapter_url}|{variant}"),
        };
        if !refresh {
            if let Some(cached) = self
                .load_review_cache::<ReviewPage>(user_ns, book_id, &cache_key)
                .await
            {
                return Ok(ReviewResponse::new(true, cached).with_author_marks(true));
            }
        }
        let (body, base) = self
            .chapter_source_body(user_ns, source, chapter_url)
            .await?;
        let mut items: Vec<crate::model::review::ReviewItem> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for sort in [ReviewSort::Hot, ReviewSort::Time] {
            for page in 1..=AUTHOR_SCAN_MAX_PAGES {
                let ctx = self.review_ctx(
                    book_url,
                    chapter_url,
                    page,
                    AUTHOR_SCAN_PAGE_SIZE,
                    sort,
                    para_index,
                    variant,
                );
                let url = self
                    .parse_body_blocking(
                        user_ns,
                        source,
                        &body,
                        &base,
                        &ctx,
                        move |p, s, b, u, ctx| match para_index {
                            Some(_) => p.para_review_url(s, b, u, ctx),
                            None => p.chapter_review_url(s, b, u, ctx),
                        },
                    )
                    .await?
                    .ok_or_else(|| AppError::BadRequest("书源的评论地址解析失败".to_string()))?;
                let result = self
                    .fetch_review_page(user_ns, source, &url, page, move |p, s, b, u| {
                        match para_index {
                            Some(_) => p.para_reviews(s, b, u),
                            None => p.chapter_reviews(s, b, u),
                        }
                    })
                    .await?;
                let has_more = result.has_more;
                items.extend(
                    result
                        .items
                        .into_iter()
                        .filter(is_author_related)
                        .filter(|item| seen.insert(item.id.clone())),
                );
                if !has_more {
                    break;
                }
            }
        }
        let page_result = ReviewPage {
            total: items.len() as i64,
            has_more: false,
            page: 1,
            items,
        };
        self.store_review_cache(user_ns, book_id, &cache_key, &page_result)
            .await;
        Ok(ReviewResponse::new(true, page_result).with_author_marks(true))
    }

    async fn fetch_review_page<F>(
        &self,
        user_ns: &str,
        source: &BookSource,
        url: &str,
        page: i32,
        parse: F,
    ) -> Result<ReviewPage, AppError>
    where
        F: FnOnce(&RuleEngine, &BookSource, &str, &str) -> ReviewPage + Send + 'static,
    {
        let res = self
            .fetch_source_url(user_ns, source, url, &source.book_source_url)
            .await?;
        let mut parsed = self
            .parse_response_blocking(user_ns, source, &res, parse)
            .await?;
        // 排查评论数据问题时需要能看到实际请求的地址与上游响应形态
        if parsed.items.is_empty() {
            tracing::debug!(
                "评论页为空: url={} 响应前 200 字节={:?}",
                url,
                res.body.chars().take(200).collect::<String>()
            );
        }
        parsed.page = page;
        Ok(parsed)
    }

    /// 用正文规则取出正文并按行切分，给段评补上「这一段是什么」。
    ///
    /// `content` 内部可能跑 JS，与其它解析入口一样必须进阻塞线程池并带
    /// `user_ns`，否则 cache/kv 落进共享桶且堵住 tokio worker。
    async fn fill_para_texts(
        &self,
        user_ns: &str,
        source: &BookSource,
        body: &str,
        base_url: &str,
        paras: &mut [ParaReviewCount],
    ) {
        if paras.is_empty() {
            return;
        }
        let text = self
            .parse_body_blocking(
                user_ns,
                source,
                body,
                base_url,
                &HashMap::new(),
                |p, s, b, u, _| p.content(s, b, u),
            )
            .await
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let lines: Vec<&str> = text.split('\n').collect();
        for para in paras.iter_mut() {
            let Ok(index) = usize::try_from(para.para_index) else {
                continue;
            };
            if let Some(line) = lines.get(index) {
                para.text = truncate_chars(line.trim(), 60);
            }
        }
    }

    async fn load_review_cache<T: serde::de::DeserializeOwned>(
        &self,
        user_ns: &str,
        book_key: &str,
        key: &str,
    ) -> Option<T> {
        let raw = self
            .review_cache
            .get(user_ns, book_key, key)
            .await
            .ok()
            .flatten()?;
        serde_json::from_str(&raw).ok()
    }

    async fn store_review_cache<T: serde::Serialize>(
        &self,
        user_ns: &str,
        book_key: &str,
        key: &str,
        value: &T,
    ) {
        let Ok(raw) = serde_json::to_string(value) else {
            return;
        };
        let _ = self.review_cache.put(user_ns, book_key, key, &raw).await;
    }
}
