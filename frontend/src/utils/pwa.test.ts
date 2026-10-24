import { describe, expect, it, vi } from 'vitest'
import { bindWaitingWorker } from './pwa'

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

describe('bindWaitingWorker', () => {
  it('registers the waiting worker and marks the update as available', () => {
    const store = createStoreStub()
    const worker = createWorkerStub()

    bindWaitingWorker(store, worker)

    expect(store.setWaitingServiceWorker).toHaveBeenCalledWith(worker)
    expect(store.setPwaUpdateAvailable).toHaveBeenCalledWith(true)
  })

  it('never posts SKIP_WAITING automatically — switching waits for the user', () => {
    const store = createStoreStub()
    const worker = createWorkerStub()

    bindWaitingWorker(store, worker)

    expect(worker.postMessage).not.toHaveBeenCalled()
  })

  it('toasts the manual-update hint only on first discovery', () => {
    const store = createStoreStub()
    bindWaitingWorker(store, createWorkerStub())
    expect(store.showToast).toHaveBeenCalledTimes(1)
    expect(store.showToast).toHaveBeenCalledWith('发现新版本，可在设置中更新', 'success')

    const alreadyAvailable = createStoreStub(true)
    bindWaitingWorker(alreadyAvailable, createWorkerStub())
    expect(alreadyAvailable.setPwaUpdateAvailable).toHaveBeenCalledWith(true)
    expect(alreadyAvailable.showToast).not.toHaveBeenCalled()
  })

  it('ignores a missing worker', () => {
    const store = createStoreStub()

    bindWaitingWorker(store, null)
    bindWaitingWorker(store, undefined)

    expect(store.setWaitingServiceWorker).not.toHaveBeenCalled()
    expect(store.setPwaUpdateAvailable).not.toHaveBeenCalled()
    expect(store.showToast).not.toHaveBeenCalled()
  })
})
