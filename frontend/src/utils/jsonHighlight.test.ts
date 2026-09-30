import { describe, it, expect } from 'vitest'
import { highlightJsonToHtml } from './jsonHighlight'

// 注意：产出的 HTML 里引号被转义为 &quot;

describe('highlightJsonToHtml', () => {
  it('键名与字符串值分开着色', () => {
    const html = highlightJsonToHtml('{"name": "值"}')
    expect(html).toContain('<span class="tok-key">&quot;name&quot;</span>')
    expect(html).toContain('<span class="tok-str">&quot;值&quot;</span>')
  })

  it('冒号前允许空白隔着', () => {
    const html = highlightJsonToHtml('{"a" : 1}')
    expect(html).toContain('<span class="tok-key">&quot;a&quot;</span>')
  })

  it('数字、布尔、null 着色', () => {
    const html = highlightJsonToHtml('{"n": -12.5e2, "b": true, "x": null}')
    expect(html).toContain('<span class="tok-num">-12.5e2</span>')
    expect(html).toContain('<span class="tok-kw">true</span>')
    expect(html).toContain('<span class="tok-kw">null</span>')
  })

  it('truex 这种标识符不会被误判成关键字', () => {
    const html = highlightJsonToHtml('{"a": "truex"}')
    expect(html).not.toContain('tok-kw')
  })

  it('字符串内的 {{...}} 与 {key} 占位符单独标色', () => {
    const html = highlightJsonToHtml('{"url": "/s/{{page+1}}?k={key}"}')
    expect(html).toContain('<span class="tok-js">{{page+1}}</span>')
    expect(html).toContain('<span class="tok-var">{key}</span>')
  })

  it('HTML 特殊字符被转义，不产生注入', () => {
    const html = highlightJsonToHtml('{"a": "<script>"}')
    expect(html).not.toContain('<script>')
    expect(html).toContain('&lt;script&gt;')
  })

  it('未闭合字符串吃到结尾且不抛错', () => {
    const html = highlightJsonToHtml('{"a": "abc')
    expect(html).toContain('<span class="tok-str">&quot;abc</span>')
  })

  it('结尾是换行时补一个空格对齐高度', () => {
    expect(highlightJsonToHtml('{\n}').endsWith(' ')).toBe(false)
    expect(highlightJsonToHtml('{\n').endsWith(' ')).toBe(true)
  })

  it('转义引号不截断字符串', () => {
    const html = highlightJsonToHtml('{"a": "x\\"y", "b": 1}')
    expect(html).toContain('<span class="tok-str">&quot;x\\&quot;y&quot;</span>')
    expect(html).toContain('<span class="tok-key">&quot;b&quot;</span>')
  })
})
