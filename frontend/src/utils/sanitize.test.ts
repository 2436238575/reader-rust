// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { sanitizeUntrustedHtml } from './sanitize'

describe('sanitizeUntrustedHtml', () => {
  it('strips script tags and their content', () => {
    const out = sanitizeUntrustedHtml('<p>正文</p><script>alert(1)</script>')
    expect(out).not.toContain('script')
    expect(out).not.toContain('alert')
    expect(out).toContain('正文')
  })

  it('strips event handler attributes', () => {
    const out = sanitizeUntrustedHtml('<p onclick="alert(1)" onerror="boom()">t</p>')
    expect(out).not.toContain('onclick')
    expect(out).not.toContain('onerror')
  })

  it('drops iframe/object/embed entirely', () => {
    const out = sanitizeUntrustedHtml(
      '<iframe src="https://evil.example"></iframe><object data="x"></object><embed src="y">'
    )
    expect(out).not.toContain('iframe')
    expect(out).not.toContain('<object')
    expect(out).not.toContain('embed')
  })

  it('removes javascript: URLs from links and images', () => {
    const out = sanitizeUntrustedHtml(
      '<a href="javascript:alert(1)">x</a><img src="javascript:alert(2)">'
    )
    expect(out).not.toContain('javascript:')
  })

  it('keeps formatting tags, images and inline styles', () => {
    const html =
      '<p style="text-indent:2em">段<ruby>汉<rt>han</rt></ruby></p>' +
      '<table><tr><td colspan="2">c</td></tr></table>' +
      '<img src="https://x/1.jpg" alt="a">'
    const out = sanitizeUntrustedHtml(html)
    expect(out).toContain('<p')
    expect(out).toContain('<ruby>')
    expect(out).toContain('<table')
    expect(out).toContain('<img src="https://x/1.jpg"')
    expect(out).toContain('text-indent')
  })

  it('keeps data: image URIs used by book sources', () => {
    const out = sanitizeUntrustedHtml('<img src="data:image/png;base64,AAAA">')
    expect(out).toContain('data:image/png')
  })

  it('keeps chapter illustration markup (figure + size hints)', () => {
    const out = sanitizeUntrustedHtml(
      '<figure class="chapter-figure"><img src="https://x/2.jpg" width="1400" height="933">' +
        '<figcaption>配图（画师：奈月Oo）</figcaption></figure>'
    )
    expect(out).toContain('<figure')
    expect(out).toContain('<figcaption>')
    expect(out).toContain('width="1400"')
    expect(out).toContain('height="933"')
  })

  it('keeps plain text untouched', () => {
    expect(sanitizeUntrustedHtml('纯文本正文')).toBe('纯文本正文')
  })

  it('strips overlay-capable CSS from style attributes', () => {
    const out = sanitizeUntrustedHtml(
      '<div style="position:fixed;inset:0;z-index:9999;background:#fff;color:#000">钓鱼</div>'
    )
    expect(out).not.toContain('position')
    expect(out).not.toContain('z-index')
    // 排版类声明保留
    expect(out).toContain('background')
    expect(out).toContain('color')
    expect(out).toContain('钓鱼')
  })

  it('strips remote-loading CSS from style attributes', () => {
    const out = sanitizeUntrustedHtml(
      '<p style="background-image:url(https://evil.example/t.png);text-indent:2em">t</p>'
    )
    expect(out).not.toContain('url(')
    expect(out).toContain('text-indent')
  })

  it('strips absolute/sticky positioning too', () => {
    expect(sanitizeUntrustedHtml('<p style="position:absolute;top:0">t</p>')).not.toContain(
      'position'
    )
    expect(sanitizeUntrustedHtml('<p style="POSITION:sticky">t</p>')).not.toContain('position')
  })
})
