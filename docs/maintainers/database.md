# 数据库

运行期数据存在 SQLite（默认 `storage/reader.db`，`DATABASE_URL` 可改）。sqlx 访问，连接池 10 条，WAL 模式（`synchronous=NORMAL`），启动时自动执行 `src/storage/db/migrations/` 下的迁移。

当前全部表结构都定义在唯一的迁移文件 `0001_init.sql` 里（历史迁移已逐批并入）。新增结构请新建迁移文件，不要改已发布的迁移——sqlx 按校验和核对已应用的迁移，改动历史文件会让旧库无法启动；确需重写时按破坏性变更处理：删除 `storage/reader.db` 重建。

三张底表的身份模型：

- **用户**：`uid` 自增，是存储命名空间的锚点（`user_ns = user_<uid>`）；`username` 只用于登录与展示。
- **书源**：`book_source_id`（per user_ns 自增）是身份键；URL 只是数据字段与导入去重键。
- **书籍**：`book_id` 是稳定身份，不随换源变化；换源只改绑 `book_url` / `book_source_id`。库里存 INTEGER（rowid 单调自增），对外（API/缓存路径/日志）一律是 `book_<n>` 文本，转换在 repo 边界完成。

所有表都按 `user_ns` 隔离；单用户部署下整库只有一个命名空间，但键结构保持多用户形态。书源 id 分配走 `MAX(id)+1`（per user_ns，调用方持事务）；书籍 id 是 rowid 别名，不显式插入即由 SQLite 单调分配。

## users

唯一账号。单用户系统，一般只有一行。

| 字段            | 类型    | 用途                                                                       |
| --------------- | ------- | -------------------------------------------------------------------------- |
| `uid`           | INTEGER | 主键，自增身份 id。命名空间锚点（`user_<uid>`），进 JWT claims             |
| `username`      | TEXT    | 登录名与前端展示名，唯一索引；只允许小写字母/数字/下划线                   |
| `password_hash` | TEXT    | Argon2id PHC 字符串；盐与参数编码在 PHC 里，无独立 salt 列                 |
| `token_version` | INTEGER | 撤销版本号：改密码自增，该账号此前签发的所有 JWT 立即失效                  |
| `created_at`    | INTEGER | 创建时间，毫秒时间戳                                                       |
| `last_login_at` | INTEGER | 最近登录时间，毫秒时间戳                                                   |

## book_sources

书源配置。主键 `(user_ns, book_source_id)`，另有唯一索引 `(user_ns, book_source_url)`。

| 字段               | 类型    | 用途                                                                     |
| ------------------ | ------- | ------------------------------------------------------------------------ |
| `user_ns`          | TEXT    | 数据隔离键（`user_<uid>`）                                               |
| `book_source_id`   | INTEGER | 身份键，per user_ns 自增；接口参数、代理令牌、缓存关联都用它             |
| `book_source_url`  | TEXT    | 书源地址。数据字段 + 导入去重键：批量导入按 URL 去重，命中时保留本地 id  |
| `book_source_name` | TEXT    | 名称快照，列表排序与展示用（不必解析 JSON）                              |
| `json`             | TEXT    | 整份书源 JSON；规则字段随阅读3.0 规范演进，不打散成列                    |
| `updated_at`       | INTEGER | 更新时间，秒级时间戳；列表按它倒序                                       |

## books

书籍登记表。`book_id INTEGER PRIMARY KEY`（rowid 别名），唯一索引 `(user_ns, book_url)`，普通索引 `(user_ns, book_source_id)`。

搜索/发现等列表产出即登记（响应才能带 bookId）；非在架的登记行由 `purgeCache`（scope=user）联动回收。

