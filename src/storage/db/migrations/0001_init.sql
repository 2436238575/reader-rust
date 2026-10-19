-- 初始 schema：当前全部表结构都定义在本文件。
--
-- 历史迁移（书源 bookSourceId、books 登记表、users.uid、shelf_books 等）已逐批
-- 并入本文件；正常升级路径是删除 storage/reader.db 重建（sqlx 会校验已应用迁移
-- 的校验和，改动本文件后旧库无法启动）。
--
-- 各表用途与字段说明见文档站「维护者 · 数据库」。

-- 唯一账号。uid 是身份与命名空间锚点（user_ns = user_<uid>，进 JWT claims）；
-- username 只用于登录与前端展示。
CREATE TABLE IF NOT EXISTS users (
    uid INTEGER PRIMARY KEY,
    username TEXT NOT NULL,
    -- Argon2id PHC 字符串；盐与参数都编码在 PHC 里，无独立 salt 列
    password_hash TEXT NOT NULL,
    -- 自增即撤销该用户此前签发的所有 JWT（改密码时使用）
    token_version INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,
    last_login_at INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username ON users(username);

-- 书源。book_source_id 是身份键（per user_ns 自增，repo 层 MAX+1 分配）；
-- book_source_url 降为数据字段 + 导入去重键。
CREATE TABLE IF NOT EXISTS book_sources (
    user_ns TEXT NOT NULL,
    book_source_id INTEGER NOT NULL,
    book_source_url TEXT NOT NULL,
    book_source_name TEXT NOT NULL,
    -- 整份书源 JSON：规则字段随阅读3.0 规范演进，不打散成列
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_source_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_book_sources_url
ON book_sources(user_ns, book_source_url);

-- 书籍登记表：book_id 是书籍的稳定身份键，不随换源变化；换源只改绑
-- book_url / book_source_id。列存 INTEGER（rowid 别名，单调自增、无分配竞态），
-- 对外（API/缓存路径/日志）一律格式化为 `book_<n>` 文本，转换在 repo 边界完成。
-- (user_ns, book_url) 唯一：同一上游地址永远解析到同一个 bookId。
-- book_source_id 为 NULL 表示本地书（local-txt:/local-epub: 伪书源）。
-- 搜索/发现等列表产出即登记（响应才能带 bookId）；非在架登记行由 purgeCache 联动回收。
CREATE TABLE IF NOT EXISTS books (
    book_id INTEGER PRIMARY KEY,
    user_ns TEXT NOT NULL,
    book_url TEXT NOT NULL,
    book_source_id INTEGER,
    toc_url TEXT,
    name TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_books_url
ON books(user_ns, book_url);

CREATE INDEX IF NOT EXISTS idx_books_source
ON books(user_ns, book_source_id);

-- 书架：按 (user_ns, book_id) 关联 books 登记表，position 排序。
-- 身份与绑定字段（bookUrl/origin/tocUrl/name/author）不落此表——读取时
-- JOIN books（必要时再 JOIN book_sources）注入，换源改绑只动 books 一行。
CREATE TABLE IF NOT EXISTS shelf_books (
    user_ns TEXT NOT NULL,
    book_id INTEGER NOT NULL,
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
    book_type INTEGER,
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

-- 通用 JSON 文档：书签、替换规则、书分组、AI 模型配置、userdata 都存这里。
CREATE TABLE IF NOT EXISTS json_documents (
    user_ns TEXT NOT NULL,
    name TEXT NOT NULL,
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, name)
);

-- AI 书籍记忆：book_id 关联 books 登记表；book_url 冗余一列供 WebDAV 恢复后
-- 校验「同 id 同书」防串书。
CREATE TABLE IF NOT EXISTS ai_book_memories (
    user_ns TEXT NOT NULL,
    book_id INTEGER NOT NULL,
    book_url TEXT NOT NULL,
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_id)
);
