-- 章节登记表：章节身份 (book_id, chapter_index) 的持久载体，整部署共享、
-- 不带用户命名空间。目录此前只存在于可再生的文件缓存
--（cache/<ns>/toc/<bookId>.json + 旁路 meta），index 只是抓取时的数组下标；
-- 落库后 index 有了稳定归属，服务端可以只凭 (bookId, index) 定位章节，
-- 对外 API 不再要求客户端持有 chapterUrl。
-- 换源整表替换：旧源章节行删除，正文历史按 chapter_url 键保留为孤儿
--（语义与文件缓存时代一致）。
-- 不声明外键：目录写入允许先于登记行存在（目录抓取不依赖 books 行），
-- 与库内其他表的无外键风格一致；孤儿行由换源/删书路径显式清理。
CREATE TABLE IF NOT EXISTS chapters (
    book_id INTEGER NOT NULL,
    chapter_index INTEGER NOT NULL,
    url TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    tag TEXT,
    is_vip INTEGER NOT NULL DEFAULT 0,
    is_pay INTEGER NOT NULL DEFAULT 0,
    is_volume INTEGER NOT NULL DEFAULT 0,
    volume TEXT,
    word_count TEXT,
    PRIMARY KEY (book_id, chapter_index)
);

-- 同一本书内章节 URL 唯一，是追加去重的数据库兜底。空 URL 章节真实存在
--（个别书源的章节没有独立地址，本地 TXT 全部为空），不参与唯一约束。
CREATE UNIQUE INDEX IF NOT EXISTS idx_chapters_book_url
    ON chapters(book_id, url) WHERE url != '';

-- 按 URL / 按 index 的查询索引。部分唯一索引带 WHERE 谓词，普通查询
--（不含 url != '' 条件）不能稳定命中它，这里补一份无谓词索引。
CREATE INDEX IF NOT EXISTS idx_chapters_book_url_lookup ON chapters(book_id, url);

-- 目录抓取状态：替代原 toc/<bookId>.meta.json 的 complete 旁路标记。
-- 与 books 行解耦（登记行由 registry 管理，目录写入不应依赖它存在）。
-- fetched_at 是最近一次抓取落库的 Unix 秒；读侧按 complete 分档判 TTL
--（完整 1 小时、未完成 60 秒，与原文件缓存口径一致）。
CREATE TABLE IF NOT EXISTS book_toc_state (
    book_id INTEGER PRIMARY KEY,
    complete INTEGER NOT NULL DEFAULT 0,
    fetched_at INTEGER NOT NULL DEFAULT 0
);
