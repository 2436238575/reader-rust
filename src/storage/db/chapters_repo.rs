//! 章节登记表的数据访问：目录的持久存储层（表结构见 0007_chapters.sql）。
//!
//! 写入两种形态：整书替换（refresh 重抓 / 完整抓取落库）与按 URL 去重追加
//! （后台补全）。并发追加由调用方（`BookService::toc_append_lock`）串行化，
//! 事务用 `BEGIN IMMEDIATE` 兜底；读取一律按 chapter_index 升序，
//! 输出直接映射 `BookChapter`。

use crate::model::book_chapter::BookChapter;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Transaction};
use std::collections::HashSet;

/// 单条 INSERT 绑定的行数上限：QueryBuilder 分批拼多值语句。
const INSERT_BATCH: usize = 400;

#[derive(Clone)]
pub struct ChaptersRepo {
    pool: sqlx::SqlitePool,
}

/// 目录抓取状态（`book_toc_state` 行）：`complete` 对应原 meta.json 的
/// 完成标志，`fetched_at` 是最近一次抓取落库的 Unix 秒。
#[derive(Debug, Clone, Copy)]
pub struct TocState {
    pub complete: bool,
    pub fetched_at: i64,
}

fn map_chapter_row(row: &SqliteRow) -> BookChapter {
    BookChapter {
        title: row.get::<String, _>("title"),
        url: row.get::<String, _>("url"),
        index: row.get::<i32, _>("chapter_index"),
        tag: row.get::<Option<String>, _>("tag"),
        is_vip: row.get::<i64, _>("is_vip") != 0,
        is_pay: row.get::<i64, _>("is_pay") != 0,
        is_volume: row.get::<i64, _>("is_volume") != 0,
        volume: row.get::<Option<String>, _>("volume"),
        word_count: row.get::<Option<String>, _>("word_count"),
    }
}

const SELECT_ALL_BY_BOOK: &str = "SELECT chapter_index, url, title, tag, is_vip, is_pay, \
is_volume, volume, word_count FROM chapters WHERE book_id = ?1 ORDER BY chapter_index ASC";

const SELECT_BY_INDEX: &str = "SELECT chapter_index, url, title, tag, is_vip, is_pay, \
is_volume, volume, word_count FROM chapters WHERE book_id = ?1 AND chapter_index = ?2";

impl ChaptersRepo {
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// 某本书的完整目录，按 index 升序。
    pub async fn get_chapters(&self, book_id: i64) -> Result<Vec<BookChapter>, sqlx::Error> {
        let rows = sqlx::query(SELECT_ALL_BY_BOOK)
            .bind(book_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(map_chapter_row).collect())
    }

