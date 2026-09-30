// JSON 语法高亮（书源口味）：产出带 span 标记的 HTML，供编辑器的
// 高亮层与透明 textarea 叠加渲染。除标准 JSON token 外，字符串内的
// {{...}} 内联 JS 与 {key}/{page} 等占位符会单独标色。
//
// 纯函数、无 DOM 依赖，单测见 jsonHighlight.test.ts。

const PLACEHOLDER_RE = /(\{\{[\s\S]*?\}\}|\{(?:key|page|searchKey|searchPage)\})/g

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

/** 字符串内容（含引号）渲染：整体标为 key/str，内部占位符单独标色 */
function renderStringToken(raw: string, cls: 'tok-key' | 'tok-str'): string {
  const parts = raw.split(PLACEHOLDER_RE)
  let out = `<span class="${cls}">`
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i]
    if (!part) continue
    if (i % 2 === 1) {
      // 命中的占位符：{{...}} 与 {...} 分两种色
      const inner = part.startsWith('{{') ? 'tok-js' : 'tok-var'
      out += `<span class="${inner}">${escapeHtml(part)}</span>`
    } else {
      out += escapeHtml(part)
    }
  }
  return out + '</span>'
}

function isIdentChar(ch: string | undefined): boolean {
  return !!ch && /[A-Za-z0-9_$]/.test(ch)
}

export function highlightJsonToHtml(src: string): string {
  let out = ''
  let i = 0
  const n = src.length

  while (i < n) {
    const ch = src[i]

    if (ch === '"') {
      // 扫描字符串（处理 \" 转义；未闭合则吃到结尾）
      let j = i + 1
      while (j < n) {
        if (src[j] === '\\') j += 2
        else if (src[j] === '"') {
          j++
          break
        } else j++
      }
      const raw = src.slice(i, j)
      // 后面跟冒号（跳过空白）的是键名
      let k = j
      while (k < n && (src[k] === ' ' || src[k] === '\t' || src[k] === '\n' || src[k] === '\r')) k++
      out += renderStringToken(raw, src[k] === ':' ? 'tok-key' : 'tok-str')
      i = j
      continue
    }

    const num = /^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(src.slice(i))
    if (num && (ch === '-' || (ch >= '0' && ch <= '9'))) {
      out += `<span class="tok-num">${escapeHtml(num[0])}</span>`
      i += num[0].length
      continue
    }

    const kw = /^(true|false|null)/.exec(src.slice(i))
    if (kw && !isIdentChar(src[i + kw[0].length])) {
      out += `<span class="tok-kw">${kw[0]}</span>`
      i += kw[0].length
      continue
    }

    out += escapeHtml(ch)
    i++
  }

  // pre 对结尾换行的渲染与 textarea 不一致，补一个空格对齐高度
  if (src.endsWith('\n')) out += ' '
  return out
}
