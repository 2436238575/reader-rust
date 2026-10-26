const SHELL_CACHE = 'reader-shell-v1-0-5'
const RUNTIME_CACHE = 'reader-runtime-v1-0-5'
const API_CACHE = 'reader-api-v1-0-5'
const IMAGE_CACHE = 'reader-image-v1-0-5'
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
  // 单文档 getUserdata 已无调用方；批量读走 getUserdataBatch?names=a,b，
  // 上方按 pathname 精确匹配，查询串随 request.url 进缓存键、按文档名组合分条目
  'getUserdataBatch',
  'getBookSources',
].map((name) => `${BASE}reader3/${name}`)

// 不在 install 里 skipWaiting：新 SW 安装完成后停留在 waiting 状态，
// 由用户在设置里点「更新应用」后通过 message 事件触发 SKIP_WAITING 接管，
// 避免发版瞬间强刷所有打开中的标签页（阅读 / TTS / 缓存全书都会被打断）。
// 首次安装（没有旧 SW 控制页面）时浏览器会直接激活新 SW，不受此影响。
self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(SHELL_CACHE).then((cache) => cache.addAll(SHELL_ASSETS)))
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

// 各缓存库的条目上限：keys() 按插入序返回，超出时逐出最旧一批。
// 没有上限时图片库会随阅读无限增长（图片本体是最占空间的一类）
const CACHE_ENTRY_LIMITS = {
  [RUNTIME_CACHE]: 80,
  [API_CACHE]: 60,
  [IMAGE_CACHE]: 300,
}

async function putWithLimit(cacheName, key, response) {
  const cache = await caches.open(cacheName)
  await cache.put(key, response)
  const limit = CACHE_ENTRY_LIMITS[cacheName]
  if (!limit) return
  const keys = await cache.keys()
  const excess = keys.length - limit
  for (const oldKey of keys.slice(0, excess)) {
    await cache.delete(oldKey)
  }
}

// 只读接口：联网成功即刷新缓存，断网/服务端不可达时回退缓存
function networkFirstApi(request) {
  return fetch(request)
    .then((response) => {
      if (response.ok) {
        const copy = response.clone()
        putWithLimit(API_CACHE, request.url, copy)
      }
      return response
    })
    .catch(() =>
      caches.open(API_CACHE).then((cache) =>
        cache.match(request.url).then((cached) => {
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
          putWithLimit(RUNTIME_CACHE, request.url, copy)
          return response
        })
        .catch(
          async () =>
            (await caches.match(request.url)) ||
            caches.match(`${BASE}offline.html`) ||
            caches.match(`${BASE}index.html`)
        )
    )
    return
  }

  // 图片管道：id 即内容寻址（md5），同 id 内容不变，cache-first 安全
  if (url.pathname.startsWith(`${BASE}reader3/image/`)) {
    const key = request.url
    event.respondWith(
      caches.open(IMAGE_CACHE).then((cache) =>
        cache.match(key).then((cached) => {
          if (cached) return cached
          return fetch(request).then((response) => {
            if (response.ok) putWithLimit(IMAGE_CACHE, key, response.clone())
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
      caches.match(request.url).then((cached) => {
        if (cached) return cached
        return fetch(request).then((response) => {
          const copy = response.clone()
          putWithLimit(RUNTIME_CACHE, request.url, copy)
          return response
        })
      })
    )
  }
})