    /// 按 URL 反查（按 index 升序取首个命中）：章节快照用。
    pub async fn find_chapter_by_url(
        &self,
        book_id: i64,
        url: &str,
    ) -> Result<Option<(i32, String)>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT chapter_index, title FROM chapters \
             WHERE book_id = ?1 AND url = ?2 ORDER BY chapter_index ASC LIMIT 1",
        )
        .bind(book_id)
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                r.get::<i32, _>("chapter_index"),
                r.get::<String, _>("title"),
            )
        }))
    }

    /// 按 index 正查：v1 边界的 index→章节解析用。
    pub async fn find_chapter_by_index(
        &self,
        book_id: i64,
        index: i32,
    ) -> Result<Option<BookChapter>, sqlx::Error> {
        let row = sqlx::query(SELECT_BY_INDEX)
            .bind(book_id)
            .bind(index)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| map_chapter_row(&r)))
    }

    pub async fn count_chapters(&self, book_id: i64) -> Result<i64, sqlx::Error> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chapters WHERE book_id = ?1")
            .bind(book_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// 整书替换 + 写目录状态（refresh 重抓 / 完整抓取落库）。
    /// 同批内 URL 去重（首 URL 优先；空 URL 不去重），index 用调用方给的值。
    pub async fn replace_chapters(
        &self,
        book_id: i64,
        chapters: &[BookChapter],
        complete: bool,
        fetched_at: i64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM chapters WHERE book_id = ?1")
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        insert_deduped_tx(&mut tx, book_id, chapters).await?;
        upsert_toc_state_tx(&mut tx, book_id, complete, fetched_at).await?;
        tx.commit().await
    }

    /// 按 URL 去重追加 + 写目录状态；返回追加后的完整目录。
    /// 追加语义与原文件缓存 append 一致：空 URL 不去重，index 从现有最大
    /// index 顺延（不用行数，防历史空洞导致主键冲突）。
    pub async fn append_chapters(
        &self,
        book_id: i64,
        new_chapters: &[BookChapter],
        complete: bool,
        fetched_at: i64,
    ) -> Result<Vec<BookChapter>, sqlx::Error> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut existing = load_chapters_tx(&mut tx, book_id).await?;
        let next_index = existing.last().map(|c| c.index + 1).unwrap_or(0);
        let mut seen: HashSet<String> = existing.iter().map(|c| c.url.clone()).collect();
        let insert_from = existing.len();
        for ch in new_chapters {
            if ch.url.is_empty() || seen.insert(ch.url.clone()) {
                let mut ch = ch.clone();
                ch.index = next_index + (existing.len() - insert_from) as i32;
                existing.push(ch);
            }
        }
        let added = existing.len() > insert_from;
        if added {
            let new_rows: Vec<&BookChapter> = existing[insert_from..].iter().collect();
            insert_chapters_tx(&mut tx, book_id, &new_rows).await?;
        }
        // 与原 meta 写入一致：即使无新增也按调用方给的 complete 更新状态。
        upsert_toc_state_tx(&mut tx, book_id, complete, fetched_at).await?;
        tx.commit().await?;
        Ok(existing)
    }

    /// 删除某本书的目录行与抓取状态（换源 / 清缓存 / refresh=1 前置清理）。
    pub async fn delete_chapters(&self, book_id: i64) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM chapters WHERE book_id = ?1")
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM book_toc_state WHERE book_id = ?1")
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    /// 读取目录抓取状态；从未抓取过的书返回 `None`（读侧按未命中处理）。
    pub async fn get_toc_state(&self, book_id: i64) -> Result<Option<TocState>, sqlx::Error> {
        let row = sqlx::query("SELECT complete, fetched_at FROM book_toc_state WHERE book_id = ?1")
            .bind(book_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| TocState {
            complete: r.get::<i64, _>("complete") != 0,
            fetched_at: r.get::<i64, _>("fetched_at"),
        }))
    }

    /// 仅供测试拨动抓取时间，模拟 TTL 过期。
    #[cfg(test)]
    pub async fn set_toc_fetched_at(&self, book_id: i64, fetched_at: i64) {
        sqlx::query("UPDATE book_toc_state SET fetched_at = ?1 WHERE book_id = ?2")
            .bind(fetched_at)
            .bind(book_id)
            .execute(&self.pool)
            .await
            .expect("set_toc_fetched_at");
    }
}

async fn load_chapters_tx(
    conn: &mut sqlx::SqliteConnection,
    book_id: i64,
) -> Result<Vec<BookChapter>, sqlx::Error> {
    let rows = sqlx::query(SELECT_ALL_BY_BOOK)
        .bind(book_id)
        .fetch_all(conn)
        .await?;
    Ok(rows.iter().map(map_chapter_row).collect())
}

/// 整批写入前的同批 URL 去重（首 URL 优先；空 URL 不去重）。
async fn insert_deduped_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    book_id: i64,
    chapters: &[BookChapter],
) -> Result<(), sqlx::Error> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(chapters.len());
    let mut deduped: Vec<&BookChapter> = Vec::with_capacity(chapters.len());
    for ch in chapters {
        if ch.url.is_empty() || seen.insert(ch.url.as_str()) {
            deduped.push(ch);
        }
    }
    insert_chapters_tx(tx, book_id, &deduped).await
}

async fn insert_chapters_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    book_id: i64,
    chapters: &[&BookChapter],
) -> Result<(), sqlx::Error> {
    for chunk in chapters.chunks(INSERT_BATCH) {
        let mut builder =
            sqlx::QueryBuilder::new("INSERT INTO chapters (book_id, chapter_index, url, title, tag, is_vip, is_pay, is_volume, volume, word_count) ");
        builder.push_values(chunk, |mut b, ch| {
            b.push_bind(book_id)
                .push_bind(ch.index)
                .push_bind(&ch.url)
                .push_bind(&ch.title)
                .push_bind(&ch.tag)
                .push_bind(ch.is_vip)
                .push_bind(ch.is_pay)
                .push_bind(ch.is_volume)
                .push_bind(&ch.volume)
                .push_bind(&ch.word_count);
        });
        builder.build().execute(&mut **tx).await?;
    }
    Ok(())
}

