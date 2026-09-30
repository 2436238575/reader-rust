# WebDAV API

WebDAV 远端备份/同步接口。所有 JSON 接口响应包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

使用前提：已登录。数据存放在 `storage/webdav/<用户名>/` 目录。

## JSON 接口

这几个接口走 `Authorization: 用户名:token` 鉴权。

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

`multipart/form-data`：`path` 字段为目标目录（缺省 `/`），`file` 字段为文件。单文件上限 100MB。响应 `data` 为已写入条目数组。

### 删除文件

```text
POST /reader3/deleteWebdavFile
POST /reader3/deleteWebdavFileList
```

请求体分别为 `{ "path": "/a.txt" }` 与 `{ "path": ["/a.txt", "/b.txt"] }`。目录递归删除。响应 `data` 为空字符串。

## WebDAV 协议接口

```text
ANY /reader3/webdav        （裸根与 / 尾斜杠形式等价）
ANY /reader3/webdav/*path
```

协议由 dav-server 实现（litmus 一致性套件验证）。走 HTTP **Basic 认证**（用户名 + 账号密码），支持 `PROPFIND` / `MKCOL` / `PUT` / `GET` / `HEAD` / `DELETE` / `MOVE` / `COPY` / `LOCK` / `UNLOCK`，供第三方 WebDAV 客户端挂载。认证失败返回 401；与登录接口共享失败限速（10 分钟窗口内失败 8 次锁定 5 分钟）。

- `PROPFIND` 支持 `Depth: 0/1/infinity`，条目带 `ETag` 与标准 HTTP-date 时间；隐藏文件（`.` 开头）会出现在结果里（JSON 列表接口仍过滤）。
- `PUT` 流式落盘，单文件上限 100MB；支持 `If-None-Match` 等条件请求与 `ETag`。
- `MOVE` / `COPY` 的 `Destination` 头接受绝对 URI 或绝对路径两种形式。
- `LOCK` 是真实内存锁：锁住的资源无锁令牌写入返回 423；锁状态存于内存，进程重启即失效。
- `OPTIONS` 由全局 CORS 层直接应答（200，不携带 DAV 能力头）。

已知平台特例：`MKCOL` 已存在目录按 RFC 4918 应返回 405，dav-server 0.11 在 Windows 上的错误码映射缺陷会返回 500（Linux 部署正常）。

## 路径安全约束

所有接口的相对路径统一按 `/`、`\` 双分隔符切分，且拒绝：`..` 回溯、盘符/冒号（含 NTFS ADS）、结尾点/空格、Windows 保留设备名（`CON`、`NUL`、`COM1` 等）。multipart 上传的文件名只取纯文件名部分，含路径分隔符或保留字符一律 400。协议接口另有 dav-server 自身的路径规范化兜底（拒绝 `..` 越级与段内编码斜杠 `%2f`）。
