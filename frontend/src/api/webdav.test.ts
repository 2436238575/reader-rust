// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import http from './http'
import {
  createWebdavBackup,
  deleteWebdavFile,
  deleteWebdavFileList,
  getWebdavFileBlob,
  getWebdavFileList,
  inspectWebdavBackup,
  restoreWebdavBackup,
  uploadFilesToWebdav,
} from './webdav'
import { API_V1_BASE } from '../utils/appBase'

vi.mock('./http', () => ({
  default: {
    get: vi.fn(),
    post: vi.fn(),
    delete: vi.fn(),
  },
  UPLOAD_TIMEOUT_MS: 900_000,
}))

vi.mock('../utils/webdavLocalState', () => ({
  captureWebdavLocalState: () => ({ theme: 'dark' }),
}))

const getMock = http.get as unknown as Mock
const postMock = http.post as unknown as Mock
const deleteMock = http.delete as unknown as Mock

describe('webdav api（v1）', () => {
  beforeEach(() => {
    getMock.mockReset()
    postMock.mockReset()
    deleteMock.mockReset()
  })

  it('列表走 /v1/me/files?path=', async () => {
    getMock.mockResolvedValue({ data: [] })
    await getWebdavFileList('/backups')
    expect(getMock).toHaveBeenCalledWith('/me/files', {
      baseURL: API_V1_BASE,
      params: { path: '/backups' },
    })
  })

  it('下载按路径分段编码，保留斜杠', async () => {
    getMock.mockResolvedValue({ data: new Blob() })
    await getWebdavFileBlob('/备份 目录/a#1.json')
    expect(getMock).toHaveBeenCalledWith(
      '/me/files/%E5%A4%87%E4%BB%BD%20%E7%9B%AE%E5%BD%95/a%231.json',
      expect.objectContaining({ baseURL: API_V1_BASE, responseType: 'blob' })
    )
  })

  it('上传用 multipart + path 查询参数', async () => {
    postMock.mockResolvedValue({ data: [] })
    await uploadFilesToWebdav([{ file: new Blob(['x']), name: 'a.txt' }], '/docs')
    const [url, body, config] = postMock.mock.calls[0]
    expect(url).toBe('/me/files')
    expect(body).toBeInstanceOf(FormData)
    expect(config).toEqual(
      expect.objectContaining({
        baseURL: API_V1_BASE,
        params: { path: '/docs' },
      })
    )
  })

  it('上传超过 20 个文件时按 20 分片', async () => {
    postMock.mockResolvedValue({ data: [] })
    const files = Array.from({ length: 25 }, (_, index) => ({
      file: new Blob(['x']),
      name: `${index}.txt`,
    }))
    await uploadFilesToWebdav(files, '/many')
    expect(postMock).toHaveBeenCalledTimes(2)
    expect((postMock.mock.calls[0][1] as FormData).getAll('file0').length).toBe(1)
    expect((postMock.mock.calls[1][1] as FormData).getAll('file0').length).toBe(1)
    expect((postMock.mock.calls[0][1] as FormData).getAll('file19').length).toBe(1)
    expect((postMock.mock.calls[1][1] as FormData).getAll('file19').length).toBe(0)
  })

  it('单删用 DELETE /v1/me/files/{path}', async () => {
    deleteMock.mockResolvedValue({})
    await deleteWebdavFile('/docs/a.txt')
    expect(deleteMock).toHaveBeenCalledWith('/me/files/docs/a.txt', {
      baseURL: API_V1_BASE,
    })
  })

  it('批删按 500 条分片', async () => {
    postMock.mockResolvedValue({ data: {} })
    const paths = Array.from({ length: 1200 }, (_, index) => `/p/${index}.txt`)
    await deleteWebdavFileList(paths)
    expect(postMock).toHaveBeenCalledTimes(3)
    expect(postMock.mock.calls[0][1]).toEqual({ paths: paths.slice(0, 500) })
    expect(postMock.mock.calls[2][1]).toEqual({ paths: paths.slice(1000) })
    for (const call of postMock.mock.calls) {
      expect(call[0]).toBe('/me/files:batchDelete')
      expect(call[2]).toEqual({ baseURL: API_V1_BASE })
    }
  })

  it('创建备份携带本地配置快照', async () => {
    postMock.mockResolvedValue({ data: { name: 'reader-backup.json' } })
    await createWebdavBackup()
    expect(postMock).toHaveBeenCalledWith(
      '/me/backups',
      { localState: { theme: 'dark' } },
      { baseURL: API_V1_BASE }
    )
  })

  it('恢复前预览备份条目数', async () => {
    postMock.mockResolvedValue({ data: { books: 1, bookSources: 0 } })
    await inspectWebdavBackup('/backups/reader-backup.json')
    expect(postMock).toHaveBeenCalledWith(
      '/me/backups:inspect',
      { path: '/backups/reader-backup.json' },
      { baseURL: API_V1_BASE }
    )
  })

  it('恢复走 /v1/me/backups:restore', async () => {
    postMock.mockResolvedValue({ data: { books: 1 } })
    await restoreWebdavBackup('/backups/reader-backup.json')
    expect(postMock).toHaveBeenCalledWith(
      '/me/backups:restore',
      { path: '/backups/reader-backup.json' },
      expect.objectContaining({ baseURL: API_V1_BASE })
    )
  })
})
