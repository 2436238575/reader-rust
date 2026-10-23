import { test, expect } from '@playwright/test'
import { gotoApp, loginViaPage, openSettings } from './support/app'
import { hasLogin, readE2EConfig } from './support/env'

const config = readE2EConfig()

test.describe('Standard E2E: refresh token', () => {
  test.beforeEach(async ({ page }) => {
    test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable the refresh regression.')
    await loginViaPage(page, config.username!, config.password!)
    await page.waitForFunction(() => !!localStorage.getItem('refreshToken'))
  })

  test('recovers the session after the access token is gone', async ({ page }) => {
    const previousRefresh = await page.evaluate(() => localStorage.getItem('refreshToken'))
    expect(previousRefresh).toBeTruthy()

    // 模拟 Access Token 过期/丢失：只留 Refresh Token，刷新页面应能静默续期
    await page.evaluate(() => localStorage.removeItem('accessToken'))
    await page.reload()

    await page.waitForFunction((oldToken) => {
      const access = localStorage.getItem('accessToken')
      const refresh = localStorage.getItem('refreshToken')
      return !!access && !!refresh && refresh !== oldToken
    }, previousRefresh)

    await expect(page.getByRole('heading', { name: /书架/ })).toBeVisible()
  })

  test('clears both tokens on logout', async ({ page }) => {
    await gotoApp(page, '/')
    await openSettings(page)
    await page.getByRole('button', { name: '退出登录', exact: true }).click()

    await page.waitForFunction(
      () => !localStorage.getItem('accessToken') && !localStorage.getItem('refreshToken')
    )
    await expect(page).toHaveURL(/#\/login/)
  })
})
