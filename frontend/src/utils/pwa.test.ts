import { describe, expect, it, vi } from 'vitest'

function createStoreStub(pwaUpdateAvailable = false) {
  return {
    pwaUpdateAvailable,
    setWaitingServiceWorker: vi.fn(),
    setPwaUpdateAvailable: vi.fn(),
    showToast: vi.fn(),
  }
}

function createWorkerStub() {
  return { postMessage: vi.fn() } as unknown as ServiceWorker
}

// bindWaitingWorker 在模块级记录上次提示过的 worker 身份；逐用例重置模块，
// 避免前一个用例的提示记录影响后一个用例的判定
async function importFreshPwa() {
  vi.resetModules()
  return import('./pwa')
}

describe('bindWaitingWorker', () => {
  it('registers the waiting worker and marks the update as available', async () => {
    const { bindWaitingWorker } = await importFreshPwa()
    const store = createStoreStub()
    const worker = createWorkerStub()

    bindWaitingWorker(store, worker)

    expect(store.setWaitingServiceWorker).toHaveBeenCalledWith(worker)
    expect(store.setPwaUpdateAvailable).toHaveBeenCalledWith(true)
  })

  it('never posts SKIP_WAITING automatically — switching waits for the user', async () => {
    const { bindWaitingWorker } = await importFreshPwa()
    const store = createStoreStub()
    const worker = createWorkerStub()

    bindWaitingWorker(store, worker)

    expect(worker.postMessage).not.toHaveBeenCalled()
  })

  it('toasts the manual-update hint on first discovery', async () => {
    const { bindWaitingWorker } = await importFreshPwa()
    const store = createStoreStub()

    bindWaitingWorker(store, createWorkerStub())

    expect(store.showToast).toHaveBeenCalledTimes(1)
    expect(store.showToast).toHaveBeenCalledWith('发现新版本，可在设置中更新', 'success')
  })

  it('does not toast again for the same waiting worker', async () => {
    const { bindWaitingWorker } = await importFreshPwa()
    const store = createStoreStub()
    const worker = createWorkerStub()

    bindWaitingWorker(store, worker)
    // 周期性 update 检查再次命中同一个 waiting worker：状态照登记，提示不重复
    bindWaitingWorker(store, worker)

    expect(store.setWaitingServiceWorker).toHaveBeenCalledTimes(2)
    expect(store.setPwaUpdateAvailable).toHaveBeenCalledTimes(2)
    expect(store.showToast).toHaveBeenCalledTimes(1)
  })

  it('toasts again when a newer waiting worker replaces the rejected one', async () => {
    const { bindWaitingWorker } = await importFreshPwa()
    const store = createStoreStub()
    const workerV2 = createWorkerStub()
    const workerV3 = createWorkerStub()

    bindWaitingWorker(store, workerV2)
    expect(store.showToast).toHaveBeenCalledTimes(1)

    // 用户拒绝 v2（pwaUpdateAvailable 保持 true），v3 发布后 waiting worker 被顶替：
    // 按布尔值判断不会再提示，必须按 worker 身份识别出新版本
    bindWaitingWorker(store, workerV3)

    expect(store.setWaitingServiceWorker).toHaveBeenLastCalledWith(workerV3)
    expect(store.showToast).toHaveBeenCalledTimes(2)
  })

  it('ignores a missing worker', async () => {
    const { bindWaitingWorker } = await importFreshPwa()
    const store = createStoreStub()

    bindWaitingWorker(store, null)
    bindWaitingWorker(store, undefined)

    expect(store.setWaitingServiceWorker).not.toHaveBeenCalled()
    expect(store.setPwaUpdateAvailable).not.toHaveBeenCalled()
    expect(store.showToast).not.toHaveBeenCalled()

    // 空 worker 不占用提示记录：随后发现真实 worker 仍能正常提示
    bindWaitingWorker(store, createWorkerStub())
    expect(store.showToast).toHaveBeenCalledTimes(1)
  })
})
