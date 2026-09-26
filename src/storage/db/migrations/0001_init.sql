-- 初始 schema。
--
-- 历史迁移 0002/0004 是纯粹的兼容补丁（补 user_ns 主键、删除从未被读写的
-- book_cache/chapter_cache），已直接并入本文件；0003 的 user_sessions 表
-- 随 JWT 改造删除——令牌不再落库，改为签名令牌 + 版本号撤销。
-- 多用户改造撤销后（单用户化），users 表的权限列（enable_*、is_admin）
-- 一并删除：唯一的账号拥有全部能力。
--
-- 变更本文件后需删除 storage/reader.db 重建（sqlx 会校验已应用迁移的校验和）。

CREATE TABLE IF NOT EXISTS users (
    username TEXT NOT NULL PRIMARY KEY,
    -- Argon2id PHC 字符串；无独立 salt 列，盐与参数都编码在 PHC 里
    password TEXT NOT NULL,
    last_login_at INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,
    -- 自增即撤销该用户此前签发的所有 JWT（改密码时使用）
    token_version INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS book_sources (
    user_ns TEXT NOT NULL DEFAULT 'default',
    book_source_url TEXT NOT NULL,
    book_source_name TEXT NOT NULL,
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_source_url)
);

-- 通用 JSON 文档：书签、替换规则、书分组、AI 模型配置都存这里
CREATE TABLE IF NOT EXISTS json_documents (
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (namespace, name)
);

CREATE TABLE IF NOT EXISTS ai_book_memories (
    user_ns TEXT NOT NULL,
    book_key TEXT NOT NULL,
    book_url TEXT NOT NULL,
    json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_ns, book_key)
);

CREATE INDEX IF NOT EXISTS idx_ai_book_memories_user_ns
ON ai_book_memories(user_ns);
