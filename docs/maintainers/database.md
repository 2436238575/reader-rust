# 数据库

运行期数据存在 SQLite（默认 `storage/reader.db`，`DATABASE_URL` 可改）。sqlx 访问，连接池 10 条，WAL 模式（`synchronous=NORMAL`），启动时自动执行 `src/storage/db/migrations/` 下的迁移。数据库保存结构化数据（书源、书架、进度、书签、AI 记忆等），以及**全部章节正文与版本历史**；图片与上传资源仍保存在文件系统里，目录布局见[数据存储结构](/maintainers/storage)。

表结构定义在 `src/storage/db/migrations/` 下：`0001_init.sql`（基础表结构、书籍/书源登记表、正文池与版本时间线）+ `0002_bookmarks_replace_rules.sql`（书签/替换规则实体表）+ `0004_book_groups.sql`（分组实体表）+ `0005_refresh_tokens.sql`（Refresh Token 轮换状态）。新增结构请新建迁移文件，不要改已发布的迁移——sqlx 按校验和核对已应用的迁移，改动历史文件会让旧库无法启动；确需重写时按破坏性变更处理：删除 `storage/reader.db` 重建。当前处于第一个 beta 之前，正文池相关的结构变更不提供就地迁移，升级时删除 `storage/` 重建。

三张底表的身份模型：

- **用户**：`uid` 自增，是用户数据的命名空间锚点（`user_ns = user_<uid>`）；`username` 只用于登录与展示。
- **书源**：整部署共享，不带 `user_ns`；`book_source_id` 是 `AUTOINCREMENT` 身份键，删除后也不复用；URL 只是数据字段与导入去重键。
- **书籍**：整部署共享，不带 `user_ns`；`book_id` 是 `AUTOINCREMENT` 稳定身份，不随换源变化，删除后也不复用。换源只改绑 `book_url` / `book_source_id`。对外（API/缓存路径/日志）一律是 `book_<n>` 文本，转换在 repo 边界完成。

正文池、版本时间线和最新指针也整部署共享；书架、书签、分组、替换规则、userdata、AI 记忆仍按 `user_ns` 隔离。`book_url` 只作为数据字段和选源展示，不再承担「同 id 同书」的校验——id 不复用之后这层校验没有意义。

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

## refresh_tokens

Refresh Token 的轮换状态。一次登录创建一条 `family_id`，每次换取在同一 family 下追加新行；旧行保留并标记 `used_at`，用于重放检测。

| 字段            | 类型    | 用途                                                                   |
| --------------- | ------- | ---------------------------------------------------------------------- |
| `token_hash`    | TEXT    | 主键，`sha256(完整 refresh token)` 的十六进制；明文永不落库            |
| `uid`           | INTEGER | 所属用户；索引 `idx_refresh_tokens_uid`                                |
| `token_version` | INTEGER | 签发时的 `users.token_version`，改密码后与当前值不一致即拒绝           |
| `family_id`     | TEXT    | 会话家族 id（登录时新建，轮换沿用）；索引 `idx_refresh_tokens_family`  |
| `issued_at`     | INTEGER | 签发时间（秒）                                                         |
| `expires_at`    | INTEGER | 过期时间（秒，滑动续期）；索引 `idx_refresh_tokens_expires`            |
| `used_at`       | INTEGER | 被消费时间（秒）；30 秒宽限窗口内允许重放，超窗重放吊销整条 family     |
| `revoked_at`    | INTEGER | 吊销时间（秒）；登出 / 改密 / 重放检测写入，非空即拒绝                 |

过期行在每次签发/轮换时顺带清理（`expires_at <= now`）；已消费但未过期的行保留，用于识别重放。

## book_sources

书源配置，整部署共享。主键 `book_source_id`（`AUTOINCREMENT`，删除后不复用），唯一索引 `book_source_url`。

| 字段               | 类型    | 用途                                                                     |
| ------------------ | ------- | ------------------------------------------------------------------------ |
| `book_source_id`   | INTEGER | 身份键，AUTOINCREMENT；接口参数、代理令牌、版本来源都用它                |
| `book_source_url`  | TEXT    | 书源地址。数据字段 + 导入去重键：批量导入按 URL 去重，命中时保留本地 id  |
| `book_source_name` | TEXT    | 名称快照，列表排序与展示用（不必解析 JSON）                              |
| `json`             | TEXT    | 整份书源 JSON；规则字段随阅读3.0 规范演进，不打散成列                    |
| `updated_at`       | INTEGER | 更新时间，秒级时间戳；列表按它倒序                                       |

