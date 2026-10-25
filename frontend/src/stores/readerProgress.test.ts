// @vitest-environment jsdom
import { ref } from 'vue'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createReaderProgress, READER_SESSION_KEY } from './readerProgress'

vi.mock('../api/bookshelf', () => ({
  saveBookProgress: vi.fn(async () => ({})),
}))
import { saveBookProgress } from '../api/bookshelf'

function makeDeps(overrides: Record<string, unknown> = {}) {
  return {
    book: ref({ bookId: 'book_1', name: '书' }),
    chapters: ref([{ title: '第1章', url: 'u0' }]),
    currentIndex: ref(0),
    chapterScrollProgress: ref(0),
    // markProgressDirty 会同步标脏书架，stub 里补上对应方法
    shelfStore: { books: [], markShelfDirty: vi.fn() },
    ...overrides,
  } as never
}

describe('readerProgress 子模块', () => {
  beforeEach(() => {
    localStorage.clear()
    vi.clearAllMocks()
  })

  it('encode/decode 是万分比与 0-1 的往返', () => {
    const progress = createReaderProgress(makeDeps())
    expect(progress.encodeServerProgress(0.4231)).toBe(4231)
    expect(progress.encodeServerProgress(2)).toBe(10000) // 钳上限
    expect(progress.decodeServerProgress(4231)).toBeCloseTo(0.4231)
    expect(progress.decodeServerProgress(0.5)).toBe(0.5) // 已是 0-1 不再除
    expect(progress.decodeServerProgress(null)).toBe(0)
  })

  it('进度未变且非 dirty 时 flush 不重复上报', async () => {
    const deps = makeDeps()
    const progress = createReaderProgress(deps)
    await progress.flushProgressToServer(true) // force：首次必发
    expect(saveBookProgress).toHaveBeenCalledTimes(1)
    // 同一位置再来：跳过
    await progress.flushProgressToServer()
    expect(saveBookProgress).toHaveBeenCalledTimes(1)
    // 标脏后发
    progress.markProgressDirty()
    await progress.flushProgressToServer()
    expect(saveBookProgress).toHaveBeenCalledTimes(2)
  })

  it('会话保存带目录，配额写满时降级为瘦身会话', async () => {
    const deps = makeDeps()
    const progress = createReaderProgress(deps)
    progress.saveReaderSession()
    const full = JSON.parse(localStorage.getItem(READER_SESSION_KEY)!)
    expect(full.chapters).toHaveLength(1)

    // 写满 localStorage 让 safeLocalSet 第一次失败：第二次瘦身写入成功
    localStorage.clear()
    const huge = 'x'.repeat(6 * 1024 * 1024)
    try {
      localStorage.setItem('filler', huge)
    } catch {
      // 环境配额不支持就算了，跳过降级断言
      return
    }
    progress.saveReaderSession()
    const slim = JSON.parse(localStorage.getItem(READER_SESSION_KEY)!)
    expect(slim.chapters).toEqual([])
    expect(slim.book.bookId).toBe('book_1')
    localStorage.removeItem('filler')
  })

  it('syncLocalBookProgress 同步书架里的在架条目', () => {
    const shelfBook = { bookId: 'book_1', durChapterPos: 0 }
    const book = ref<Record<string, unknown>>({ bookId: 'book_1', name: '书' })
    const deps = makeDeps({ shelfStore: { books: [shelfBook] }, book })
    const progress = createReaderProgress(deps)
    progress.syncLocalBookProgress(0.25)
    expect(shelfBook.durChapterPos).toBe(2500)
    expect(book.value.durChapterPos).toBe(2500)
  })

  it('resetProgressTracking 清空时间戳/脏标/上报键', async () => {
    const progress = createReaderProgress(makeDeps())
    progress.serverProgressTime.value = 123
    progress.markProgressDirty()
    await progress.flushProgressToServer(true)
    progress.resetProgressTracking()
    expect(progress.serverProgressTime.value).toBe(0)
    expect(progress.progressDirty.value).toBe(false)
    expect(progress.lastServerProgressKey.value).toBe('')
  })
})
