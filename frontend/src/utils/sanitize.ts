import DOMPurify from 'dompurify'

// 书源正文是完全不可信的第三方 HTML。白名单保留排版相关标签
// （段落/标题/表格/注音/图片/基础内联样式），剥掉脚本、事件属性、iframe
// 等可执行内容；href/src 的 javascript: 协议由 DOMPurify 默认移除，
// img 的 data: URI 默认放行（书源内嵌图常用）。
const ALLOWED_TAGS = [
  'p',
  'br',
  'hr',
  'img',
  'ruby',
  'rt',
  'rp',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'b',
  'strong',
  'i',
  'em',
  'u',
  's',
  'del',
  'ins',
  'mark',
  'small',
  'sub',
  'sup',
  'span',
  'font',
  'blockquote',
  'pre',
  'code',
  'ul',
  'ol',
  'li',
  'dl',
  'dt',
  'dd',
  'table',
  'thead',
  'tbody',
  'tfoot',
  'tr',
  'td',
  'th',
  'caption',
  'colgroup',
  'col',
  'a',
  'div',
  'section',
  'article',
  'aside',
  'figure',
  'figcaption',
  'center',
  'details',
  'summary',
]

const ALLOWED_ATTR = [
  'src',
  'alt',
  'title',
  'width',
  'height',
  'style',
  'class',
  'id',
  'dir',
  'lang',
  'href',
  'colspan',
  'rowspan',
  'align',
  'valign',
  'border',
  'cellpadding',
  'cellspacing',
  'color',
  'face',
  'size',
  'start',
  'type',
  'open',
]

/**
 * style 属性里的危险 CSS 声明过滤。书源正文可以注入
 * `style="position:fixed;inset:0;z-index:9999;..."` 覆盖全屏伪造界面（钓鱼引导），
 * 或经 `url(` 发起远程请求（追踪/加载恶意资源）。排版所需的字体/缩进/颜色不受影响。
 */
function isDangerousCssDecl(decl: string): boolean {
  const lower = decl.trim().toLowerCase()
  if (!lower) return false
  const prop = lower.split(':', 1)[0].trim()
  // position（任意值都能脱离文档流做覆盖层）、z-index 压层、IE 遗物 behavior/binding
  if (['position', 'z-index', 'behavior', 'binding', '-moz-binding'].includes(prop)) {
    return true
  }
  // 任何属性值里的 url(/expression(：远程加载与脚本执行通道
  return /(?:url|expression)\s*\(/.test(lower)
}

// hook 是全局的（DOMPurify 实例级 API 不存在），模块加载时安装一次；
// 全前端只有这里用 DOMPurify，无串扰面
DOMPurify.addHook('uponSanitizeAttribute', (_node, data) => {
  if (data.attrName !== 'style' || !data.attrValue) return
  const cleaned = data.attrValue
    .split(';')
    .filter((decl) => !isDangerousCssDecl(decl))
    .join(';')
  if (cleaned !== data.attrValue) {
    data.attrValue = cleaned
  }
})

/**
 * 清洗不可信的第三方 HTML（书源正文）。
 *
 * 返回值可以安全地交给 `v-html`：脚本与事件属性已剥除，标签集合收敛到
 * 排版所需的最小集，style 里的覆盖层/远程加载类声明也被剥除。
 * 校验用例见 `sanitize.test.ts`。
 */
export function sanitizeUntrustedHtml(html: string): string {
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    ALLOW_DATA_ATTR: false,
  })
}
