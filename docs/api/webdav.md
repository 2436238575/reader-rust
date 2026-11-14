# WebDAV API

WebDAV 远端备份/同步接口。数据存放在 `storage/webdav/<用户命名空间>/` 目录，命名空间为 `user_<uid>`。

两套命名空间并存：`/v1` 是推荐接口（裸 JSON + problem+json 错误），`/reader3` 保留兼容（统一 `{ isSuccess, data, errorMsg }` 包装）。两者读写同一个用户家目录，可以混用。

## v1 接口（推荐）

会话鉴权与 `/v1` 其他接口一致：浏览器由 HttpOnly Cookie `reader_access` 自动携带，API 客户端用 `Authorization: Bearer <jwt>`；非 GET 请求需要 `X-Requested-With: XMLHttpRequest`（携带 Bearer 头的客户端豁免该 CSRF 检查）。HTTP **Basic 认证只用于协议入口** `/v1/webdav`。

### 文件浏览

```text
GET    /v1/me/files?path=/                      # 目录列表，缺省根目录
POST   /v1/me/files?path=/backups               # multipart 上传（file 字段可重复）
GET    /v1/me/files/{path}                      # 下载（流式，路径可含 /）
DELETE /v1/me/files/{path}                      # 递归删除
POST   /v1/me/files:batchDelete                 # { "paths": ["/a", "/b"] }
```

