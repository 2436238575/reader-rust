import { test, expect } from '@playwright/test'
import { gotoApp, loginViaPage, openSettings } from './support/app'
import { hasLogin, readE2EConfig } from './support/env'

const config = readE2EConfig()

test.describe('Standard E2E: cookie session', () => {
  test.beforeEach(async ({ page }) => {
    test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable the refresh regression.')
    await loginViaPage(page, config.username!, config.password!)
    // 会话凭据在 HttpOnly Cookie 里；localStorage 只留非敏感登录提示
    await page.waitForFunction(() => !!localStorage.getItem('reader-auth-hint'))
  })

  test('recovers the session after the access cookie is gone', async ({ page, context }) => {
    const before = await context.cookies()
    const accessBefore = before.find((cookie) => cookie.name === 'reader_access')
    const refreshBefore = before.find((cookie) => cookie.name === 'reader_refresh')
    expect(accessBefore, '登录后应有 reader_access Cookie').toBeTruthy()
    expect(refreshBefore, '登录后应有 reader_refresh Cookie').toBeTruthy()
    expect(accessBefore!.httpOnly).toBe(true)
    expect(refreshBefore!.httpOnly).toBe(true)

    // 模拟 Access Token 过期/丢失：只删 reader_access，保留 reader_refresh
    const kept = before.filter((cookie) => cookie.name !== 'reader_access')
    await context.clearCookies()
    await context.addCookies(kept)
    await page.reload()

    // 启动引导：getUserInfo 拿不到用户时凭 refresh Cookie 刷新，再取一次用户
    await expect(page.getByRole('heading', { name: /书架/ })).toBeVisible()
    const after = await context.cookies()
    const accessAfter = after.find((cookie) => cookie.name === 'reader_access')
    const refreshAfter = after.find((cookie) => cookie.name === 'reader_refresh')
    expect(accessAfter, '刷新后应重新下发 reader_access').toBeTruthy()
    expect(refreshAfter, '刷新后应重新下发 reader_refresh').toBeTruthy()
    // 一次性轮换：Refresh Cookie 的值必须变化
    expect(refreshAfter!.value).not.toBe(refreshBefore!.value)
  })

  test('clears both cookies and the hint on logout', async ({ page, context }) => {
    await gotoApp(page, '/')
    await openSettings(page)
    await page.getByRole('button', { name: '退出登录', exact: true }).click()

    await expect(page).toHaveURL(/#\/login/)
    const cookies = await context.cookies()
    expect(cookies.some((cookie) => cookie.name === 'reader_access')).toBe(false)
    expect(cookies.some((cookie) => cookie.name === 'reader_refresh')).toBe(false)
    expect(await page.evaluate(() => localStorage.getItem('reader-auth-hint'))).toBeNull()
  })
})
