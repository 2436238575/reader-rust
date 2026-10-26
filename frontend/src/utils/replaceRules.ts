// 替换规则的 scope 匹配与按序应用：从 reader store 拆出的纯函数（reader store 拆分专项）。
// store 里剩一层薄壳：把当前的 rules 与 book 传进来，结果喂给简繁转换。

import type { Book, ReplaceRule } from '../types'

/**
 * scope 形态：
 * - 空 / `*`：全局生效
 * - `source:<bookSourceId>`：指定书源
 * - `book:<bookId>`：指定书籍
 * - `<书名>` 或 `<书名>;<bookId>`：旧版按书名（可再限书号）的兼容写法
 */
export function matchReplaceRuleScope(rule: ReplaceRule, currentBook: Book | null): boolean {
  const scope = (rule.scope || '').trim()
  if (!scope || scope === '*') return true
  if (!currentBook) return false

  if (scope.startsWith('source:')) {
    return scope.slice('source:'.length) === String(currentBook.bookSourceId ?? '')
  }

  if (scope.startsWith('book:')) {
    return scope.slice('book:'.length) === (currentBook.bookId ?? '')
  }

  const scopeParts = scope.split(';')
  if (scopeParts[0] !== '*' && scopeParts[0] !== currentBook.name) {
    return false
  }
  return scopeParts.length === 1 || scopeParts[1] === (currentBook.bookId ?? '')
}

/** 按 order 升序应用启用且 scope 命中的规则；单条规则坏（如非法正则）不影响其余规则 */
export function applyReplaceRulesToText(
  text: string,
  rules: ReplaceRule[],
  currentBook: Book | null
): string {
  if (!text) return ''
  let result = text

  const enabledRules = [...rules]
    .filter((r) => r.isEnabled && matchReplaceRuleScope(r, currentBook))
    .sort((a, b) => a.order - b.order)

  for (const rule of enabledRules) {
    try {
      if (rule.isRegex) {
        const re = new RegExp(rule.pattern, 'gm')
        result = result.replace(re, rule.replacement)
      } else {
        result = result.replaceAll(rule.pattern, rule.replacement)
      }
    } catch (e) {
      console.error(`Failed to apply rule: ${rule.name}`, e)
    }
  }
  return result
}