## books

书籍登记表，整部署共享。`book_id INTEGER PRIMARY KEY AUTOINCREMENT`，唯一索引 `book_url`，普通索引 `book_source_id`。

搜索/发现等列表产出即登记（响应才能带 bookId）；登记行**不再随清缓存回收**，正文历史依赖它保持可达。从书架删除只删 `shelf_books` 行，`books` 行保留，重新加入同一地址会复用原 bookId。

| 字段              | 类型    | 用途                                                                       |
| ----------------- | ------- | -------------------------------------------------------------------------- |
| `book_id`         | INTEGER | 稳定身份键（AUTOINCREMENT，永不复用）；对外格式化为 `book_<n>`，不随换源变化 |
| `book_url`        | TEXT    | 上游书籍地址。同一地址解析到同一个 bookId；只作数据与选源，不做校验        |
| `book_source_id`  | INTEGER | 当前绑定的书源 id；`NULL` 表示本地书（`local-txt:`/`local-epub:` 伪书源）  |
| `toc_url`         | TEXT    | 目录地址；换源改绑时清空，下次访问重新抓取                                 |
| `name`            | TEXT    | 书名快照（登记时可能还是空串，后续登记补全），默认 `''`                    |
| `author`          | TEXT    | 作者快照，默认 `''`                                                        |
| `created_at`      | INTEGER | 登记时间，秒级时间戳                                                       |
| `updated_at`      | INTEGER | 最近登记/改绑时间，秒级时间戳                                              |

## chapter_contents

正文池，整部署共享，**只插入不更新**。相同原文只存一份，`content_hash` 是原文的 SHA-256。

| 字段           | 类型    | 用途                                                        |
| -------------- | ------- | ----------------------------------------------------------- |
| `content_id`   | INTEGER | 主键                                                        |
| `content_hash` | TEXT    | 原文 SHA-256，唯一索引；跨书、跨版本去重                    |
| `codec`        | INTEGER | `0` 明文 UTF-8，`1` zstd                                    |
| `raw_size`     | INTEGER | 原文字节数                                                  |
| `stored_size`  | INTEGER | 实际落库字节数（压缩后）                                    |
| `content`      | BLOB    | 正文内容                                                    |
| `created_at`   | INTEGER | 首次写入时间，毫秒时间戳                                    |

## chapter_versions

版本时间线，整部署共享，**只插入不更新**。每章的每个不同内容版本一行。

| 字段               | 类型    | 用途                                                                 |
| ------------------ | ------- | -------------------------------------------------------------------- |
| `version_id`       | INTEGER | 主键，AUTOINCREMENT；排序与版本标识                                  |
| `book_id`          | INTEGER | 关联 `books`                                                         |
| `chapter_url`      | TEXT    | 章节 URL（当前章节身份）                                             |
| `content_id`       | INTEGER | 关联 `chapter_contents`                                              |
| `kind`             | TEXT    | `upstream` 上游抓取 / `edited` 用户编辑 / `imported` 手动导入        |
| `book_source_id`   | INTEGER | 抓取时的书源快照                                                     |
| `book_source_name` | TEXT    | 抓取时的书源名快照                                                   |
| `base_version_id`  | INTEGER | 用户版本基于哪一版；上游版本为空                                     |
| `chapter_index`    | INTEGER | 章节序号快照                                                         |
| `chapter_title`    | TEXT    | 章节标题快照                                                         |
| `book_url`         | TEXT    | 抓取/保存时的书籍地址快照，只作数据                                  |
| `fetch_url`        | TEXT    | 实际抓取到的地址（分页章节为首地址）                                 |
| `note`             | TEXT    | 手动导入的备注                                                       |
| `created_at`       | INTEGER | 版本时间，毫秒时间戳；导入可指定历史时间                             |

索引：`(book_id, chapter_url, version_id DESC)`、`(book_id, version_id DESC)`、`content_id`。

## chapter_latest

最新指针，整部署共享，**唯一允许更新的正文表**。默认显示版本按
`chapter_view_state.selected_version_id` > `latest_user_version_id` >
`latest_upstream_version_id` 解析。

