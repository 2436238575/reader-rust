-- 书架入 SQLite：shelf_books 按 (user_ns, book_id) 关联 books 登记表。
--
-- 列全展开（进度/分组/排序/元数据快照）；bookUrl/origin/tocUrl/name/author
-- 等身份与绑定字段不落此表——读取时 JOIN books（必要时再 JOIN book_sources）
-- 注入，换源改绑只动 books 一行。
--
-- 迁移自 data/<ns>/bookshelf.json 不做：升级按破坏性变更删 storage/ 重建。

CREATE TABLE IF NOT EXISTS shelf_books (
    user_ns TEXT NOT NULL,
    book_id TEXT NOT NULL,
    -- 书架排序：整架替换/拖拽排序时按数组序重写
    position INTEGER NOT NULL,
    -- 分组位掩码（对应 Book.group）
    group_bits INTEGER,
    -- 阅读进度
    dur_chapter_index INTEGER,
    dur_chapter_pos INTEGER,
    dur_chapter_time INTEGER,
    dur_chapter_title TEXT,
    -- 元数据快照（入架时抓取，换源时刷新）
    cover_url TEXT,
    custom_cover_url TEXT,
    can_update INTEGER,
    intro TEXT,
    latest_chapter_title TEXT,
    last_check_time INTEGER,
    total_chapter_num INTEGER,
    type INTEGER,
    word_count TEXT,
    info_html TEXT,
    toc_html TEXT,
    kind TEXT,
    update_time TEXT,
    can_re_name TEXT,
    download_urls TEXT,
    charset TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_id)
);

CREATE INDEX IF NOT EXISTS idx_shelf_books_ns ON shelf_books(user_ns, position);
