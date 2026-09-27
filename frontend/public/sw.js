const SHELL_CACHE = 'reader-shell-v1-0-1'
const RUNTIME_CACHE = 'reader-runtime-v1-0-1'
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

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(SHELL_CACHE)
      .then((cache) => cache.addAll(SHELL_ASSETS))
      .then(() => self.skipWaiting()),
  )
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((key) => key !== SHELL_CACHE && key !== RUNTIME_CACHE).map((key) => caches.delete(key))),
    ).then(() => self.clients.claim()),
  )
})

self.addEventListener('message', (event) => {
  if (event.data?.type === 'SKIP_WAITING') {
    self.skipWaiting()
  }
})

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
        .catch(async () =>
          (await caches.match(request))
          || caches.match(`${BASE}offline.html`)
          || caches.match(`${BASE}index.html`),
        ),
    )
    return
  }

  if (
    url.pathname.startsWith(`${BASE}assets/`)
    || url.pathname.startsWith(`${BASE}icons/`)
    // 字体分片文件名带内容 hash（MiSansVF.<hash>.<n>.woff2），可以安全 cache-first
    || url.pathname.startsWith(`${BASE}fonts/`)
    || /\.(png|svg|css|js|ico)$/.test(url.pathname)
  ) {
    event.respondWith(
      caches.match(request).then((cached) => {
        if (cached) return cached
        return fetch(request).then((response) => {
          const copy = response.clone()
          caches.open(RUNTIME_CACHE).then((cache) => cache.put(request, copy))
          return response
        })
      }),
    )
  }
})
