//! 阅读统计：阅读事件明细写入、按自然日派生统计、旧数据启动回填。
//!
//! 天边界固定按服务器时区偏移计算：本地 04:00 起算新一天，凌晨 03:59:59
//! 仍属前一天。`day_key` 是整数 epoch day（不是日期字符串），
//! `day_key = div_euclid(utc_secs + offset_secs - 4*3600, 86400)`。
//! 偏移在启动时取一次 `chrono::Local` 的当前偏移并固定，不处理 DST。
//!
//! 明细按 `(user, book, chapter, day)` 存秒数，不存聚合；客户端上报的是
//! 该章当日的累计秒数，服务端 upsert 取 MAX，重复上报不会翻倍。

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::error::error::AppError;
use crate::service::json_document_service::JsonDocumentService;
use crate::storage::db::chapters_repo::ChaptersRepo;
use crate::storage::db::read_stats_repo::ReadStatsRepo;
use crate::storage::db::repo::parse_book_id;
use crate::util::time::now_ts;

/// 单条上报事件（handler 已把 seconds clamp 到 0..=86400）。
#[derive(Debug, Clone)]
pub struct ReadEventInput {
    pub book_id: String,
    pub chapter_index: i32,
    pub seconds: i64,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadEventsBatchResult {
    /// 本批处理的事件条数
    pub accepted: u32,
    /// 服务端当前自然日（epoch day）；客户端据此校准「当日累计秒数」的清零时机
    pub day_key: i64,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadingStats {
    pub total_seconds: i64,
    pub today_seconds: i64,
    pub read_chapters: i64,
    pub finished_books: i64,
    pub streak_days: i64,
}

pub struct ReadStatsService {
    repo: ReadStatsRepo,
    docs: JsonDocumentService,
    chapters: ChaptersRepo,
    /// 服务器时区相对 UTC 的秒数，启动时固定（不处理 DST）。
    utc_offset_secs: i64,
}

impl ReadStatsService {
    pub fn new(repo: ReadStatsRepo, docs: JsonDocumentService, chapters: ChaptersRepo) -> Self {
        let utc_offset_secs = chrono::Local::now().offset().local_minus_utc() as i64;
        Self {
            repo,
            docs,
            chapters,
            utc_offset_secs,
        }
    }

    /// 纯计算：UTC 秒 → 本地 04:00 起算的 epoch day。
    pub fn day_key_for(utc_secs: i64, utc_offset_secs: i64) -> i64 {
        (utc_secs + utc_offset_secs - 4 * 3600).div_euclid(86_400)
    }

    pub fn current_day_key(&self) -> i64 {
        Self::day_key_for(now_ts(), self.utc_offset_secs)
    }

    /// 写入一批阅读事件；同一事务内推进书架 `dur_chapter_time`。
    ///
    /// 书不在登记表（含非法 bookId）整批失败，不做部分成功。
    pub async fn record_batch(
        &self,
        user_ns: &str,
        events: &[ReadEventInput],
        client_day_key: Option<i64>,
    ) -> Result<ReadEventsBatchResult, AppError> {
        let server_day_key = self.current_day_key();
        if events.is_empty() {
            return Ok(ReadEventsBatchResult {
                accepted: 0,
                day_key: server_day_key,
            });
        }
        // 客户端带着上次响应拿到的 dayKey：跨日/离线缓冲的事件写回它所属的
        // 那一天，避免把昨天的累计秒数记到今天；未来时间戳按当前日兜底。
        let write_day_key = match client_day_key {
            Some(day) if day <= server_day_key => day,
            _ => server_day_key,
        };

        let mut rows = Vec::with_capacity(events.len());
        let mut book_ids = HashSet::with_capacity(events.len());
        for event in events {
            let book_id = parse_book_id(&event.book_id)
                .ok_or_else(|| AppError::NotFound(format!("书籍不存在: {}", event.book_id)))?;
            book_ids.insert(book_id);
            rows.push((book_id, event.chapter_index, event.seconds.clamp(0, 86_400)));
        }
        let book_ids: Vec<i64> = book_ids.into_iter().collect();
        let registered = self.repo.registered_book_ids(&book_ids).await?;
        if registered.len() != book_ids.len() {
            let missing = book_ids
                .iter()
                .find(|id| !registered.contains(id))
                .copied()
                .unwrap_or_default();
            return Err(AppError::NotFound(format!("书籍不存在: book_{missing}")));
        }

        self.repo
            .upsert_events(user_ns, write_day_key, &rows, now_ts())
            .await?;
        Ok(ReadEventsBatchResult {
            accepted: rows.len() as u32,
            day_key: server_day_key,
        })
    }

    pub async fn stats(&self, user_ns: &str) -> Result<ReadingStats, AppError> {
        let today = self.current_day_key();
        let total_seconds = self.repo.total_seconds(user_ns).await?;
        let today_seconds = self.repo.today_seconds(user_ns, today).await?;
        let read_chapters = self.repo.read_chapters(user_ns).await?;
        let finished_books = self.repo.finished_books(user_ns).await?;
        let days = self.repo.positive_days(user_ns).await?;
        let mut streak_days = 0i64;
        // 今天还没读时从昨天起算：连续 30 天的人凌晨 04:01 打开统计页
        // 不应看到「连续 0 天」，只要没整天不读就还在连击中。
        let mut expected = if days.contains(&today) {
            today
        } else {
            today - 1
        };
        for day in days {
            if day > expected {
                continue;
            }
            if day < expected {
                break;
            }
            streak_days += 1;
            expected -= 1;
        }
        Ok(ReadingStats {
            total_seconds,
            today_seconds,
            read_chapters,
            finished_books,
            streak_days,
        })
    }

    /// 启动时幂等回填旧 userdata 文档：`readChapterHistory`（已读章节）与
    /// `reading-stats.totalSeconds`（旧版总时长）。
    ///
    /// - 新表已有该用户数据，或 `read_stats_legacy.backfilled_at > 0` → 跳过；
    /// - 文档读取失败时不写回填标记，下次启动重试；
    /// - 回填行全部落在回填当天并标 `backfilled=1`，秒数 0；
    /// - URL 形式的已读键按当前目录反查 index，查不到丢弃；`index:K` 直接使用。
    pub async fn backfill_all(&self) -> Result<(), AppError> {
        let today = self.current_day_key();
        let now = now_ts();
        for user_ns in self.repo.user_namespaces().await? {
            if self.repo.has_rows(&user_ns).await? {
                continue;
            }
            if self
                .repo
                .backfilled_at(&user_ns)
                .await?
                .is_some_and(|timestamp| timestamp > 0)
            {
                continue;
            }
            let history = match self.docs.get_value(&user_ns, "readChapterHistory").await {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(%error, user_ns, "阅读历史文档无法解析，跳过本次回填");
                    continue;
                }
            };
            let rows = self.resolve_history(history.as_ref()).await?;
            let legacy_total = match self.docs.get_value(&user_ns, "reading-stats").await {
                Ok(value) => legacy_total_seconds(value.as_ref()),
                // 读取失败不写标记，避免把旧总时长永久丢成 0
                Err(error) => {
                    tracing::warn!(%error, user_ns, "旧阅读统计文档读取失败，下次启动重试");
                    continue;
                }
            };
            self.repo
                .insert_backfill(&user_ns, today, &rows, legacy_total, now)
                .await?;
            tracing::info!(
                user_ns,
                rows = rows.len(),
                legacy_total,
                day_key = today,
                "阅读统计历史回填完成"
            );
        }
        Ok(())
    }

    async fn resolve_history(&self, history: Option<&Value>) -> Result<Vec<(i64, i32)>, AppError> {
        let Some(Value::Object(books)) = history else {
            return Ok(Vec::new());
        };
        let mut rows = HashSet::new();
        for (book_key, value) in books {
            let Some(book_id) = parse_book_id(book_key) else {
                continue;
            };
            let Some(keys) = value.as_array() else {
                continue;
            };
            let chapters = self.chapters.get_chapters(book_id).await?;
            let by_url: HashMap<&str, i32> = chapters
                .iter()
                .filter(|chapter| !chapter.url.trim().is_empty())
                .map(|chapter| (chapter.url.as_str(), chapter.index))
                .collect();
            for key in keys {
                let Some(raw) = key.as_str() else {
                    continue;
                };
                let Some((prefix, rest)) = raw.split_once("::") else {
                    continue;
                };
                if parse_book_id(prefix) != Some(book_id) {
                    continue;
                }
                if let Some(index) = rest.strip_prefix("index:") {
                    if let Ok(index) = index.parse::<i32>() {
                        if index >= 0 {
                            rows.insert((book_id, index));
                        }
                    }
                    continue;
                }
                if let Some(index) = by_url.get(rest) {
                    rows.insert((book_id, *index));
                    continue;
                }
                // 本地 TXT/EPUB 章节不写 chapters 登记表，URL 形如
                // `local-txt:<book>#<index>` / `local-epub:<book>#<index>`：
                // 从末尾 # 后解析 index，避免回填把本地书的已读记录全丢掉。
                if rest.starts_with("local-txt:") || rest.starts_with("local-epub:") {
                    if let Some((_, raw_index)) = rest.rsplit_once('#') {
                        if let Ok(index) = raw_index.parse::<i32>() {
                            if index >= 0 {
                                rows.insert((book_id, index));
                            }
                        }
                    }
                }
            }
        }
        let mut rows: Vec<(i64, i32)> = rows.into_iter().collect();
        rows.sort_unstable();
        Ok(rows)
    }
}

fn legacy_total_seconds(value: Option<&Value>) -> i64 {
    value
        .and_then(|value| value.get("totalSeconds"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|seconds| seconds.floor() as i64))
        })
        .unwrap_or(0)
        .max(0)
}

#[cfg(test)]
mod tests {
    use super::ReadStatsService;

    #[test]
    fn day_key_rolls_over_at_local_four_am() {
        // UTC，无偏移：epoch day 0 从 04:00 起算；03:59:59 仍属 -1
        assert_eq!(ReadStatsService::day_key_for(3 * 3600 + 3599, 0), -1);
        assert_eq!(ReadStatsService::day_key_for(4 * 3600, 0), 0);
        // 东八区：本地 04:00 对应 UTC 前一天 20:00
        let offset = 8 * 3600;
        assert_eq!(ReadStatsService::day_key_for(20 * 3600 - 1, offset), 0);
        assert_eq!(ReadStatsService::day_key_for(20 * 3600, offset), 1);
        // 负 epoch 也保持 div_euclid 的向负无穷取整
        assert_eq!(ReadStatsService::day_key_for(-1, 0), -1);
    }
}
