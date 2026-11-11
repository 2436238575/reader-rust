import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import { deleteBookGroup, getBookGroups, saveBookGroupId, saveBookGroup } from './bookshelf'
import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    patch: vi.fn(),
    delete: vi.fn(),
  },
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock
const patchMock = http.patch as unknown as Mock
const deleteMock = http.delete as unknown as Mock

describe('书架分组 api（v1）', () => {
  beforeEach(() => {
    getMock.mockReset()
    postMock.mockReset()
    patchMock.mockReset()
    deleteMock.mockReset()
  })

  it('getBookGroups 走 v1 分组列表', async () => {
    getMock.mockResolvedValue({ data: [{ groupId: 1, groupName: '默认', orderNo: 0 }] })

    await getBookGroups()

    expect(getMock).toHaveBeenCalledWith('/me/groups', { baseURL: API_V1_BASE })
  })

  it('新建/更新分组 upsert', async () => {
    postMock.mockResolvedValue({ data: undefined, status: 204 })
    const group = { groupId: 4, groupName: '四', orderNo: 0 }

    await saveBookGroup(group as never)

    expect(postMock).toHaveBeenCalledWith('/me/groups', group, { baseURL: API_V1_BASE })
  })

  it('删组走 DELETE 路径', async () => {
    deleteMock.mockResolvedValue({ data: undefined, status: 204 })

    await deleteBookGroup(4)

    expect(deleteMock).toHaveBeenCalledWith('/me/groups/4', { baseURL: API_V1_BASE })
  })

  it('单本书的分组归属走书架 PATCH 的 group 字段（0 = 清空）', async () => {
    patchMock.mockResolvedValue({ data: undefined, status: 204 })

    await saveBookGroupId('book_1', 4)
    expect(patchMock).toHaveBeenCalledWith('/me/shelf/book_1', { group: 4 }, {
      baseURL: API_V1_BASE,
    })

    await saveBookGroupId('book_1', 0)
    expect(patchMock).toHaveBeenCalledWith('/me/shelf/book_1', { group: 0 }, {
      baseURL: API_V1_BASE,
    })
  })
})
