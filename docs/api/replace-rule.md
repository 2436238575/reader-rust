# 替换规则 API

替换规则（正文净化规则）的增删改查接口。所有响应均包裹为统一结构：`{ "isSuccess": boolean, "data": any, "errorMsg": string }`。

规则以 `name` 为唯一键，按用户命名空间隔离存储；不存在全局共享规则。

## 规则对象（ReplaceRule）

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | number | 规则 ID |
| `name` | string | 规则名（唯一键） |
| `group` | string? | 分组名 |
| `pattern` | string | 匹配内容（普通文本或正则） |
| `replacement` | string | 替换为 |
| `scope` | string? | 作用范围 |
| `isEnabled` | boolean | 是否启用 |
| `isRegex` | boolean | 是否正则 |
| `order` | number | 执行顺序 |

## 获取替换规则列表

```text
GET /reader3/getReplaceRules
```

响应 `data` 为 `ReplaceRule` 数组。

## 保存替换规则

```text
POST /reader3/saveReplaceRule
```

请求体为单个 `ReplaceRule` 对象。`name` 或 `pattern` 为空返回 400；同名覆盖。响应 `data` 为空字符串。

## 批量保存替换规则

```text
POST /reader3/saveReplaceRules
```

请求体为 `ReplaceRule` 数组；`name` 或 `pattern` 为空的条目静默忽略。响应 `data` 为空字符串。

## 删除替换规则

```text
POST /reader3/deleteReplaceRule
```

请求体为单个 `ReplaceRule` 对象，按 `name` 匹配删除。响应 `data` 为空字符串。

## 批量删除替换规则

```text
POST /reader3/deleteReplaceRules
```

请求体为 `ReplaceRule` 数组，按 `name` 匹配删除。响应 `data` 为空字符串。
