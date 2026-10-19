import { test, expect } from '@playwright/test'
import { gotoApp, loginViaPage } from './support/app'
import { hasLogin, readE2EConfig } from './support/env'

const config = readE2EConfig()

/** 经 API 上传一本自带三章的 TXT（自包含，不依赖外部网络），返回书名。
 *  文件名带时间戳：共享 storage 的重复运行互不污染（同内容同 hash 会复用同一本书，
 *  上次跑到的阅读进度会串到本次断言里）。 */
async function seedLocalTxtBook(page): Promise<{ bookId: string; name: string }> {
  await page.waitForFunction(() => !!localStorage.getItem('accessToken'))
  const seed = await page.evaluate(async () => {
    const token = localStorage.getItem('accessToken')
    const content = '第一章 开始\n第一章正文内容。\n第二章 中段\n第二章正文内容。\n第三章 收尾\n第三章正文内容。\n'
    const form = new FormData()
    form.append('file', new File([content], `阅读器冒烟-${Date.now()}.txt`, { type: 'text/plain' }))
    const resp = await fetch('/reader3/uploadTxtBook', {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}` },
      body: form,
    })
    if (!resp.ok) return null
    const payload = await resp.json()
    return { bookId: payload?.data?.bookId ?? null, name: payload?.data?.name ?? '' }
  })
  expect(seed.bookId, 'TXT 上传应返回 bookId').toBeTruthy()
  return seed
}

test.describe('Standard E2E: reader', () => {
  test.beforeEach(async ({ page }) => {
    test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable reader regression.')
    await loginViaPage(page, config.username!, config.password!)
  })

  test('opens local book, reads, pages, and jumps via catalog', async ({ page }) => {
    const { bookId, name } = await seedLocalTxtBook(page)

    // 书架打开书（卡片区应出现刚上传的书）
    // 登录落地页就是书架：同 URL goto 不会重抓，必须 reload 才会带上新上传的书
    await page.reload()
    await expect(page.locator('[data-v-app]')).toBeVisible()
    // 点书卡容器（封面区是 click.stop 的独立入口，点标题文本可能落进它）
    const card = page.locator('.book-card', { hasText: name }).first()
    await expect(card).toBeVisible()
    await card.click()

    // 阅读器加载出第一章正文
    await expect(page).toHaveURL(/#\/reader/)
    await expect(page.locator('.chapter-title').first()).toHaveText(/第一章 开始/)
    await expect(page.getByText('第一章正文内容。')).toBeVisible()

    // 顶部/底部「下一章」按钮翻页
    await page.locator('button.next-btn:not([disabled])').first().click()
    await expect(page.locator('.chapter-title').first()).toHaveText(/第二章 中段/)

    // 目录面板跳转第三章
    await page.locator('.sidebar-item', { hasText: '目录' }).first().click()
    await expect(page.getByText('第三章 收尾').first()).toBeVisible()
    await page.getByText('第三章 收尾').first().click()
    await expect(page.locator('.chapter-title').first()).toHaveText(/第三章 收尾/)
    await expect(page.getByText('第三章正文内容。')).toBeVisible()

    // 回到书架（已在 hash 路由内，直接导航即可），进度已记住
    await gotoApp(page, '/')
    await page.locator('.book-card', { hasText: name }).first().click()
    await expect(page.locator('.chapter-title').first()).toHaveText(/第三章 收尾/)
    expect(bookId).toMatch(/^book_\d+$/)
  })
})
