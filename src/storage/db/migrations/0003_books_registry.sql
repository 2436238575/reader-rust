-- 书籍登记表：bookId 是书籍的稳定身份键，不随换源变化。
--
-- book_id 形如 `book_<n>`（per user_ns 自增），自带命名空间避免与缓存目录里
-- 的 md5-hex 撞名，日志里一眼可辨。book_url 是上游地址（换源时改绑），
-- (user_ns, book_url) 唯一——同一地址永远解析到同一个 bookId。
-- book_source_id 为 NULL 表示本地书（local-txt:/local-epub: 伪书源）。
--
-- 搜索/发现等列表产出即登记（响应才能带 bookId）；非在架书籍的登记行
-- 由 purgeCache 联动回收。

CREATE TABLE IF NOT EXISTS books (
    user_ns TEXT NOT NULL DEFAULT 'default',
    book_id TEXT NOT NULL,
    book_url TEXT NOT NULL,
    book_source_id INTEGER,
    toc_url TEXT,
    name TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_books_url
ON books(user_ns, book_url);

CREATE INDEX IF NOT EXISTS idx_books_source
ON books(user_ns, book_source_id);
