-- 阅读统计明细：按 (用户, 书籍, 章节, 自然日) 存秒数。
--
-- 不存聚合：总时长/今日/已读章数/读完本数/连续天数都在查询时派生，避免
-- 「聚合行被重复累加」后无法对账。day_key 是整数 epoch day（不是日期字符串），
-- 天边界由服务层按固定时区偏移计算（本地 04:00 起算新一天，见
-- `service/read_stats_service.rs`）。
--
-- `backfilled` 标记历史回填行：旧 `readChapterHistory` 只有「读过哪章」，
-- 没有阅读时长；回填行只用于已读章数/读完判定，不计入今日时长与连续天数。
-- 客户端之后对同一 (书, 章, 日) 上报真实秒数时，upsert 会把 backfilled 清 0。
CREATE TABLE IF NOT EXISTS read_chapter_days (
    user_ns TEXT NOT NULL,
    book_id INTEGER NOT NULL,
    chapter_index INTEGER NOT NULL,
    day_key INTEGER NOT NULL,
    seconds INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    backfilled INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_ns, book_id, chapter_index, day_key)
);

-- 今日时长与连续天数都按 (user_ns, day_key) 过滤；主键以 user_ns 开头，
-- 单独按天的查询需要这条索引。
CREATE INDEX IF NOT EXISTS idx_read_chapter_days_ns_day
    ON read_chapter_days(user_ns, day_key);

-- 旧版阅读统计（json_documents['reading-stats'].totalSeconds）没有明细，
-- 无法拆到自然日/章节；回填时把总额单独存这里，统计总时长时相加。
-- backfilled_at > 0 表示该用户已完成一次回填（文档读取失败时不写标记，
-- 下次启动重试）。
CREATE TABLE IF NOT EXISTS read_stats_legacy (
    user_ns TEXT PRIMARY KEY,
    legacy_total_seconds INTEGER NOT NULL DEFAULT 0,
    backfilled_at INTEGER NOT NULL DEFAULT 0
);
