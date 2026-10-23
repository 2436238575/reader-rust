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

-- 书源。整部署共享，不带用户命名空间。book_source_id 是稳定身份键，
-- AUTOINCREMENT 保证删除后也不复用；book_source_url 是数据字段 + 导入去重键。
CREATE TABLE IF NOT EXISTS book_sources (
    book_source_id INTEGER PRIMARY KEY AUTOINCREMENT,
    book_source_url TEXT NOT NULL,
    book_source_name TEXT NOT NULL,
    -- 整份书源 JSON：规则字段随阅读3.0 规范演进，不打散成列
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_book_sources_url
ON book_sources(book_source_url);

-- 书籍登记表：整部署共享，不带用户命名空间。book_id 是书籍的稳定身份键，
-- 不随换源变化；换源只改绑 book_url / book_source_id。AUTOINCREMENT 保证
-- 删除后 id 也不复用，正文历史不会因为 id 回收而串到另一本书。
-- book_url 唯一：同一上游地址永远解析到同一个 bookId；它只是数据字段，
-- 不再承担「同 id 同书」的校验。
-- book_source_id 为 NULL 表示本地书（local-txt:/local-epub: 伪书源）。
-- 搜索/发现等列表产出即登记（响应才能带 bookId）；登记行不再随清缓存回收。
CREATE TABLE IF NOT EXISTS books (
    book_id INTEGER PRIMARY KEY AUTOINCREMENT,
    book_url TEXT NOT NULL,
    book_source_id INTEGER,
    toc_url TEXT,
    name TEXT NOT NULL DEFAULT '',
    author TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_books_url ON books(book_url);

CREATE INDEX IF NOT EXISTS idx_books_source ON books(book_source_id);

-- ─── 正文池 / 时间线 / 最新指针 ──────────────────────────────────────────
-- 三张表都不带用户命名空间：正文整部署共享，同一段文本只存一份。
-- 正文池和时间线只插入，不更新；最新指针是唯一允许变化的表。

-- 正文池：content_hash 是原文的 sha256，用于去重。codec=0 明文 UTF-8，
-- codec=1 zstd。raw_size 是原文字节数，stored_size 是实际落库字节数。
CREATE TABLE IF NOT EXISTS chapter_contents (
    content_id INTEGER PRIMARY KEY,
    content_hash TEXT NOT NULL UNIQUE,
    codec INTEGER NOT NULL DEFAULT 0,
    raw_size INTEGER NOT NULL,
    stored_size INTEGER NOT NULL,
    content BLOB NOT NULL,
    created_at INTEGER NOT NULL
);

-- 时间线：一章的版本序列，只插入。kind 为 upstream / edited / imported。
-- base_version_id 只对用户版本有意义：编辑或手动导入时基于哪一版。
-- book_url / book_source_name 是抓取或保存时的快照，只作数据，不参与校验。
CREATE TABLE IF NOT EXISTS chapter_versions (
    version_id INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id INTEGER NOT NULL,
    chapter_url TEXT NOT NULL,
    content_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    book_source_id INTEGER,
    book_source_name TEXT NOT NULL DEFAULT '',
    base_version_id INTEGER,
    chapter_index INTEGER NOT NULL DEFAULT 0,
    chapter_title TEXT NOT NULL DEFAULT '',
    book_url TEXT NOT NULL DEFAULT '',
    fetch_url TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_versions_chapter
ON chapter_versions(book_id, chapter_url, version_id DESC);

CREATE INDEX IF NOT EXISTS idx_versions_book
ON chapter_versions(book_id, version_id DESC);

CREATE INDEX IF NOT EXISTS idx_versions_content
ON chapter_versions(content_id);

-- 最新指针：全局只存最近一次上游版本和最近一次用户版本。
-- 默认显示哪一版由 chapter_view_state 和这两个指针共同决定：
-- 用户选择 > latest_user_version_id > latest_upstream_version_id。
-- last_attempt_at 每次回源尝试都更新（避免失败后每个请求都重试）；
-- last_success_at 只在回源成功时更新；upstream_changed_at 记录内容最近一次变化。
CREATE TABLE IF NOT EXISTS chapter_latest (
    book_id INTEGER NOT NULL,
    chapter_url TEXT NOT NULL,
    latest_upstream_version_id INTEGER,
    latest_user_version_id INTEGER,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    upstream_changed_at INTEGER,
    edited_at INTEGER,
    last_error TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (book_id, chapter_url)
);

CREATE INDEX IF NOT EXISTS idx_latest_book ON chapter_latest(book_id);

-- 每用户每章选中的版本。切换版本是阅读偏好，仍然按用户隔离；
-- 有行表示用户手动选过，删除该行表示「跟随最新」。
CREATE TABLE IF NOT EXISTS chapter_view_state (
    user_ns TEXT NOT NULL,
    book_id INTEGER NOT NULL,
    chapter_url TEXT NOT NULL,
    selected_version_id INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_id, chapter_url)
);

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
