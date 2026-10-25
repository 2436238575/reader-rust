import type { useAppStore } from '../stores/app'

type AppStore = ReturnType<typeof useAppStore>

function isLocalhostEnv() {
  if (typeof window === 'undefined') return false
  const hostname = window.location.hostname
  return hostname === 'localhost' || hostname === '127.0.0.1'
}

async function clearServiceWorkerCaches() {
  if (typeof navigator === 'undefined' || !('serviceWorker' in navigator)) return
  const registrations = await navigator.serviceWorker.getRegistrations()
  await Promise.all(registrations.map((registration) => registration.unregister()))
  if (typeof window !== 'undefined' && 'caches' in window) {
    const keys = await caches.keys()
    await Promise.all(keys.map((key) => caches.delete(key)))
  }
}

type PwaUpdateStore = Pick<
  AppStore,
  'pwaUpdateAvailable' | 'setWaitingServiceWorker' | 'setPwaUpdateAvailable' | 'showToast'
>

// 发现 waiting worker 时只登记状态并提示，不主动 SKIP_WAITING：
// 是否切换由用户在设置抽屉点「更新应用」决定（applyPwaUpdate → message → SKIP_WAITING
// → controllerchange → 刷新一次），避免发版瞬间强刷正在阅读 / 听书 / 缓存全书的标签页。
//
// 提示按 waiting worker 的身份去重而不是看 pwaUpdateAvailable：用户拒绝 v2 后
// 发布 v3，waiting worker 会被新对象顶替而 pwaUpdateAvailable 保持 true 不变，
// 按布尔值判断就再也不提示了；worker 变化时必须重新 toast，同一个 worker
// （周期性 update 检查反复命中）不重复提示。应用更新后页面随 controllerchange
// 刷新，模块状态随之重置，无需手动清。
let lastNotifiedWaitingWorker: ServiceWorker | null = null

export function bindWaitingWorker(
  appStore: PwaUpdateStore,
  worker: ServiceWorker | null | undefined
) {
  if (!worker) return
  appStore.setWaitingServiceWorker(worker)
  appStore.setPwaUpdateAvailable(true)
  if (worker !== lastNotifiedWaitingWorker) {
    lastNotifiedWaitingWorker = worker
    appStore.showToast('发现新版本，可在设置中更新', 'success')
  }
}

export function registerPwa(appStore: AppStore) {
  if (typeof window === 'undefined') return

  appStore.setOnlineStatus(navigator.onLine)

  window.addEventListener('online', () => {
    appStore.setOnlineStatus(true)
    appStore.showToast('网络已恢复', 'success')
  })

  window.addEventListener('offline', () => {
    appStore.setOnlineStatus(false)
    appStore.showToast('已进入离线模式', 'warning')
  })

  window.addEventListener('beforeinstallprompt', (event) => {
    event.preventDefault()
    appStore.setDeferredInstallPrompt(event)
  })

  window.addEventListener('appinstalled', () => {
    appStore.setDeferredInstallPrompt(null)
    appStore.showToast('已安装到主屏幕', 'success')
  })

  if (!('serviceWorker' in navigator)) return

  if (isLocalhostEnv()) {
    window.addEventListener('load', () => {
      void clearServiceWorkerCaches().finally(() => {
        appStore.setPwaReady(false)
      })
    })
    return
  }

  async function checkForUpdates(registration: ServiceWorkerRegistration) {
    try {
      await registration.update()
      if (registration.waiting) {
        bindWaitingWorker(appStore, registration.waiting)
      }
    } catch {
      // Ignore update check errors and keep the current app available.
    }
  }

  window.addEventListener('load', () => {
    navigator.serviceWorker
      .register(`${import.meta.env.BASE_URL}sw.js`)
      .then((registration) => {
        appStore.setPwaReady(true)

        if (registration.waiting) {
          bindWaitingWorker(appStore, registration.waiting)
        }

        void checkForUpdates(registration)

        registration.addEventListener('updatefound', () => {
          const installing = registration.installing
          if (!installing) return
          installing.addEventListener('statechange', () => {
            if (installing.state === 'installed' && navigator.serviceWorker.controller) {
              bindWaitingWorker(appStore, registration.waiting || installing)
            }
          })
        })

        let refreshing = false
        navigator.serviceWorker.addEventListener('controllerchange', () => {
          if (refreshing) return
          refreshing = true
          appStore.setPwaUpdateAvailable(false)
          appStore.setWaitingServiceWorker(null)
          window.location.reload()
        })

        window.setInterval(
          () => {
            if (!navigator.onLine) return
            void checkForUpdates(registration)
          },
          5 * 60 * 1000
        )
      })
      .catch(() => {
        appStore.setPwaReady(false)
      })
  })
}
