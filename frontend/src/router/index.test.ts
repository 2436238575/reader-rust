// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import router from './index'
import { AUTH_HINT_STORAGE_KEY } from '../utils/secureAccess'

// 路由表里的视图都是懒加载，导航确认时会真的 resolve 组件；替成空组件，
// 避免把整个阅读器视图链拉进守卫单测
vi.mock('../views/ReaderView.vue', () => ({
  default: { name: 'ReaderView', render: () => null },
}))
vi.mock('../views/LoginView.vue', () => ({
  default: { name: 'LoginView', render: () => null },
}))

beforeEach(() => {
  localStorage.clear()
})

describe('路由守卫依赖登录提示（hint）', () => {
  it('没有 hint 时跳转登录页并携带 redirect', async () => {
    await router.push('/reader')

    expect(router.currentRoute.value.name).toBe('login')
    expect(router.currentRoute.value.query.redirect).toBe('/reader')
  })

  it('有 hint 时放行受保护路由（不再读取任何令牌）', async () => {
    localStorage.setItem(AUTH_HINT_STORAGE_KEY, JSON.stringify({ username: 'admin' }))

    await router.push('/reader')

    expect(router.currentRoute.value.name).toBe('reader')
  })

  it('登录页自身总是放行', async () => {
    await router.push('/login')

    expect(router.currentRoute.value.name).toBe('login')
  })
})
