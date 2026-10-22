-- 书架分组从 json_documents 的整文档拆为实体表，并修复位域分配。
--
-- 位域约定（docs/api/book-group.md，与阅读3.0 一致）：书籍的 group 字段是
-- 位掩码，组 id 必须是 2 的幂。此前建组用 max_id+1 分配，第 3 个组 id=3
--（0b11）会同时命中组 1、组 2 的过滤。建表后分配最小未占用的幂位
--（1, 2, 4, 8…），删除组时同步清掉书籍位掩码里的对应位，空位可被回填。
--
-- 上限 31 个组（1<<0 ..= 1<<30）：前端的位运算是 32 位有符号整数，
-- 1<<31 会变负号，63 位上限在前端根本不可用。CHECK 在表级兜住
-- 客户端直传的非法 id（传 3 就是同时进两个组的老 bug）。
--
-- 破坏性变更：老数据随 storage/ 重建丢弃（无就地迁移）。

CREATE TABLE IF NOT EXISTS book_groups (
    user_ns TEXT NOT NULL,
    group_id INTEGER NOT NULL,
    group_name TEXT NOT NULL,
    order_no INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_ns, group_id),
    CHECK (group_id > 0 AND group_id <= 1073741824 AND (group_id & (group_id - 1)) = 0)
);
