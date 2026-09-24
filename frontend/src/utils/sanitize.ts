import DOMPurify from 'dompurify'

// 书源正文与 RSS 文章是完全不可信的第三方 HTML。白名单保留排版相关标签
// （段落/标题/表格/注音/图片/基础内联样式），剥掉脚本、事件属性、iframe
// 等可执行内容；href/src 的 javascript: 协议由 DOMPurify 默认移除，
// img 的 data: URI 默认放行（书源内嵌图常用）。
const ALLOWED_TAGS = [
  'p', 'br', 'hr', 'img', 'ruby', 'rt', 'rp',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
  'b', 'strong', 'i', 'em', 'u', 's', 'del', 'ins', 'mark', 'small', 'sub', 'sup',
  'span', 'font', 'blockquote', 'pre', 'code',
  'ul', 'ol', 'li', 'dl', 'dt', 'dd',
  'table', 'thead', 'tbody', 'tfoot', 'tr', 'td', 'th', 'caption', 'colgroup', 'col',
  'a', 'div', 'section', 'article', 'aside', 'figure', 'figcaption',
  'center', 'details', 'summary',
]

const ALLOWED_ATTR = [
  'src', 'alt', 'title', 'width', 'height', 'style', 'class', 'id', 'dir', 'lang',
  'href', 'colspan', 'rowspan', 'align', 'valign', 'border', 'cellpadding', 'cellspacing',
  'color', 'face', 'size', 'start', 'type', 'open',
]

/**
 * 清洗不可信的第三方 HTML（书源正文、RSS 文章）。
 *
 * 返回值可以安全地交给 `v-html`：脚本与事件属性已剥除，标签集合收敛到
 * 排版所需的最小集。校验用例见 `sanitize.test.ts`。
 */
export function sanitizeUntrustedHtml(html: string): string {
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    ALLOW_DATA_ATTR: false,
  })
}
