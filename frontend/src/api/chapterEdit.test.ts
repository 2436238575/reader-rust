// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import { importChapterText, previewChapterImport } from './chapterEdit'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
  },
}))

const postMock = http.post as unknown as Mock

function lastCall(): { url: string; form: FormData; config: { headers: Record<string, string> } } {
  const [url, form, config] = postMock.mock.calls[0]
  return { url, form: form as FormData, config }
}

beforeEach(() => {
  postMock.mockReset().mockResolvedValue({ data: undefined })
})

describe('chapterEdit 导入表单（预览复用）', () => {
  const base = {
    bookId: 'book_1',
    mode: 'book' as const,
    kind: 'edited' as const,
    makeCurrent: true,
  }

  it('带 previewId 的导入只传令牌，不再附文件/文本', async () => {
    const file = new File(['第一章 a\n正文'], 'book.txt', { type: 'text/plain' })
    await importChapterText({ ...base, file, text: '不应发送', previewId: 'pv-1' }, [0, 2])

    expect(postMock).toHaveBeenCalledTimes(1)
    const { url, form, config } = lastCall()
    expect(url).toBe('/importChapterText')
    expect(config.headers['Content-Type']).toBe('multipart/form-data')
    expect(form.get('previewId')).toBe('pv-1')
    expect(form.get('file')).toBeNull()
    expect(form.get('text')).toBeNull()
    expect(form.get('bookId')).toBe('book_1')
    expect(form.get('mode')).toBe('book')
    expect(form.get('selectedIndexes')).toBe('0,2')
  })

  it('不带 previewId 时维持旧行为：附上传文件', async () => {
    const file = new File(['第一章 a\n正文'], 'book.txt', { type: 'text/plain' })
    await importChapterText({ ...base, file })

    const { form } = lastCall()
    expect(form.get('previewId')).toBeNull()
    expect((form.get('file') as File | null)?.name).toBe('book.txt')
    expect(form.get('text')).toBeNull()
  })

  it('没有文件也没有 previewId 时附粘贴文本', async () => {
    await importChapterText({ ...base, text: '第一章 a\n正文' })

    const { form } = lastCall()
    expect(form.get('previewId')).toBeNull()
    expect(form.get('file')).toBeNull()
    expect(form.get('text')).toBe('第一章 a\n正文')
  })

  it('预览请求照常附文件，不带 previewId', async () => {
    const file = new File(['x'], 'a.txt')
    await previewChapterImport({ bookId: 'book_1', mode: 'book', file })

    const { url, form } = lastCall()
    expect(url).toBe('/previewChapterImport')
    expect(form.get('previewId')).toBeNull()
    expect((form.get('file') as File | null)?.name).toBe('a.txt')
  })
})
