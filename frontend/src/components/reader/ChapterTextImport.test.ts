// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import type { ChapterImportPreview, ChapterImportResult } from '../../api/chapterEdit'

const { previewChapterImport, importChapterText, getChapterImportDiff } = vi.hoisted(() => ({
  previewChapterImport: vi.fn(),
  importChapterText: vi.fn(),
  getChapterImportDiff: vi.fn(),
}))

// 组件只用到这三个运行时导出；类型导入在编译期擦除，不需要出现在 mock 里
vi.mock('../../api/chapterEdit', () => ({
  previewChapterImport,
  importChapterText,
  getChapterImportDiff,
}))

import ChapterTextImport from './ChapterTextImport.vue'

let app: App | null = null
let host: HTMLDivElement | null = null

function mountComponent(extraProps: Record<string, unknown> = {}) {
  host = document.createElement('div')
  document.body.appendChild(host)
  app = createApp(ChapterTextImport, { bookId: 'book_1', ...extraProps })
  app.mount(host)
  return host
}

async function flush() {
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
}

function findButton(root: HTMLElement, text: string): HTMLButtonElement {
  const button = [...root.querySelectorAll('button')].find((item) =>
    item.textContent?.trim().includes(text)
  )
  if (!button) throw new Error(`找不到按钮: ${text}`)
  return button as HTMLButtonElement
}

function makePreview(previewId: string | null = 'pv-1'): ChapterImportPreview {
  return {
    mode: 'book',
    total: 2,
    matched: 2,
    unmatched: 0,
    empty: 0,
    changed: 1,
    cached: 2,
    uncached: 0,
    previewId,
    chapters: [
      {
        sourceIndex: 0,
        title: '第一章 开始',
        chars: 6,
        excerpt: '导入正文一',
        status: 'matched',
        matchKind: 'title',
        chapterIndex: 0,
        chapterUrl: 'local-txt:1',
        chapterTitle: '第一章 开始',
        cached: true,
        changed: true,
      },
      {
        sourceIndex: 1,
        title: '第二章 继续',
        chars: 6,
        excerpt: '导入正文二',
        status: 'matched',
        matchKind: 'title',
        chapterIndex: 1,
        chapterUrl: 'local-txt:2',
        chapterTitle: '第二章 继续',
        cached: true,
        changed: false,
      },
    ],
  }
}

const importResult: ChapterImportResult = {
  mode: 'book',
  selected: 1,
  imported: 1,
  unchanged: 0,
  skipped: 0,
  failed: 0,
  errors: [],
}

const diffResult = {
  changed: true,
  ops: [],
  diffError: '',
  currentExcerpt: '',
  currentChars: 6,
  importChars: 6,
}

async function previewViaPaste(root: HTMLElement, text: string) {
  const textarea = root.querySelector('textarea.text-input') as HTMLTextAreaElement | null
  if (!textarea) throw new Error('找不到粘贴文本框')
  textarea.value = text
  textarea.dispatchEvent(new Event('input'))
  await nextTick()
  findButton(root, '解析并预览').click()
  await flush()
}

beforeEach(() => {
  previewChapterImport.mockReset().mockResolvedValue(makePreview())
  importChapterText.mockReset().mockResolvedValue(importResult)
  getChapterImportDiff.mockReset().mockResolvedValue(diffResult)
})

afterEach(() => {
  app?.unmount()
  app = null
  host?.remove()
  host = null
})

describe('ChapterTextImport 预览复用', () => {
  it('预览成功后导入只传 previewId，不再带文件/文本', async () => {
    const imported = vi.fn()
    const el = mountComponent({ onImported: imported })

    await previewViaPaste(el, '第一章 开始\n导入正文一。\n第二章 继续\n导入正文二。')
    expect(previewChapterImport).toHaveBeenCalledTimes(1)
    // 预览请求仍然带原文
    expect(previewChapterImport.mock.calls[0][0].text).toContain('导入正文一')
    expect(el.querySelector('.preview-area')).toBeTruthy()

    // 默认只勾选「有差异」的第一章
    const importButton = findButton(el, '导入 1 章')
    expect(importButton.disabled).toBe(false)
    importButton.click()
    await flush()

    expect(importChapterText).toHaveBeenCalledTimes(1)
    const [params, selected] = importChapterText.mock.calls[0]
    expect(params.previewId).toBe('pv-1')
    expect(params.file).toBeNull()
    expect(params.text).toBeUndefined()
    expect(params.bookId).toBe('book_1')
    expect(params.mode).toBe('book')
    expect(selected).toEqual([0])
    expect(imported).toHaveBeenCalledTimes(1)
    expect(imported.mock.calls[0][0].result.imported).toBe(1)
  })

  it('重新选择文件后旧预览作废（previewId 随之丢弃），需重新预览', async () => {
    const el = mountComponent()
    await previewViaPaste(el, '第一章 开始\n导入正文一。')
    expect(el.querySelector('.preview-area')).toBeTruthy()

    const fileInput = el.querySelector('input.file-input') as HTMLInputElement
    const next = new File(['第一章 换一本\n新正文。'], 'new.txt', { type: 'text/plain' })
    Object.defineProperty(fileInput, 'files', { value: [next], configurable: true })
    fileInput.dispatchEvent(new Event('change'))
    await flush()

    // 预览面板关闭，导入没有发生过，旧 previewId 不会被拿去导入
    expect(el.querySelector('.preview-area')).toBeNull()
    expect(importChapterText).not.toHaveBeenCalled()

    // 重新预览拿到新令牌后，导入用新令牌
    previewChapterImport.mockResolvedValue(makePreview('pv-2'))
    findButton(el, '解析并预览').click()
    await flush()
    expect(el.querySelector('.preview-area')).toBeTruthy()
    findButton(el, '导入 1 章').click()
    await flush()
    expect(importChapterText).toHaveBeenCalledTimes(1)
    expect(importChapterText.mock.calls[0][0].previewId).toBe('pv-2')
  })

  it('切换匹配模式丢弃当前预览', async () => {
    const el = mountComponent()
    await previewViaPaste(el, '第一章 开始\n导入正文一。')
    expect(el.querySelector('.preview-area')).toBeTruthy()

    findButton(el, '当前章节').click()
    await nextTick()
    expect(el.querySelector('.preview-area')).toBeNull()
    expect(importChapterText).not.toHaveBeenCalled()
  })

  it('预览过期被后端拒绝时作废 previewId，重试回退到重新上传正文', async () => {
    importChapterText.mockRejectedValueOnce(new Error('预览已过期或不存在，请重新解析后再导入'))
    const el = mountComponent()
    await previewViaPaste(el, '第一章 开始\n导入正文一。\n第二章 继续\n导入正文二。')

    findButton(el, '导入 1 章').click()
    await flush()

    // 第一次导入用 previewId，被拒后错误文案展示出来、预览面板保留（勾选不丢）
    expect(importChapterText).toHaveBeenCalledTimes(1)
    expect(importChapterText.mock.calls[0][0].previewId).toBe('pv-1')
    expect(el.querySelector('.preview-area')).toBeTruthy()
    expect(el.querySelector('.error-text')?.textContent).toContain('请重新解析')

    // 重试不再带 previewId，回退到上传当前文本
    importChapterText.mockResolvedValue(importResult)
    findButton(el, '导入 1 章').click()
    await flush()
    expect(importChapterText).toHaveBeenCalledTimes(2)
    const retry = importChapterText.mock.calls[1][0]
    expect(retry.previewId).toBeUndefined()
    expect(retry.text).toContain('导入正文一')
  })
})
