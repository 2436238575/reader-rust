import http, { UPLOAD_TIMEOUT_MS } from './http'
import { API_V1_BASE } from '../utils/appBase'
import { captureWebdavLocalState } from '../utils/webdavLocalState'

export interface WebdavFileEntry {
  name: string
  path: string
  size: number
  isDirectory: boolean
  /** RFC 3339 UTC */
  modifiedAt: string
}

export interface WebdavRestoreResult {
  books: number
  groups: number
  bookSources: number
  bookmarks: number
  skippedBookmarks: number
  replaceRules: number
  /** 备份里的 localStorage 快照；null = 这份备份不含本地配置，不要改动当前键 */
  localState?: Record<string, string> | null
}

export interface WebdavBackupInspect {
  version: number
  createdAt: string
  books: number
  groups: number
  bookSources: number
  bookmarks: number
  replaceRules: number
  /** 备份是否包含浏览器本地配置 */
  hasLocalState: boolean
}

/** 批量删除分片：与后端单次 1000 条上限留出余量。 */
const BATCH_DELETE_CHUNK = 500

/** 单请求最大文件数：后端 multipart 字段上限 20（path 走 query，不占字段）。 */
const UPLOAD_FILE_CHUNK = 20

/** 文件路径逐段编码：路径本身是资源身份，不能整体 encode（斜杠要保留）。 */
function fileUrl(path: string) {
  const encoded = path
    .split('/')
    .filter(Boolean)
    .map((segment) => encodeURIComponent(segment))
    .join('/')
  return `/me/files/${encoded}`
}

export function getWebdavFileList(path = '/') {
  return http
    .get<WebdavFileEntry[]>('/me/files', {
      baseURL: API_V1_BASE,
      params: { path },
    })
    .then((r) => r.data)
}

export function getWebdavFileBlob(path: string) {
  return http
    .get<Blob>(fileUrl(path), {
      baseURL: API_V1_BASE,
      responseType: 'blob',
      timeout: UPLOAD_TIMEOUT_MS,
    })
    .then((r) => r.data)
}

export async function uploadFilesToWebdav(
  files: Array<{ file: Blob; name: string }>,
  path = '/'
) {
  const uploaded: WebdavFileEntry[] = []
  for (let index = 0; index < files.length; index += UPLOAD_FILE_CHUNK) {
    const formData = new FormData()
    files.slice(index, index + UPLOAD_FILE_CHUNK).forEach((item, itemIndex) => {
      formData.append(`file${itemIndex}`, item.file, item.name)
    })
    const response = await http.post<WebdavFileEntry[]>('/me/files', formData, {
      baseURL: API_V1_BASE,
      params: { path },
      timeout: UPLOAD_TIMEOUT_MS,
      headers: {
        'Content-Type': 'multipart/form-data',
      },
    })
    uploaded.push(...response.data)
  }
  return uploaded
}

export function deleteWebdavFile(path: string) {
  return http.delete(fileUrl(path), { baseURL: API_V1_BASE }).then(() => undefined)
}

export async function deleteWebdavFileList(paths: string[]) {
  for (let index = 0; index < paths.length; index += BATCH_DELETE_CHUNK) {
    await http.post(
      '/me/files:batchDelete',
      { paths: paths.slice(index, index + BATCH_DELETE_CHUNK) },
      { baseURL: API_V1_BASE }
    )
  }
}

/** 服务端快照：写入 /backups/reader-backup-<UTC>.json；本地配置由前端采集。 */
export function createWebdavBackup() {
  return http
    .post<WebdavFileEntry>(
      '/me/backups',
      { localState: captureWebdavLocalState() },
      { baseURL: API_V1_BASE }
    )
    .then((r) => r.data)
}

/** 恢复前预览：确认框展示各域条数，空书源等异常在点确定前可见。 */
export function inspectWebdavBackup(path: string) {
  return http
    .post<WebdavBackupInspect>(
      '/me/backups:inspect',
      { path },
      { baseURL: API_V1_BASE }
    )
    .then((r) => r.data)
}

/** 以备份为准恢复；返回各域条数与待前端落地的 localStorage 快照。 */
export function restoreWebdavBackup(path: string) {
  return http
    .post<WebdavRestoreResult>(
      '/me/backups:restore',
      { path },
      { baseURL: API_V1_BASE, timeout: UPLOAD_TIMEOUT_MS }
    )
    .then((r) => r.data)
}