| 字段              | 类型    | 用途                                                                       |
| ----------------- | ------- | -------------------------------------------------------------------------- |
| `user_ns`         | TEXT    | 数据隔离键                                                                 |
| `book_id`         | INTEGER | 稳定身份键（rowid 别名，单调自增）；对外格式化为 `book_<n>`，不随换源变化  |
| `book_url`        | TEXT    | 上游书籍地址。同一地址永远解析到同一个 bookId（唯一索引保证）              |
| `book_source_id`  | INTEGER | 当前绑定的书源 id；`NULL` 表示本地书（`local-txt:`/`local-epub:` 伪书源）  |
| `toc_url`         | TEXT    | 目录地址；换源改绑时清空，下次访问重新抓取                                 |
| `name`            | TEXT    | 书名快照（登记时可能还是空串，后续登记补全），默认 `''`                    |
| `author`          | TEXT    | 作者快照，默认 `''`                                                        |
| `created_at`      | INTEGER | 登记时间，秒级时间戳                                                       |
| `updated_at`      | INTEGER | 最近登记/改绑时间，秒级时间戳                                              |

## shelf_books

书架。主键 `(user_ns, book_id)`，普通索引 `(user_ns, position)`。

身份与绑定字段（bookUrl/origin/tocUrl/name/author）不落此表——读取时 JOIN `books`（必要时再 JOIN `book_sources`）注入，换源改绑只动 `books` 一行。

| 字段                   | 类型    | 用途                                                                 |
| ---------------------- | ------- | -------------------------------------------------------------------- |
| `user_ns`              | TEXT    | 数据隔离键                                                           |
| `book_id`              | INTEGER | 关联 `books` 登记表                                                  |
| `position`             | INTEGER | 书架排序；整架替换/拖拽排序时按数组序重写                            |
| `group_bits`           | INTEGER | 分组位掩码（对应阅读3.0 的 `Book.group`）                            |
| `dur_chapter_index`    | INTEGER | 阅读进度：章节序号                                                   |
| `dur_chapter_pos`      | INTEGER | 阅读进度：章内位置                                                   |
| `dur_chapter_time`     | INTEGER | 阅读进度上报时间，毫秒时间戳（前端 `Date.now()`）                    |
| `dur_chapter_title`    | TEXT    | 阅读进度：章节标题快照                                               |
| `cover_url`            | TEXT    | 封面地址快照（入架时抓取，换源时刷新）                               |
| `custom_cover_url`     | TEXT    | 自定义封面（优先于 `cover_url` 展示）                                |
| `can_update`           | INTEGER | 是否参与更新检查，布尔以 0/1 存                                      |
| `intro`                | TEXT    | 简介快照                                                             |
| `latest_chapter_title` | TEXT    | 最新章节标题快照（更新检查后刷新）                                   |
| `last_check_time`      | INTEGER | 最近更新检查时间，毫秒时间戳                                         |
| `total_chapter_num`    | INTEGER | 总章节数快照                                                         |
| `book_type`            | INTEGER | 书籍类型，沿用阅读3.0 的 `Book.type` 语义                            |
| `word_count`           | TEXT    | 字数快照（如 `123万字`）                                             |
| `info_html`            | TEXT    | 详情页 HTML 快照（净化后）                                           |
| `toc_html`             | TEXT    | 目录页 HTML 快照（净化后）                                           |
| `kind`                 | TEXT    | 分类标签快照                                                         |
| `update_time`          | TEXT    | 上游更新时间快照，沿用 `Book.updateTime` 语义                        |
| `can_re_name`          | TEXT    | 沿用阅读3.0 的 `Book.canReName` 语义                                 |
| `download_urls`        | TEXT    | 下载地址快照，沿用 `Book.downloadUrls` 语义                          |
| `charset`              | TEXT    | 书源字符集快照                                                       |
| `created_at`           | INTEGER | 入架时间，秒级时间戳                                                 |
| `updated_at`           | INTEGER | 行最近写入时间，秒级时间戳                                           |

## json_documents

通用 JSON 文档存取。书分组、AI 模型配置、userdata（最近阅读/阅读统计等）存这张表，不各建新表。主键 `(user_ns, name)`。

> 书签与替换规则曾是这里的两个文档，2026-10 拆为实体表（见下两节）——
> 整文档读改写有并发互覆、无 delete、删书级联要全量过滤三个老问题。

