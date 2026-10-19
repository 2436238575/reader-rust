-- 用户自增 uid：命名空间与存储路径的锚点。
--
-- username 只用于登录与前端展示；uid 进 JWT claims，user_ns 落 `u<uid>`。
-- 存量行由启动引导回填（见 storage/db/mod.rs），正常升级路径删除 storage/ 重建。

ALTER TABLE users ADD COLUMN uid INTEGER;

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_uid ON users(uid);
