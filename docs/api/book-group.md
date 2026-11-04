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

请求体为单个 `BookGroup` 对象。`groupId` 为 0 时服务端分配最小未占用的幂位；显式指定（WebDAV 恢复对齐备份）必须是 2 的幂且不超过 2^30——`group` 是位掩码，非法 id（如 3 = 0b11）会让书籍同时落进多个组，服务端按 400 拒绝。分组上限 31 个（1<<0 ..= 1<<30，前端位运算是 32 位有符号整数）。响应 `data` 为 `"success"`。

## 删除分组

```text
POST /reader3/deleteBookGroup
```

请求体：`{ "groupId": 0 }`。删除会同步清掉所有书籍位掩码里该组的位（空出的幂位之后可安全分配给新组）。响应 `data` 为 `"success"`。

## 批量保存分组顺序

```text
POST /reader3/saveBookGroupOrder
```

请求体为**排序项数组**（只描述顺序，不含分组名）：

```json
[
  { "groupId": 1, "orderNo": 0 },
  { "groupId": 4, "orderNo": 1 }
]
```

语义是**只改顺序**：

- 只写 `book_groups.order_no`，**不删组、不改名、不动书籍位掩码**；
- **未提交的分组保持原 `orderNo`**，不会被删除（旧版整表覆盖会删组并清掉书上的位，已废弃）；
- `groupId` 必须是合法的 2 的幂（1, 2, 4 … 2^30）且组必须存在；
- 空数组、重复 `groupId`、非法幂位、组不存在一律 **400**；
- 全部校验通过后在单事务内写入，不会出现「前几条生效、后面报错」的半截状态。

分组的增删改入口只有 `POST /reader3/saveBookGroup`（新建/改名）与
`POST /reader3/deleteBookGroup`（删除），排序接口不承担这些职责。

响应 `data` 为 `"success"`。

> **WebDAV 恢复**用的是整表覆盖语义（不在备份里的组应当被删除），走服务层
> `save_groups`，与本接口无关。

## 设置书籍的分组

```text
POST /reader3/saveBookGroupId
```

请求体：

| 参数      | 类型   | 必填 | 说明                            |
| --------- | ------ | ---- | ------------------------------- |
| `bookId`  | string | 是   | 书籍 ID                         |
| `groupId` | number | 否   | 目标分组 ID，缺省为 0（未分组） |

书籍不在书架时返回 404。响应 `data` 为 `"success"`。

## 批量加入分组

```text
POST /reader3/addBookGroupMulti
```

请求体：`{ "bookIds": ["book_12", "book_13"], "groupId": 0 }`。按位或并入分组（`group` 是位掩码字段，与阅读3.0 一致）；不在书架的书籍静默跳过。响应 `data` 为 `"success"`。

## 批量移出分组

```text
POST /reader3/removeBookGroupMulti
```

请求体：`{ "bookIds": ["book_12", "book_13"], "groupId": 0 }`。按位清除（`group & ~groupId`）。响应 `data` 为 `"success"`。
