const SHELL_CACHE = 'reader-shell-v1-0-2'
const RUNTIME_CACHE = 'reader-runtime-v1-0-2'
const API_CACHE = 'reader-api-v1-0-2'
const IMAGE_CACHE = 'reader-image-v1-0-2'
// 部署前缀按 SW 自身的位置推导（`/read/sw.js` → `/read/`）：
// public/ 下的文件不经过构建处理，写死根路径在子路径部署下会全部 404
const BASE = new URL('./', self.location).pathname
const SHELL_ASSETS = [
  BASE,
  `${BASE}index.html`,
  `${BASE}offline.html`,
  `${BASE}site.webmanifest`,
  `${BASE}favicon.ico`,
  `${BASE}favicon-32x32.png`,
  `${BASE}favicon-16x16.png`,
  `${BASE}apple-touch-icon.png`,
]

// 只读数据接口（GET）：network-first，离线时回退到最近一次成功的响应。
// 单用户应用 + 同源，URL 即缓存键（令牌在请求头里，不进缓存键）。
// POST 接口（正文/目录等）进不了 Cache API，由前端 IndexedDB 兜底。
const API_CACHE_PATHS = [
  'getBookshelf',
  'getShelfBookWithCacheInfo',
  'getBookGroups',
  'getBookmarks',
  'getReplaceRules',
  'getUserInfo',
  'getUserConfig',
  'getUserdata',
  'getBookSources',
].map((name) => `${BASE}reader3/${name}`)

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(SHELL_CACHE)
      .then((cache) => cache.addAll(SHELL_ASSETS))
      .then(() => self.skipWaiting())
  )
})

self.addEventListener('activate', (event) => {
  const keep = [SHELL_CACHE, RUNTIME_CACHE, API_CACHE, IMAGE_CACHE]
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => !keep.includes(key)).map((key) => caches.delete(key)))
      )
      .then(() => self.clients.claim())
  )
})

self.addEventListener('message', (event) => {
  if (event.data?.type === 'SKIP_WAITING') {
    self.skipWaiting()
  }
})

// 只读接口：联网成功即刷新缓存，断网/服务端不可达时回退缓存
function networkFirstApi(request) {
  return fetch(request)
    .then((response) => {
      if (response.ok) {
        const copy = response.clone()
        caches.open(API_CACHE).then((cache) => cache.put(request, copy))
      }
      return response
    })
    .catch(() =>
      caches.open(API_CACHE).then((cache) =>
        cache.match(request).then((cached) => {
          if (cached) return cached
          // 没有缓存可回退时把网络错误原样抛回去，让前端走自己的错误处理
          return Promise.reject(new Error('offline and no cached response'))
        })
      )
    )
}

self.addEventListener('fetch', (event) => {
  const { request } = event
  if (request.method !== 'GET') return

  const url = new URL(request.url)
  if (url.origin !== self.location.origin) return

  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request)
        .then((response) => {
          const copy = response.clone()
          caches.open(RUNTIME_CACHE).then((cache) => cache.put(request, copy))
          return response
        })
        .catch(
          async () =>
            (await caches.match(request)) ||
            caches.match(`${BASE}offline.html`) ||
            caches.match(`${BASE}index.html`)
        )
    )
    return
  }

  // 图片管道：id 即内容寻址（md5），同 id 内容不变，cache-first 安全
  if (url.pathname.startsWith(`${BASE}reader3/image/`)) {
    event.respondWith(
      caches.open(IMAGE_CACHE).then((cache) =>
        cache.match(request).then((cached) => {
          if (cached) return cached
          return fetch(request).then((response) => {
            if (response.ok) cache.put(request, response.clone())
            return response
          })
        })
      )
    )
    return
  }

  if (API_CACHE_PATHS.includes(url.pathname)) {
    event.respondWith(networkFirstApi(request))
    return
  }

  if (
    url.pathname.startsWith(`${BASE}assets/`) ||
    url.pathname.startsWith(`${BASE}icons/`) ||
    // 字体分片文件名带内容 hash（MiSansVF.<hash>.<n>.woff2），可以安全 cache-first
    url.pathname.startsWith(`${BASE}fonts/`) ||
    /\.(png|svg|css|js|ico)$/.test(url.pathname)
  ) {
    event.respondWith(
      caches.match(request).then((cached) => {
        if (cached) return cached
        return fetch(request).then((response) => {
          const copy = response.clone()
          caches.open(RUNTIME_CACHE).then((cache) => cache.put(request, copy))
          return response
        })
      })
    )
  }
})
