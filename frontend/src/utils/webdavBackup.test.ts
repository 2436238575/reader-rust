import { beforeEach, describe, expect, it, vi } from 'vitest'
import { restoreWebdavBackup, type WebdavBackupPayload } from './webdavBackup'

// node 环境没有 localStorage：applyLocalState 需要最小实现
const localStore = new Map<string, string>()
vi.stubGlobal('localStorage', {
  getItem: (key: string) => localStore.get(key) ?? null,
  setItem: (key: string, value: string) => void localStore.set(key, value),
  removeItem: (key: string) => void localStore.delete(key),
})

const calls: string[] = []
const savedGroups: number[] = []
const restoredBooks: { bookId?: string; group?: number }[] = []

vi.mock('../api/bookmark', () => ({
  getBookmarks: vi.fn(async () => []),
  deleteBookmarks: vi.fn(async () => undefined),
  saveBookmarks: vi.fn(async () => {
    calls.push('saveBookmarks')
    return ''
  }),
}))
vi.mock('../api/bookshelf', () => ({
  deleteBookGroup: vi.fn(async () => undefined),
  deleteBooks: vi.fn(async () => undefined),
  getBookGroups: vi.fn(async () => []),
  getBookshelf: vi.fn(async () => []),
  restoreBooks: vi.fn(async (books: { bookId?: string; group?: number }[]) => {
    calls.push('restoreBooks')
    restoredBooks.push(...books)
    return []
  }),
  saveBookGroup: vi.fn(async (group: { groupId: number }) => {
    savedGroups.push(group.groupId)
    return ''
  }),
}))
vi.mock('../api/replaceRule', () => ({
  getReplaceRules: vi.fn(async () => []),
  deleteReplaceRules: vi.fn(async () => undefined),
  saveReplaceRules: vi.fn(async () => undefined),
}))
vi.mock('../api/source', () => ({
  deleteAllBookSources: vi.fn(async () => undefined),
  getBookSources: vi.fn(async () => []),
  saveBookSources: vi.fn(async () => undefined),
}))

function payload(overrides: Partial<WebdavBackupPayload>): WebdavBackupPayload {
  return {
    version: 1,
    createdAt: '',
    app: 'test',
    bookshelf: { books: [], groups: [] },
    bookSources: [],
    bookmarks: [],
    replaceRules: [],
    localState: {},
    ...overrides,
  }
}

describe('restoreWebdavBackup 的分组位域防御', () => {
  beforeEach(() => {
    calls.length = 0
    savedGroups.length = 0
    restoredBooks.length = 0
  })

  it('非法组 id 被丢弃，书籍位掩码里的悬空位被剥掉', async () => {
    await restoreWebdavBackup(
      payload({
        bookshelf: {
          // id=3 是老 max_id+1 时代的非法 id（0b11）；1 和 4 是合法幂位
          groups: [
            { groupId: 1, groupName: 'A', orderNo: 0 },
            { groupId: 3, groupName: '坏组', orderNo: 1 },
            { groupId: 4, groupName: 'B', orderNo: 2 },
          ],
          books: [
            // 0b111 = 组1 + 坏组3的两位 + 组4：坏组的位被剥掉后剩 0b101
            { bookId: 'book_1', group: 0b111 } as never,
            { bookId: 'book_2' } as never,
          ],
        },
      })
    )

    expect(savedGroups).toEqual([1, 4])
    expect(restoredBooks[0].group).toBe(0b101)
    expect(restoredBooks[1].group).toBe(0)
  })

  it('先恢复书籍再恢复书签（书签落库要按恢复后的登记表记 book_url）', async () => {
    await restoreWebdavBackup(
      payload({
        bookshelf: { books: [{ bookId: 'book_1' } as never], groups: [] },
        bookmarks: [{ bookId: 'book_1' } as never],
      })
    )
    expect(calls).toEqual(['restoreBooks', 'saveBookmarks'])
  })
})
