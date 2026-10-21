-- 书签与替换规则从 json_documents 拆为实体表。
--
-- 整文档读改写的旧形态有三个老毛病：并发保存互相覆盖、没有 delete、
-- 删书级联要加载整个文档过滤。实体表之后：保存是单行 upsert、删书
-- 级联是一句 DELETE WHERE book_id=?、并发不再互踩。
--
-- 破坏性变更：老数据随 storage/ 重建丢弃（无就地迁移）。

-- 书签：身份 = (user_ns, book_id, chapter_index, chapter_pos, kind)，
-- 同位同类重复保存覆盖。book_name/book_author 是展示快照，不参与判等。
-- chapter_pos 是万分比章内进度（0–10000）：章书签（kind=bookmark）恒 0；
-- 划线（kind=underline）取收藏那一刻的章内进度。kind 进主键后，
-- 章书签与章首划线（pos=0）不再撞键，同章多类、多进度共存。
CREATE TABLE IF NOT EXISTS bookmarks (
    user_ns TEXT NOT NULL,
    book_id INTEGER NOT NULL,
    chapter_index INTEGER NOT NULL,
    chapter_pos INTEGER NOT NULL,
    kind TEXT NOT NULL DEFAULT 'bookmark',
    -- 以下为数据列
    time INTEGER NOT NULL DEFAULT 0,
    book_name TEXT NOT NULL DEFAULT '',
    book_author TEXT NOT NULL DEFAULT '',
    chapter_name TEXT NOT NULL DEFAULT '',
    book_text TEXT NOT NULL DEFAULT '',
    content TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_ns, book_id, chapter_index, chapter_pos, kind)
);

CREATE INDEX IF NOT EXISTS idx_bookmarks_book ON bookmarks(user_ns, book_id);

-- 替换规则：按 (user_ns, name) upsert（沿用阅读3.0 的判等语义），
-- id 只是客户端带来的数据列。scope 形如 book:book_7 / source:<bookSourceId>。
CREATE TABLE IF NOT EXISTS replace_rules (
    user_ns TEXT NOT NULL,
    name TEXT NOT NULL,
    id INTEGER NOT NULL DEFAULT 0,
    group_name TEXT,
    pattern TEXT NOT NULL,
    replacement TEXT NOT NULL DEFAULT '',
    scope TEXT,
    is_enabled INTEGER NOT NULL DEFAULT 1,
    is_regex INTEGER NOT NULL DEFAULT 0,
    rule_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_ns, name)
);
