-- 章节正文编辑版的索引表（方案 B：正文仍落 data/ 文件，索引进表）。
--
-- 此前的 `_index.json` 是整文档读-改-写：两章并发保存互相覆盖丢条目，
-- 删除从未编辑过的书时因目录不存在 500。进表后：保存是单行 upsert、
-- 删书级联是一句 DELETE、目录徽章按书查询。
--
-- 破坏性变更：老数据随 storage/ 重建丢弃（无就地迁移）。

-- 章节编辑索引：身份 = (user_ns, book_id, chapter_url)。
-- chapter_index/chapter_title 是展示快照；edited_at 毫秒时间戳。
CREATE TABLE IF NOT EXISTS chapter_edits (
    user_ns TEXT NOT NULL,
    book_id INTEGER NOT NULL,
    chapter_url TEXT NOT NULL,
    -- 以下为数据列
    chapter_index INTEGER NOT NULL DEFAULT 0,
    chapter_title TEXT NOT NULL DEFAULT '',
    edited_at INTEGER NOT NULL DEFAULT 0,
    -- 恢复校验冗余列：WebDAV 恢复重建登记表后，按它识别「id 被别的书复用」
    -- 的残留编辑版（与 ai_book_memories.book_url 同一手法）
    book_url TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_ns, book_id, chapter_url)
);

CREATE INDEX IF NOT EXISTS idx_chapter_edits_book ON chapter_edits(user_ns, book_id);