async fn upsert_toc_state_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    book_id: i64,
    complete: bool,
    fetched_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO book_toc_state (book_id, complete, fetched_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(book_id) DO UPDATE SET complete = excluded.complete, fetched_at = excluded.fetched_at",
    )
    .bind(book_id)
    .bind(complete)
    .bind(fetched_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_repo(tag: &str) -> (ChaptersRepo, sqlx::SqlitePool, std::path::PathBuf) {
        crate::util::test_temp::clean_stale_test_dirs();
        let dir = std::env::temp_dir().join(format!(
            "reader-rust-chapters-repo-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = crate::storage::db::init_pool(&format!(
            "sqlite:{}?mode=rwc",
            dir.join("reader.db").display()
        ))
        .await
        .unwrap();
        (ChaptersRepo::new(pool.clone()), pool, dir)
    }

    fn chapter(index: i32, url: &str, title: &str) -> BookChapter {
        BookChapter {
            url: url.to_string(),
            title: title.to_string(),
            index,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn replace_dedupes_by_url_keeps_first_and_roundtrips_fields() {
        let (repo, pool, dir) = test_repo("replace-dedup").await;
        repo.replace_chapters(
            1,
            &[
                chapter(0, "https://a/1", "一"),
                chapter(1, "https://a/1", "重复"),
                chapter(2, "", "空URL"),
                chapter(3, "", "空URL2"),
                chapter(4, "https://a/2", "二"),
            ],
            true,
            100,
        )
        .await
        .unwrap();
        let all = repo.get_chapters(1).await.unwrap();
        // 同批 URL 去重（首 URL 优先），空 URL 全保留
        assert_eq!(
            all.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![0, 2, 3, 4]
        );
        assert_eq!(all[0].title, "一");
        assert_eq!(all[1].title, "空URL");
        assert_eq!(all[2].title, "空URL2");
        assert_eq!(all[3].title, "二");

        // 字段往返：布尔/可选列不丢
        repo.replace_chapters(
            2,
            &[BookChapter {
                url: "u".into(),
                title: "t".into(),
                index: 0,
                tag: Some("tag".into()),
                is_vip: true,
                is_pay: true,
                is_volume: true,
                volume: Some("第一卷".into()),
                word_count: Some("1000".into()),
            }],
            true,
            100,
        )
        .await
        .unwrap();
        let got = repo.get_chapters(2).await.unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].is_vip && got[0].is_pay && got[0].is_volume);
        assert_eq!(got[0].tag.as_deref(), Some("tag"));
        assert_eq!(got[0].volume.as_deref(), Some("第一卷"));
        assert_eq!(got[0].word_count.as_deref(), Some("1000"));

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn append_dedupes_against_existing_and_continues_max_index() {
        let (repo, pool, dir) = test_repo("append-dedup").await;
        repo.replace_chapters(1, &[chapter(0, "a", "A"), chapter(1, "b", "B")], true, 100)
            .await
            .unwrap();
        // 与缓存尾部重叠 + 空 URL 章节：重叠跳过，空 URL 追加，index 从 2 顺延
        let all = repo
            .append_chapters(
                1,
                &[
                    chapter(9, "b", "B2"),
                    chapter(9, "c", "C"),
                    chapter(9, "", "E"),
                ],
                true,
                200,
            )
            .await
            .unwrap();
        assert_eq!(
            all.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", ""]
        );
        assert_eq!(
            all.iter().map(|c| c.index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        // 完全重叠的追加：无新增，返回现有目录
        let again = repo
            .append_chapters(1, &[chapter(9, "c", "C")], true, 300)
            .await
            .unwrap();
        assert_eq!(again.len(), 4);

        // 历史空洞也不会让追加撞主键：先造一个空洞，再追加
        sqlx::query("DELETE FROM chapters WHERE book_id = 1 AND chapter_index = 2")
            .execute(&pool)
            .await
            .unwrap();
        let after_gap = repo
            .append_chapters(1, &[chapter(9, "d", "D")], true, 400)
            .await
            .unwrap();
        assert_eq!(after_gap.last().unwrap().index, 4, "index 从最大值 +1 顺延");

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn toc_state_upsert_and_delete_roundtrip() {
        let (repo, pool, dir) = test_repo("toc-state").await;
        assert!(repo.get_toc_state(1).await.unwrap().is_none());

        repo.replace_chapters(1, &[chapter(0, "a", "A")], false, 111)
            .await
            .unwrap();
        let state = repo.get_toc_state(1).await.unwrap().unwrap();
        assert!(!state.complete);
        assert_eq!(state.fetched_at, 111);

        // 完整覆盖置完成
        repo.replace_chapters(1, &[chapter(0, "a", "A")], true, 222)
            .await
            .unwrap();
        let state = repo.get_toc_state(1).await.unwrap().unwrap();
        assert!(state.complete);
        assert_eq!(state.fetched_at, 222);

        // 无新增的追加同样更新状态
        repo.append_chapters(1, &[chapter(0, "a", "A")], false, 333)
            .await
            .unwrap();
        let state = repo.get_toc_state(1).await.unwrap().unwrap();
        assert!(!state.complete);
        assert_eq!(state.fetched_at, 333);

        // 删除连状态一起清
        repo.delete_chapters(1).await.unwrap();
        assert!(repo.get_chapters(1).await.unwrap().is_empty());
        assert!(repo.get_toc_state(1).await.unwrap().is_none());

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 验收口径：按 (book_id, chapter_index) / (book_id, url) 的查询必须走
    /// 索引，不允许全表扫描（百万行级目录的查询预算依赖这一点）。
    #[tokio::test]
    async fn lookups_use_indexes_never_table_scans() {
        let (repo, pool, dir) = test_repo("explain-plan").await;
        repo.replace_chapters(1, &[chapter(0, "a", "A")], true, 100)
            .await
            .unwrap();

        for (sql, label) in [
            (
                "EXPLAIN QUERY PLAN SELECT chapter_index FROM chapters                  WHERE book_id = 1 AND chapter_index = 0",
                "主键查询",
            ),
            (
                "EXPLAIN QUERY PLAN SELECT chapter_index FROM chapters                  WHERE book_id = 1 AND url = 'a'",
                "URL 反查",
            ),
            (
                "EXPLAIN QUERY PLAN SELECT chapter_index, url, title FROM chapters                  WHERE book_id = 1 ORDER BY chapter_index ASC",
                "整书读取",
            ),
        ] {
            let rows = sqlx::query(sql).fetch_all(&pool).await.unwrap();
            let joined: String = rows
                .iter()
                .map(|row| row.get::<String, _>("detail"))
                .collect::<Vec<_>>()
                .join("; ");
            assert!(
                !joined.contains("SCAN chapters"),
                "{label}不允许全表扫描: {joined}"
            );
            assert!(joined.contains("SEARCH"), "{label}应命中索引: {joined}");
        }

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn find_by_url_and_index_and_count() {
        let (repo, pool, dir) = test_repo("find-lookup").await;
        repo.replace_chapters(
            1,
            &[
                chapter(0, "a", "A"),
                chapter(1, "b", "B"),
                chapter(2, "", "E"),
            ],
            true,
            100,
        )
        .await
        .unwrap();
        assert_eq!(
            repo.find_chapter_by_url(1, "b").await.unwrap(),
            Some((1, "B".into()))
        );
        assert_eq!(repo.find_chapter_by_url(1, "missing").await.unwrap(), None);
        assert_eq!(
            repo.find_chapter_by_url(1, "").await.unwrap(),
            Some((2, "E".into())),
            "空 URL 也能反查（按 index 取首个）"
        );
        assert_eq!(
            repo.find_chapter_by_index(1, 1)
                .await
                .unwrap()
                .map(|c| c.url),
            Some("b".into())
        );
        assert!(repo.find_chapter_by_index(1, 9).await.unwrap().is_none());
        assert_eq!(repo.count_chapters(1).await.unwrap(), 3);
        assert_eq!(repo.count_chapters(2).await.unwrap(), 0);

        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
