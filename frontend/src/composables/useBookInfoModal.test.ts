import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useBookInfoModal } from './useBookInfoModal'
import { getBookInfo } from '../api/bookshelf'

vi.mock('../api/bookshelf', () => ({
  getBookInfo: vi.fn(),
}))

const getBookInfoMock = vi.mocked(getBookInfo)

function makeStore(overrides: Record<string, unknown> = {}) {
  return {
    book: {
      bookId: 'book_1',
      name: '旧书名',
      author: '作者',
      bookUrl: 'https://up.example/book',
      origin: 'https://source.example',
      originName: '书源甲',
      durChapterTitle: '第一章',
    },
    currentIndex: 2,
    currentChapter: { title: '第三章' },
    ...overrides,
  } as never
}

describe('useBookInfoModal（v1 详情覆盖层）', () => {
  beforeEach(() => {
    getBookInfoMock.mockReset()
  })

  it('详情缺席的 origin/originName 保留书架条目的真值', async () => {
    getBookInfoMock.mockResolvedValue({
      bookId: 'book_1',
      name: '新书名',
      bookUrl: 'https://up.example/book',
      kind: '玄幻',
    })

    const modal = useBookInfoModal(
      makeStore(),
      {} as never,
      { showToast: vi.fn() } as never,
      {} as never,
    )
    await modal.openInfo()

    expect(modal.bookInfoBook.value).toMatchObject({
      name: '新书名',
      kind: '玄幻',
      origin: 'https://source.example',
      originName: '书源甲',
      durChapterIndex: 2,
      durChapterTitle: '第三章',
    })
  })

  it('值为 undefined 的键也不覆盖旧值（双保险）', async () => {
    getBookInfoMock.mockResolvedValue({
      bookId: 'book_1',
      name: '新书名',
      bookUrl: 'https://up.example/book',
      intro: undefined,
      origin: undefined,
    } as never)

    const modal = useBookInfoModal(
      makeStore(),
      {} as never,
      { showToast: vi.fn() } as never,
      {} as never,
    )
    await modal.openInfo()

    expect(modal.bookInfoBook.value?.origin).toBe('https://source.example')
    expect(modal.bookInfoBook.value?.intro).toBeUndefined()
  })

  it('详情请求失败时保留当前会话的书并提示', async () => {
    getBookInfoMock.mockRejectedValue(new Error('boom'))
    const showToast = vi.fn()

    const modal = useBookInfoModal(
      makeStore(),
      {} as never,
      { showToast } as never,
      {} as never,
    )
    await modal.openInfo()

    expect(modal.bookInfoBook.value?.originName).toBe('书源甲')
    expect(showToast).toHaveBeenCalledWith(
      '获取书籍详情失败，已显示当前缓存信息',
      'warning',
    )
  })
})