| 字段                          | 类型    | 用途                                                   |
| ----------------------------- | ------- | ------------------------------------------------------ |
| `book_id`                     | INTEGER | 主键之一                                               |
| `chapter_url`                 | TEXT    | 主键之一                                               |
| `latest_upstream_version_id`  | INTEGER | 最近一次上游版本                                       |
| `latest_user_version_id`      | INTEGER | 最近一次保存的编辑版，或设为当前的手动导入版本          |
| `last_attempt_at`             | INTEGER | 最近一次回源尝试（成功或失败），用于 24h 节流           |
| `last_success_at`             | INTEGER | 最近一次回源成功                                       |
| `upstream_changed_at`         | INTEGER | 上游内容最近一次变化                                   |
| `edited_at`                   | INTEGER | 最近一次用户版本成为当前的时间                         |
| `last_error`                  | TEXT    | 最近一次回源失败原因（短文案，不带上游 URL）            |

## chapter_view_state

每用户每章选中的版本。主键 `(user_ns, book_id, chapter_url)`。有行表示手动选过；
删除该行表示「跟随最新」。切换版本按用户记录，跨设备一致。

### 手动改章节 URL（换源后接续历史）

第一版不做换源后的自动接续。需要把旧源的正文历史挂到新章节时，停服后手工执行：

```sql
-- 先确认新 URL 没有已有记录；有的话要先合并或删除，否则主键冲突
UPDATE chapter_versions   SET chapter_url = '<new>' WHERE book_id = <id> AND chapter_url = '<old>';
UPDATE chapter_latest     SET chapter_url = '<new>' WHERE book_id = <id> AND chapter_url = '<old>';
UPDATE chapter_view_state SET chapter_url = '<new>'
 WHERE user_ns = '<ns>' AND book_id = <id> AND chapter_url = '<old>';
```

`chapter_versions` 正常只插入不更新，这条语句属于管理员例外。`chapter_latest`
和 `chapter_view_state` 的主键包含 `chapter_url`，新 URL 已有行时必须先决定保留哪一边的
指针/选择。

### 清理旧版本留下的空上游正文（维护窗口手工执行）

旧版本把「文本源返回空正文」也写成了 `kind='upstream'`、`raw_size=0` 的版本。
新版本已经阻止这种写入（文本源空正文按失败处理；只有 `bookSourceType=2` 的图片源允许
空正文，且只记检查时间，不追加版本），但**存量行不会自动删除**。

这些行是抓取失败留下的空壳，不属于有效历史。确认要清理时，先停服并备份 `storage/reader.db`，
再按顺序执行：

```sql
-- 1. 取消指向空上游版本的「当前版本」选择
DELETE FROM chapter_view_state
WHERE selected_version_id IN (
  SELECT v.version_id
  FROM chapter_versions v
  JOIN chapter_contents c ON c.content_id = v.content_id
  WHERE v.kind = 'upstream' AND c.raw_size = 0
);

-- 2. 用户版本若把空上游记成基准，清掉这个悬空引用（只影响展示的元数据）
UPDATE chapter_versions
SET base_version_id = NULL
WHERE base_version_id IN (
  SELECT v.version_id
  FROM chapter_versions v
  JOIN chapter_contents c ON c.content_id = v.content_id
  WHERE v.kind = 'upstream' AND c.raw_size = 0
);

-- 3. 指针改到该章最近一个非空上游版本；没有就置空
UPDATE chapter_latest
SET latest_upstream_version_id = (
  SELECT v.version_id
  FROM chapter_versions v
  JOIN chapter_contents c ON c.content_id = v.content_id
  WHERE v.book_id = chapter_latest.book_id
    AND v.chapter_url = chapter_latest.chapter_url
    AND v.kind = 'upstream'
    AND c.raw_size > 0
  ORDER BY v.version_id DESC
  LIMIT 1
)
WHERE latest_upstream_version_id IN (
  SELECT v.version_id
  FROM chapter_versions v
  JOIN chapter_contents c ON c.content_id = v.content_id
  WHERE v.kind = 'upstream' AND c.raw_size = 0
);

-- 4. 删除空上游版本，再回收没有版本引用的正文
DELETE FROM chapter_versions
WHERE version_id IN (
  SELECT v.version_id
  FROM chapter_versions v
  JOIN chapter_contents c ON c.content_id = v.content_id
  WHERE v.kind = 'upstream' AND c.raw_size = 0
);

DELETE FROM chapter_contents
WHERE content_id NOT IN (SELECT content_id FROM chapter_versions);
```

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

