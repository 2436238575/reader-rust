-- 书源稳定 id：bookSourceId。身份键从 book_source_url 切换为自增 id，
-- URL 降为数据字段 + 导入去重键（保留 (user_ns, book_source_url) 唯一性由主键承担）。
--
-- 存量行由启动引导按 rowid 顺序回填（见 storage/db/mod.rs）；新插入行在
-- repo 层按 MAX(book_source_id)+1 分配。破坏性变更：随版本升级说明删除 storage/ 重建。

ALTER TABLE book_sources ADD COLUMN book_source_id INTEGER;

CREATE UNIQUE INDEX IF NOT EXISTS idx_book_sources_id
ON book_sources(user_ns, book_source_id);
