-- 删除两张历史遗留死表：自建库以来全代码库无任何 SQL 读写。
-- 书籍元信息直接按需抓取，章节正文缓存走 storage/cache 文件（FileCache），
-- 章节列表缓存走 storage/data/<ns>/chapters/*.json，均不经过这两张表。
DROP TABLE IF EXISTS book_cache;
DROP TABLE IF EXISTS chapter_cache;
