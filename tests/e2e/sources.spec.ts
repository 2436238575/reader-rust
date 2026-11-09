import { test, expect } from '@playwright/test'
import { gotoApp, loginViaPage } from './support/app'
import { hasLogin, readE2EConfig } from './support/env'

const config = readE2EConfig()

/**
 * 书源管理页的浏览器侧验收：列表经 /v1/sources → api 适配层 → store 渲染，
 * 编辑器经 `parseSourceId` 回数字 id 后走 /reader3/getBookSource 取全量。
 * 这条链路 cargo test 覆盖不到，只能由 Playwright 验证。
 */
test.describe('Standard E2E: source manager', () => {
  test.beforeEach(async ({ page }) => {
    test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable the source manager regression.')
    await loginViaPage(page, config.username!, config.password!)
  })

  test('list renders from v1 and edit modal loads the full source', async ({ page }) => {
    // 预置两个假书源（/reader3 写链路，sources 写批次前仍有效）
    await page.waitForFunction(() => !!localStorage.getItem('reader-auth-hint'))
    const ok = await page.evaluate(async () => {
      const resp = await fetch('/reader3/saveBookSources', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify([
          { bookSourceName: 'E2E 源甲', bookSourceUrl: 'https://e2e-source-a.example' },
          { bookSourceName: 'E2E 源乙', bookSourceUrl: 'https://e2e-source-b.example' },
        ]),
      })
      return resp.ok
    })
    expect(ok, '预置假书源应成功').toBe(true)

    await gotoApp(page, '/sources')
    // 列表来自 /v1/sources（游标分页拉全量 + 适配层）：两个源都应渲染出来
    await expect(page.getByText('E2E 源甲')).toBeVisible()
    await expect(page.getByText('E2E 源乙')).toBeVisible()

    // 点击条目进入编辑器：editSource 用数字 id 调 /reader3/getBookSource 取全量，
    // 成功才能看到带 URL 的编辑面板（这条验证 src_<n> → id 的反向映射）
    await page.getByRole('button', { name: /E2E 源甲/ }).click()
    await expect(page.locator('.editor-title h3')).toHaveText('E2E 源甲')
    await expect(page.locator('.editor-title p')).toHaveText('https://e2e-source-a.example')
  })
})
