# 书籍分组 API

书架分组的增删改查与书籍分组归属调整接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

分组按用户命名空间隔离存储；给书籍设置分组时，目标书籍必须已在**当前用户**的书架中。

## 分组对象（BookGroup）

| 字段        | 类型   | 说明    |
| ----------- | ------ | ------- |
| `groupId`   | number | 分组 ID |
| `groupName` | string | 分组名  |
| `orderNo`   | number | 排序号  |

## 获取分组列表

```text
GET /reader3/getBookGroups
```

响应 `data` 为 `BookGroup` 数组。

## 保存分组

```text
POST /reader3/saveBookGroup
```

请求体为单个 `BookGroup` 对象（`groupId` 为 0 或已存在时按 ID 覆盖）。响应 `data` 为 `"success"`。

## 删除分组

```text
POST /reader3/deleteBookGroup
```

请求体：`{ "groupId": 0 }`。响应 `data` 为 `"success"`。

## 批量保存分组顺序

```text
POST /reader3/saveBookGroupOrder
```

请求体为 `BookGroup` 数组，整体替换分组列表（用于拖拽排序后保存）。响应 `data` 为 `"success"`。

## 设置书籍的分组

```text
POST /reader3/saveBookGroupId
```

请求体：

| 参数      | 类型   | 必填 | 说明                            |
| --------- | ------ | ---- | ------------------------------- |
| `bookUrl` | string | 是   | 书籍 URL                        |
| `groupId` | number | 否   | 目标分组 ID，缺省为 0（未分组） |

书籍不在书架时返回 404。响应 `data` 为 `"success"`。

## 批量加入分组

```text
POST /reader3/addBookGroupMulti
```

请求体：`{ "bookUrls": ["..."], "groupId": 0 }`。按位或并入分组（`group` 是位掩码字段，与阅读3.0 一致）；不在书架的书籍静默跳过。响应 `data` 为 `"success"`。

## 批量移出分组

```text
POST /reader3/removeBookGroupMulti
```

请求体：`{ "bookUrls": ["..."], "groupId": 0 }`。按位清除（`group & ~groupId`）。响应 `data` 为 `"success"`。
