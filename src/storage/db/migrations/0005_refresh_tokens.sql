-- Refresh Token 轮换状态。
--
-- 令牌本体是随机串，明文只下发一次；这里只存 SHA-256 摘要，数据库泄露
-- 也无法还原可用令牌。一次登录 = 一条 family，每次轮换在同一个 family 下
-- 追加新行，旧行只标记 used_at 不删除，用来识别重放。
CREATE TABLE IF NOT EXISTS refresh_tokens (
    token_hash    TEXT PRIMARY KEY,
    uid           INTEGER NOT NULL,
    token_version INTEGER NOT NULL,
    family_id     TEXT NOT NULL,
    issued_at     INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    used_at       INTEGER,
    revoked_at    INTEGER
);

CREATE INDEX IF NOT EXISTS idx_refresh_tokens_uid ON refresh_tokens(uid);
CREATE INDEX IF NOT EXISTS idx_refresh_tokens_family ON refresh_tokens(family_id);
CREATE INDEX IF NOT EXISTS idx_refresh_tokens_expires ON refresh_tokens(expires_at);