| 字段         | 类型    | 用途                                                          |
| ------------ | ------- | ------------------------------------------------------------- |
| `user_ns`    | TEXT    | 数据隔离键                                                    |
| `name`       | TEXT    | 文档名；userdata 类文档名走白名单校验（只允许字母数字与 `._-`） |
| `json`       | TEXT    | 文档内容，整份 JSON                                           |
| `updated_at` | INTEGER | 更新时间，毫秒时间戳                                          |

## bookmarks

书签。主键 `(user_ns, book_id, chapter_index, chapter_pos)`——同书同位置重复保存覆盖。普通索引 `(user_ns, book_id)`（删书级联用）。

| 字段            | 类型    | 用途                                       |
| --------------- | ------- | ------------------------------------------ |
| `user_ns`       | TEXT    | 数据隔离键                                 |
| `book_id`       | INTEGER | 关联 books 登记表                          |
| `chapter_index` | INTEGER | 章节序号（身份键之一）                     |
| `chapter_pos`   | INTEGER | 章内位置（身份键之一）                     |
| `time`          | INTEGER | 创建/更新时间，毫秒时间戳                  |
| `book_name`     | TEXT    | 书名快照（展示用，不参与判等）             |
| `book_author`   | TEXT    | 作者快照（展示用）                         |
| `chapter_name`  | TEXT    | 章节标题快照                               |
| `book_text`     | TEXT    | 进度文案快照                               |
| `content`       | TEXT    | 书签备注/摘录                              |

## replace_rules

替换规则。主键 `(user_ns, name)`（沿用阅读3.0 按名称 upsert 的判等语义）。

| 字段          | 类型    | 用途                                                  |
| ------------- | ------- | ----------------------------------------------------- |
| `user_ns`     | TEXT    | 数据隔离键                                            |
| `name`        | TEXT    | 规则名（身份键）                                      |
| `id`          | INTEGER | 客户端带来的 id（时间戳），纯数据列                   |
| `group_name`  | TEXT    | 分组名（列名避让 SQL 关键字 group）                   |
| `pattern`     | TEXT    | 匹配规则                                              |
| `replacement` | TEXT    | 替换内容                                              |
| `scope`       | TEXT    | 作用域：`book:<bookId>` / `source:<bookSourceId>`，空为全局 |
| `is_enabled`  | INTEGER | 布尔以 0/1 存                                         |
| `is_regex`    | INTEGER | 布尔以 0/1 存                                         |
| `rule_order`  | INTEGER | 排序权重（列名避让 SQL 关键字 order）                 |

## ai_book_memories

AI 书籍记忆（AI 资料编排产物）。主键 `(user_ns, book_id)`。

| 字段         | 类型    | 用途                                                                       |
| ------------ | ------- | -------------------------------------------------------------------------- |
| `user_ns`    | TEXT    | 数据隔离键                                                                 |
| `book_id`    | INTEGER | 关联 `books` 登记表；换源后记忆天然保留                                    |
| `book_url`   | TEXT    | 冗余校验列：WebDAV 恢复重建登记表后逐行比对，同 id 不同 URL 的记忆删除防串书 |
| `json`       | TEXT    | 整份记忆 JSON                                                              |
| `updated_at` | INTEGER | 更新时间，毫秒时间戳                                                       |

## 设计取舍

- **书源存整份 JSON** 而不是打散成列——书源字段会随阅读3.0 规范演进，打散后每次演进都要改表；代价是无法按规则字段建索引。
- **章节正文存文件而不是数据库**——单章动辄数十 KB 且数量极多，进库会让文件迅速膨胀并拖慢备份。正文缓存以 MD5 文件名落在 `storage/cache/` 下，数据库不存索引。
- **`json_documents` 通用化**——零散键值（分组、配置、userdata）复用一张表，减少迁移次数；有实体语义的（书签、替换规则）拆实体表。
- **不声明外键**——表间一致性由 service 层维护（如删书时级联清书架行、书签、AI 记忆与图片记录）；`foreign_keys` pragma 虽开，但表结构里没有 FK 约束。
- **时间戳单位不统一是历史现状**：`users` / `json_documents` / `ai_book_memories` 用毫秒，其余表用秒；`shelf_books` 里来自前端的字段（`dur_chapter_time`、`last_check_time`）随前端用毫秒。列的用途表里已逐列标注。