- 列表返回 `{ name, path, size, isDirectory, modifiedAt }` 数组；`modifiedAt` 是 RFC 3339 UTC。隐藏文件（`.` 开头）不列出。响应带强 `ETag`，`If-None-Match` 命中回 304。
- 上传单文件上限 100MB、单请求字段上限 20 个，边收边写同目录临时文件，成功后 rename；错误与取消路径都会清理临时文件。响应 201 + 已写入条目数组。
- 下载按扩展名给显式 `Content-Type`，`.html/.xml/.svg` 额外带 `Content-Security-Policy: sandbox`；支持元数据 `ETag` 与 304。
- 路径统一按 `/`、`\` 双分隔符切分，拒绝 `..`、盘符/冒号、结尾点/空格与 Windows 设备名。
- 批删先校验全部路径再执行；不存在的计入 `missing`，单次最多 1000 条（前端按 500 分片）。

### 备份与恢复

```text
POST /v1/me/backups                  # body { "localState": { ... } } → 201 { name, path, size, ... }
POST /v1/me/backups:inspect          # { "path": "/backups/reader-backup-....json" } → 条目数摘要
POST /v1/me/backups:restore          # { "path": "/backups/reader-backup-....json" }
```

- `:inspect` 不写库，返回 `{version, createdAt, books, groups, bookSources, bookmarks, replaceRules, hasLocalState}`；前端恢复确认框据此展示条目数，空书源（会清空全部书源）在点确定前就能看到。
- 备份文件写入 `/backups/reader-backup-<UTC 时间>-<随机后缀>.json`，内容包含书架（含顺序）、分组、书源、书签、替换规则。`bookshelf.books/groups`、`bookSources`、`bookmarks`、`replaceRules` 都是必填数组：缺任何一个返回 422，不会按空数组清空数据。
- `localState` 是浏览器 localStorage 白名单快照，由前端在请求体里提交（服务端不接触浏览器存储）；字段可缺省，缺省表示这份备份不含本地配置。恢复结果原样返回 `localState`：`null` 表示备份没有该字段，前端不应改动当前 localStorage；显式空对象才按整份覆盖。
- 恢复是**以备份为准的删除 + 重建**：书源清空重导、分组整表覆盖、书架按备份重建（只清当前用户书架，登记行与正文历史保留）、书签与替换规则整表重建。恢复结果返回各域处理条数、`skippedBookmarks`（旧备份里指向书架外且没有 `bookUrl` 的书签）与 `localState`。
- 新版备份的书签会为**书架外**的条目补 `bookUrl`，恢复时按 URL 找回登记行，因此「已移出书架但仍有书签」的数据不会丢；旧备份没有该字段，只能按 bookId 映射，书架外条目计入 `skippedBookmarks`。
- `bookId` 以恢复后的实际身份为准：同 URL 既有登记行会复用原 id，备份 id 被别的 URL 占用时会重新分配；书签与替换规则的 `book:` / `source:` scope 会按映射重写，避免书签静默丢失。
- 非法分组 id（如旧版遗留的 3）按旧前端同口径丢弃，书籍分组位掩码里的对应位一并剥掉。备份版本只接受 `version=1`。
- **五个域在同一个 SQLite 事务里**：任何一步失败（DB 错误、磁盘满、请求取消）整体回滚，不会出现「书源已换、书架已清」的半恢复状态。文档级非法（版本、字段缺失、书籍 URL/数量/ID、书签量程、JSON 格式）在开启事务前拒绝。同一进程只允许一个恢复在执行，并发调用返回 409。
- 备份文件超过 100MB 返回 413；路径不存在 404；JSON 非法/字段不完整 422。

## `/reader3` 兼容接口

以下接口与其他 `/reader3/*` 接口一致，使用会话鉴权：浏览器由 HttpOnly Cookie `reader_access` 自动携带，API 客户端用 `Authorization: Bearer <jwt>`；`?accessToken=` 查询参数只保留给书源登录代理路径，这里不再接受。HTTP **Basic 认证只用于下面的协议入口**。

### 获取文件列表

```text
GET /reader3/getWebdavFileList?path=/
```

`path` 缺省为 `/`。响应 `data` 为条目数组：`{ name, size, path, lastModified, isDirectory }`。隐藏文件（`.` 开头）不列出。

### 下载文件

```text
GET /reader3/getWebdavFile?path=/a.txt
```

响应为文件内容（流式，非 JSON 包装）。

### 上传文件

```text
POST /reader3/uploadFileToWebdav
```

`multipart/form-data`：`path` 字段为目标目录（缺省 `/`），`file` 字段为文件。单文件上限 100MB，服务端边接收边流式落盘；单请求 multipart 字段数上限 20 个（超限 400）。响应 `data` 为已写入条目数组。上传先写目标同目录的 `.upload-*.tmp` 临时文件、完成后再改名；任何失败**或中途取消**（客户端断连、上传超时 `INBOUND_UPLOAD_TIMEOUT_SECS` 掐断）都会清理临时文件，不在目录里留孤儿。目标与现有**目录**同名时直接报错（文件不能覆盖目录，目录原样保留）。同名覆盖文件时先直接改名（POSIX 上原子覆盖）；Windows 上改名失败（目标被占用等）会先把旧文件改名成同目录的 `.reader-upload-backup-*.tmp` 备份再重试，**不先删除旧文件**——重试失败则把备份改回原名恢复旧文件，恢复也失败的极端情况下备份保留、错误信息带备份路径。服务端另有清扫机制兜底：启动时、每 6 小时以及访问目录时清理超过 1 小时的上传临时文件；备份超过 1 小时且目标存在则删除、目标缺失则恢复成目标文件，无法从文件名解析原目标的旧格式备份保留 7 天后才删除。

### 删除文件

```text
POST /reader3/deleteWebdavFile
POST /reader3/deleteWebdavFileList
```

请求体分别为 `{ "path": "/a.txt" }` 与 `{ "path": ["/a.txt", "/b.txt"] }`。目录递归删除。响应 `data` 为空字符串。

## WebDAV 协议接口

```text
ANY /v1/webdav             （推荐；裸根与 / 尾斜杠形式等价）
ANY /v1/webdav/*path
ANY /reader3/webdav        （兼容）
ANY /reader3/webdav/*path
```

这些入口共用同一份用户家目录与路径加固，区别只有 dav-server 的 strip 前缀。

协议由 dav-server 实现（litmus 一致性套件验证）。走 HTTP **Basic 认证**（用户名 + 账号密码），支持 `PROPFIND` / `MKCOL` / `PUT` / `GET` / `HEAD` / `DELETE` / `MOVE` / `COPY` / `LOCK` / `UNLOCK`，供第三方 WebDAV 客户端挂载。认证失败返回 401；限速命中返回 429 + `Retry-After`。与登录接口共享同一份失败限速（IP 维度失败 5 次、用户名维度失败 10 次后封禁 6 小时）。

- `PROPFIND` 支持 `Depth: 0/1/infinity`，条目带 `ETag` 与标准 HTTP-date 时间；隐藏文件（`.` 开头）会出现在结果里（JSON 列表接口仍过滤）。
- `PUT` 流式落盘，单文件上限 100MB；支持 `If-None-Match` 等条件请求与 `ETag`。
- `MOVE` / `COPY` 的 `Destination` 头接受绝对 URI 或绝对路径两种形式。
- `LOCK` 是真实内存锁：锁住的资源无锁令牌写入返回 423；锁状态存于内存，进程重启即失效。
- `OPTIONS` 由全局 CORS 层直接应答（200，不携带 DAV 能力头）。

已知平台特例：`MKCOL` 已存在目录按 RFC 4918 应返回 405，dav-server 0.11 在 Windows 上的错误码映射缺陷会返回 500（Linux 部署正常）。

## 路径安全约束

所有接口的相对路径统一按 `/`、`\` 双分隔符切分，且拒绝：`..` 回溯、盘符/冒号（含 NTFS ADS）、结尾点/空格、Windows 保留设备名（`CON`、`NUL`、`COM1` 等）。multipart 上传的文件名只取纯文件名部分，含路径分隔符或保留字符一律 400。协议接口另有 dav-server 自身的路径规范化处理（拒绝 `..` 越级与段内编码斜杠 `%2f`）。
