-- 章节标题持久覆盖层：用户手动改的章节名。
--
-- 目录显示标题的事实来源是目录缓存文件（toc/<bookId>.json），但它会在
-- refresh=1 重抓/重解析时被整份重写——改名若直接改缓存就会丢。覆盖层
-- 在读取时叠加：目录缓存保持「上游事实」，override 记录「用户的改名」，
-- 重抓后按 chapter_url 重新套用。
--
-- 整部署共享，不带 user_ns（与正文池三表同口径；当前单用户部署）。
-- 换源后 chapter_url 变化，旧覆盖行成为孤儿：读取按 url 查不到即忽略，
-- 不主动清理（与正文历史不接续同一口径）。删书时级联清理。
CREATE TABLE IF NOT EXISTS chapter_title_overrides (
    book_id INTEGER NOT NULL,
    chapter_url TEXT NOT NULL,
    title TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (book_id, chapter_url)
);

CREATE INDEX IF NOT EXISTS idx_title_overrides_book ON chapter_title_overrides(book_id);
