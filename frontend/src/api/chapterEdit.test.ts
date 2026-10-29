// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  importChapterText,
  previewChapterImport,
  uploadChapterImportFile,
} from './chapterEdit'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
  },
  UPLOAD_TIMEOUT_MS: 900_000,
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
    expect(form.get('fileSize')).toBeNull()
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
    // 原始字节数用于后端核对上传是否完整
    expect(form.get('fileSize')).toBe(String(file.size))
    // fileSize 必须排在 file 前面：代理截断正文时小字段先到
    const keys = [...form.keys()]
    expect(keys.indexOf('fileSize')).toBeLessThan(keys.indexOf('file'))
    expect(form.get('text')).toBeNull()
  })

  it('带 uploadId 时只传令牌，不再附文件/文本', async () => {
    const file = new File(['第一章 a\n正文'], 'book.txt', { type: 'text/plain' })
    await importChapterText({ ...base, file, uploadId: 'u-1' }, [0])

    const { form } = lastCall()
    expect(form.get('uploadId')).toBe('u-1')
    expect(form.get('previewId')).toBeNull()
    expect(form.get('file')).toBeNull()
    expect(form.get('fileSize')).toBeNull()
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

describe('自适应分片上传', () => {
  it('413 后减半分片并把上限钉住，最终拼完整个文件', async () => {
    const file = new File([new Uint8Array(3 * 1024 * 1024)], 'big.txt')
    const attempted: number[] = []
    const afterFailure: number[] = []
    const progress: number[] = []
    let failed = false

    postMock.mockReset().mockImplementation(
      async (_url: string, form: FormData) => {
        const offset = Number(form.get('offset'))
        const chunk = form.get('chunk') as File
        attempted.push(chunk.size)
        // 模拟代理上限约 1MB：超过 768KB 的分片直接断连（无状态码）
        if (chunk.size > 768 * 1024) {
          failed = true
          throw Object.assign(new Error('network reset'), { status: undefined })
        }
        if (failed) afterFailure.push(chunk.size)
        return {
          data: {
            uploadId: (form.get('uploadId') as string | null) ?? 'u-1',
            received: offset + chunk.size,
            complete: offset + chunk.size >= file.size,
          },
        }
      }
    )

    const id = await uploadChapterImportFile(file, (uploaded) => progress.push(uploaded))

    expect(id).toBe('u-1')
    expect(progress[progress.length - 1]).toBe(file.size)
    // 至少有一次超过 768KB 的尝试被代理拒绝
    expect(attempted.some((size) => size > 768 * 1024)).toBe(true)
    // 失败后减半到 576KB 并把上限钉住，后续成功分片不再涨回去
    expect(afterFailure.length).toBeGreaterThan(0)
    expect(afterFailure.every((size) => size <= 576 * 1024)).toBe(true)
  })
})
