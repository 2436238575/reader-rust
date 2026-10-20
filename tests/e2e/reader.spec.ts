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

/** 上传一本长文多章的 TXT：横翻需要一章撑出多页，连续滚动裁剪需要多章堆叠。 */
async function seedLongBook(page, chapterCount: number, paragraphsPerChapter: number) {
  await page.waitForFunction(() => !!localStorage.getItem('accessToken'))
  const seed = await page.evaluate(
    async ({ chapterCount, paragraphsPerChapter }) => {
      const token = localStorage.getItem('accessToken')
      const para =
        '这一章的正文段落足够长，用来撑满阅读器的页面高度。书页上的字迹清晰，内容连续不断，'.repeat(
          4
        )
      const chapters = Array.from(
        { length: chapterCount },
        (_, i) => `第${i + 1}章 长文\n` + Array.from({ length: paragraphsPerChapter }, () => para).join('\n')
      ).join('\n')
      const form = new FormData()
      form.append('file', new File([chapters], `长文冒烟-${Date.now()}.txt`, { type: 'text/plain' }))
      const resp = await fetch('/reader3/uploadTxtBook', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
        body: form,
      })
      if (!resp.ok) return null
      const payload = await resp.json()
      return { bookId: payload?.data?.bookId ?? null, name: payload?.data?.name ?? '' }
    },
    { chapterCount, paragraphsPerChapter }
  )
  expect(seed.bookId, 'TXT 上传应返回 bookId').toBeTruthy()
  return seed
}

/** 从书架打开书（登录落地页即书架，同 URL 不重抓，先 reload 带上新上传的书）。 */
async function openBookFromShelf(page, name: string) {
  await page.reload()
  await expect(page.locator('[data-v-app]')).toBeVisible()
  const card = page.locator('.book-card', { hasText: name }).first()
  await expect(card).toBeVisible()
  await card.click()
  await expect(page).toHaveURL(/#\/reader/)
}

/** 阅读器设置抽屉里切换翻页方式。 */
async function switchReadMethod(page, method: '左右翻页' | '上下滚动' | '上下滑动') {
  await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
  await page.getByRole('button', { name: method, exact: true }).click()
  // 关抽屉回正文
  await page.keyboard.press('Escape')
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

  test('horizontal paging keeps page across search panel and mode roundtrip', async ({ page }) => {
    // 长文第一章要撑出多页横翻
    const { name } = await seedLongBook(page, 3, 60)
    await openBookFromShelf(page, name)
    await expect(page.locator('.chapter-title').first()).toHaveText(/第1章 长文/)

    // 切到横翻：出现横翻布局且分页数 > 1
    await switchReadMethod(page, '左右翻页')
    await expect(page.locator('.horizontal-page').first()).toBeVisible()
    await expect
      .poll(async () => page.locator('.horizontal-page').count(), { timeout: 10_000 })
      .toBeGreaterThan(1)

    // 翻到下一页：transform 变化（页码前进的直接证据）
    // 只比位移：transition-duration 会随瞬时跳跃标记在 120ms 后复原，不属于页码语义
    const transformOf = async () => {
      const style = await page.locator('.horizontal-pages').getAttribute('style')
      return style?.match(/translate3d\((-?[\d.]+)px/)?.[1] ?? style
    }
    const before = await transformOf()
    await page.keyboard.press('PageDown')
    await expect.poll(async () => transformOf(), { timeout: 5_000 }).not.toBe(before)
    const pagedTransform = await transformOf()

    // 开合搜索面板（不输入）：页码不动（回归锚点：曾仅开合面板就丢回第 1 页）
    await page.locator('.tb-btn[title="搜索"]').click()
    const searchInput = page.locator('input[placeholder="搜索当前章节内容..."]')
    await expect(searchInput).toBeVisible()
    await expect.poll(async () => transformOf()).toBe(pagedTransform)
    // 输入无匹配关键词：不触发定位，页码也不动
    await searchInput.pressSequentially('zxqwv', { delay: 30 })
    await expect.poll(async () => transformOf()).toBe(pagedTransform)
    // 输入有匹配关键词：搜索会定位到首个命中所在页（这是功能不是 bug）
    await searchInput.fill('正文')
    await expect.poll(async () => transformOf(), { timeout: 5_000 }).not.toBe(pagedTransform)
    // 关闭面板：ESC 关搜索（键盘关闭链回归锚点），停在命中页不归零
    await page.keyboard.press('Escape')
    await expect(searchInput).toBeHidden()
    const locatedTransform = await transformOf()
    await expect.poll(async () => transformOf()).toBe(locatedTransform)

    // 切回竖滚：位置不丢，仍是第一章且正文可见
    await switchReadMethod(page, '上下滑动')
    await expect(page.locator('.chapter-title').first()).toHaveText(/第1章 长文/)
    await expect(page.getByText('这一章的正文段落足够长').first()).toBeVisible()
  })

  test('continuous scrolling prunes upper chapters and rehydrates on scroll back', async ({
    page,
  }) => {
    // 8 章长文：向下连读后上方章节应被裁成等高占位，回滚顶部时回载
    const { name } = await seedLongBook(page, 8, 40)
    await openBookFromShelf(page, name)
    await expect(page.locator('.chapter-title').first()).toHaveText(/第1章 长文/)

    await switchReadMethod(page, '上下滚动')
    await expect(page.locator('.continuous-chapter').first()).toBeVisible()

    // 逐步把容器滚到底部触发连读加载，直到第 6 章出现（index 5）
    const scroll = page.locator('.reader-scroll-container')
    await expect
      .poll(
        async () => {
          await scroll.evaluate((el) => {
            el.scrollTop = el.scrollHeight
          })
          await page.waitForTimeout(300)
          return page.locator('.continuous-chapter').count()
        },
        { timeout: 30_000, intervals: [400, 800, 1200] }
      )
      .toBeGreaterThanOrEqual(6)

    // 继续往下读让活动章越过缓冲：第 1 章（index 0）应被裁成占位
    await expect
      .poll(
        async () => {
          await scroll.evaluate((el) => {
            el.scrollTop = el.scrollHeight
          })
          await page.waitForTimeout(300)
          return page
            .locator('.continuous-chapter[data-chapter-index="0"] .chapter-placeholder')
            .count()
        },
        { timeout: 15_000, intervals: [400, 800, 1200] }
      )
      .toBe(1)

    // 占位章保留了裁剪前的渲染高度（min-height）：总滚动高度不因裁剪塌陷
    const placeholderHeight = await page
      .locator('.continuous-chapter[data-chapter-index="0"]')
      .evaluate((el) => el.getBoundingClientRect().height)
    expect(placeholderHeight).toBeGreaterThan(1000)

    // 回滚到占位章附近（小幅步进：连续模式的位置系统会把大步瞬移拉回已存进度，
    // 这里模拟真实回滚）：占位进入锚线后回载出真实内容
    const section1 = page.locator('.continuous-chapter[data-chapter-index="1"]')
    await expect
      .poll(
        async () => {
          await section1.evaluate((el) => {
            const container = el.closest('.reader-scroll-container') as HTMLElement
            container.scrollTop = Math.max(0, el.offsetTop + 50)
          })
          await page.waitForTimeout(400)
          return section1.locator('.chapter-text').count()
        },
        { timeout: 20_000, intervals: [500, 800, 1200] }
      )
      .toBe(1)
    await expect(section1.getByText('这一章的正文段落足够长').first()).toBeVisible()
  })
})