书签/划线。主键 `(user_ns, book_id, chapter_index, chapter_pos, kind)`——同书同位置同类型重复保存覆盖；kind 进主键后章书签与章首划线（pos=0）不再互撞。普通索引 `(user_ns, book_id)`（删书级联用）。

| 字段            | 类型    | 用途                                                                 |
| --------------- | ------- | -------------------------------------------------------------------- |
| `user_ns`       | TEXT    | 数据隔离键                                                           |
| `book_id`       | INTEGER | 关联 books 登记表                                                    |
| `chapter_index` | INTEGER | 章节序号（身份键之一）                                               |
| `chapter_pos`   | INTEGER | 万分比章内进度 0–10000（身份键之一；章书签恒 0，划线取收藏时进度）   |
| `kind`          | TEXT    | 类型（身份键之一）：`bookmark` 章书签 / `underline` 划线             |
| `time`          | INTEGER | 创建/更新时间，毫秒时间戳                                            |
| `book_name`     | TEXT    | 书名快照（展示用，不参与判等）                                       |
| `book_author`   | TEXT    | 作者快照（展示用）                                                   |
| `chapter_name`  | TEXT    | 章节标题快照                                                         |
| `book_text`     | TEXT    | 进度文案/划线摘录快照                                                |
| `content`       | TEXT    | 书签备注/摘录                                                        |
| `book_url`      | TEXT    | 保存时的书籍地址快照，只作数据与备份展示                     |

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
| `book_url`   | TEXT    | 保存时的书籍地址快照，只作数据与备份展示                     |
| `json`       | TEXT    | 整份记忆 JSON                                                              |
| `updated_at` | INTEGER | 更新时间，毫秒时间戳                                                       |

## book_groups

书架分组。主键 `(user_ns, group_id)`。`group_id` 是位域里的 2 的幂（1/2/4/8…，与阅读3.0 的位掩码约定一致；分配取最小未占用幂位，上限 31 个组：前端位运算是 32 位有符号整数；表级 CHECK 兜住非 2 的幂）。

| 字段         | 类型    | 用途               |
| ------------ | ------- | ------------------ |
| `user_ns`    | TEXT    | 数据隔离键         |
| `group_id`   | INTEGER | 位域 id（2 的幂）  |
| `group_name` | TEXT    | 组名               |
| `order_no`   | INTEGER | 排序               |

## 设计取舍

- **书源存整份 JSON** 而不是打散成列——书源字段会随阅读3.0 规范演进，打散后每次演进都要改表；代价是无法按规则字段建索引。
- **章节正文进数据库**——正文池 + 时间线 + 最新指针三张表，正文按 hash 去重、默认 zstd 压缩。这样可以保留上游历史、让编辑版不覆盖旧版本，并让 diff 针对任意两个版本计算。代价是 `reader.db` 会随历史持续增长，因此正文历史**不提供清理入口，也不自动淘汰**；备份需要包含这个库。
- **正文池和时间线只插入不更新**——并发写不需要原地覆盖，旧版本不会因为一次刷新或编辑消失；`chapter_latest` 是唯一允许变化的正文表，`chapter_view_state` 按用户记录当前选择。
- **书籍/书源 id 不分命名空间且不复用**——`book_id` / `book_source_id` 都用 AUTOINCREMENT，删书、换源、清缓存都不会让历史串到另一本书。`book_url` 只作数据字段，不再承担校验。
- **`json_documents` 通用化**——零散键值（配置、userdata）复用一张表，减少迁移次数；有实体语义的（书签、替换规则、分组）拆实体表。
- **不声明外键**——表间一致性由 service 层维护（如删书时级联清书架行、书签、AI 记忆与图片记录）；`foreign_keys` pragma 虽开，但表结构里没有 FK 约束。
- **时间戳单位不统一是历史现状**：`users` / `json_documents` / `ai_book_memories` 用毫秒，其余表用秒；`shelf_books` 里来自前端的字段（`dur_chapter_time`、`last_check_time`）随前端用毫秒。正文池、版本时间线和指针用毫秒。列的用途表里已逐列标注。
