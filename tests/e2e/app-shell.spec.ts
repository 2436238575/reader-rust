import { test, expect } from '@playwright/test'
import { closeSettings, gotoApp, loginViaPage, openSettings, triggerGlobalSearch } from './support/app'
import { hasLogin, readE2EConfig } from './support/env'

const config = readE2EConfig()

/** 预置两个假书源（搜索页的范围筛选在可选源 ≤1 时整行隐藏） */
async function seedTwoSources(page) {
  const mk = (id: number) => ({
    bookSourceName: `假源${id}`,
    bookSourceUrl: `https://fake-source-${id}.example`,
    enabled: true,
  })
  // 登录点击返回后 hint 落笔是异步的：先等它出现；会话 Cookie 会随同源
  // fetch 自动携带，不再从 localStorage 取令牌
  await page.waitForFunction(() => !!localStorage.getItem('reader-auth-hint'))
  const ok = await page.evaluate(async (sources) => {
    const resp = await fetch('/reader3/saveBookSources', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(sources),
    })
    return resp.ok
  }, [mk(1), mk(2)])
  expect(ok, '预置假书源应成功').toBe(true)
}

test.describe('Standard E2E: app shell', () => {
  // 应用全站需要登录：没有配置凭据时整组跳过（与 standard-flow 同约定）
  test.beforeEach(async ({ page }) => {
    test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable app shell regression.')
    await loginViaPage(page, config.username!, config.password!)
  })

  test('navigates across core tabs', async ({ page }) => {
    await gotoApp(page, '/')

    await expect(page.getByRole('link', { name: '书架', exact: true })).toBeVisible()
    await page.getByRole('link', { name: '发现', exact: true }).click()
    await expect(page).toHaveURL(/#\/explore$/)
    await expect(page.getByRole('heading', { name: '发现' })).toBeVisible()

    await page.getByRole('link', { name: '最近', exact: true }).click()
    await expect(page).toHaveURL(/#\/recent$/)

    await page.getByRole('link', { name: '书架', exact: true }).click()
    await expect(page).toHaveURL(/#\/?$/)
    await expect(page.getByRole('heading', { name: /书架/ })).toBeVisible()
  })

  test('opens settings and management entry points', async ({ page }) => {
    await gotoApp(page, '/')
    await openSettings(page)

    await expect(page.getByRole('button', { name: '书源管理', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '缓存管理', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '备份与恢复', exact: true })).toBeVisible()

    await closeSettings(page)
  })

  test('enters search mode and exposes scope filters', async ({ page }) => {
    await gotoApp(page, '/')
    await seedTwoSources(page)
    await triggerGlobalSearch(page, '测试')

    await expect(page.getByRole('button', { name: '全部书源', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '按分组', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '单个书源', exact: true })).toBeVisible()
  })

  test('opens bookshelf management dialogs', async ({ page }) => {
    await gotoApp(page, '/')

    await page.getByRole('button', { name: '分组管理' }).click()
    await expect(page.getByRole('heading', { name: '分组管理' })).toBeVisible()
    await page.locator('.modal-container .close-btn').first().click()
    await expect(page.getByRole('heading', { name: '分组管理' })).not.toBeVisible()

    await openSettings(page)
    await page.getByRole('button', { name: '缓存管理' }).click()
    await expect(page.getByRole('heading', { name: '缓存管理' })).toBeVisible()
  })
})
