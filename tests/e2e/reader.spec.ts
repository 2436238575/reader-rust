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
    const content =
      '第一章 开始\n第一章正文内容。\n第二章 中段\n第二章正文内容。\n第三章 收尾\n第三章正文内容。\n'
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
        (_, i) =>
          `第${i + 1}章 长文\n` +
          Array.from({ length: paragraphsPerChapter }, () => para).join('\n')
      ).join('\n')
      const form = new FormData()
      form.append(
        'file',
        new File([chapters], `长文冒烟-${Date.now()}.txt`, { type: 'text/plain' })
      )
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

  test('chapter edit overlays content, marks catalog, and diff panel renders', async ({ page }) => {
    const { name } = await seedLocalTxtBook(page)
    await openBookFromShelf(page, name)
    // 等第一章正文渲染出来
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()

    // 打开编辑器（设置抽屉 → 编辑本章正文）
    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    await page.locator('.opt-btn', { hasText: '编辑本章正文' }).click()
    const editor = page.locator('.editor-textarea')
    await expect(editor).toBeVisible()
    await editor.fill('第一章正文内容。\n这一整段是新加上去的。\n编辑版独家段落。')
    await page.getByRole('button', { name: '保存修改' }).click()

    // 正文换成编辑版 + diff 标记出现（新增段左绿竖线）
    await expect(page.getByText('这一整段是新加上去的。').first()).toBeVisible()
    await expect(page.locator('p.diff-added').first()).toBeVisible()

    // 保存后设置抽屉还开着，先回正文再点标记
    await page.keyboard.press('Escape')
    // 点 diff 标记就地预览：新增段落文本 + 查看全部进完整面板
    await page.locator('p.diff-added').first().click()
    const preview = page.locator('.diff-preview')
    await expect(preview).toBeVisible()
    await expect(preview).toContainText('新增段落')
    await expect(preview).toContainText('这一整段是新加上去的。')
    await preview.getByRole('button', { name: '查看全部' }).click()
    await expect(page.locator('.diff-modal')).toBeVisible()
    await page.keyboard.press('Escape')

    // 目录里出现「已编辑」徽章
    await page.locator('.sidebar-item', { hasText: '目录' }).first().click()
    await expect(page.locator('.status-badge.edited', { hasText: '已编辑' })).toBeVisible()
    await page.keyboard.press('Escape')

    // diff 面板：设置 → 查看与上游的差异
    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    await page.getByRole('button', { name: '查看与上游的差异' }).click()
    const panel = page.locator('.diff-modal')
    await expect(panel).toBeVisible()
    // 新增段绿底行在面板里
    await expect(
      panel.locator('.diff-row.add', { hasText: '这一整段是新加上去的。' })
    ).toBeVisible()
    // 统计行有 +2 段
    await expect(panel.locator('.stat-add')).toContainText('+2')
    await page.keyboard.press('Escape')

    // 刷新页面：编辑版正文与 diff 标记应直接恢复（浏览器缓存里可能存着上游版）
    await page.reload()
    await expect(page.getByText('这一整段是新加上去的。').first()).toBeVisible()
    await expect(page.locator('p.diff-added').first()).toBeVisible()

    // 编辑器里恢复上游版本
    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    await page.locator('.opt-btn', { hasText: '编辑本章正文' }).click()
    await expect(page.getByText('本章已编辑', { exact: false })).toBeVisible()
    page.on('dialog', (dialog) => void dialog.accept())
    await page.getByRole('button', { name: '恢复上游版本' }).click()

    // 正文回到上游版（第一章原文），编辑内容与标记都消失
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()
    await expect(page.getByText('编辑版独家段落。')).toHaveCount(0)
    await expect(page.locator('p.diff-added')).toHaveCount(0)
  })

  test('content version switch shows upstream original and restores the edit', async ({ page }) => {
    const { name } = await seedLocalTxtBook(page)
    await openBookFromShelf(page, name)
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()

    // 先保存一版编辑
    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    await page.locator('.opt-btn', { hasText: '编辑本章正文' }).click()
    await page.locator('.editor-textarea').fill('编辑版正文内容。')
    await page.getByRole('button', { name: '保存修改' }).click()
    await expect(page.getByText('编辑版正文内容。').first()).toBeVisible()
    await page.keyboard.press('Escape')

    // 切到上游原版：正文换回原文，改动标记不显示
    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    const versionRow = page.locator('.setting-row', { hasText: '正文版本' })
    await versionRow.getByRole('button', { name: '上游原版' }).click()
    await page.keyboard.press('Escape')
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()
    await expect(page.getByText('编辑版正文内容。')).toHaveCount(0)
    await expect(page.locator('p.diff-modified, p.diff-added')).toHaveCount(0)

    // 切回编辑版：编辑内容和标记都回来
    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    await versionRow.getByRole('button', { name: '默认版本' }).click()
    await page.keyboard.press('Escape')
    await expect(page.getByText('编辑版正文内容。').first()).toBeVisible()
    await expect(page.locator('p.diff-modified')).toHaveCount(1)
  })

  test('version history lists the current edited version and previews it', async ({ page }) => {
    const { name } = await seedLocalTxtBook(page)
    await openBookFromShelf(page, name)
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()

    await page.locator('.sidebar-item', { hasText: '设置' }).first().click()
    await page.locator('.opt-btn', { hasText: '编辑本章正文' }).click()
    await page.locator('.editor-textarea').fill('历史面板里的编辑版。')
    await page.getByRole('button', { name: '保存修改' }).click()
    await expect(page.getByText('历史面板里的编辑版。').first()).toBeVisible()
    await page.keyboard.press('Escape')

    // 工具栏「正文历史」：时间线里应有一个编辑版本，且标记为当前
    await page.locator('button[title="正文历史"]').click()
    const modal = page.locator('.version-modal')
    await expect(modal).toBeVisible()
    const row = modal.locator('.version-row', { hasText: '编辑' }).first()
    await expect(row).toBeVisible()
    await expect(row.locator('.badge.current')).toBeVisible()

    // 点版本预览正文
    await row.click()
    await expect(modal.locator('.preview-content')).toContainText('历史面板里的编辑版。')
    await modal.locator('.close-btn').click()
    await expect(modal).toHaveCount(0)
  })

  test('whole-book TXT import previews matches and refreshes adjacent chapters', async ({ page }) => {
    const { name } = await seedLocalTxtBook(page)
    await openBookFromShelf(page, name)
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()

    await page.locator('button[title="正文历史"]').click()
    const modal = page.locator('.version-modal')
    await expect(modal).toBeVisible()
    await modal.getByRole('button', { name: '导入 TXT' }).click()

    const firstText =
      '第一章 开始\n导入后的第一章。\n第二章 中段\n导入后的第二章。\n第三十七章 不存在\n这章不该匹配。'
    await modal.getByRole('textbox', { name: /粘贴整本小说文本/ }).fill(firstText)
    await modal.getByRole('button', { name: '解析并预览' }).click()

    // 预览：两章标题匹配，目录里没有的第三十七章留作未匹配；
    // 新书没有缓存正文，无法对比，默认不勾选并引导去缓存
    await expect(modal.getByText('共 3 章')).toBeVisible()
    await expect(modal.getByText('匹配 2')).toBeVisible()
    await expect(modal.getByText('未匹配 1')).toBeVisible()
    await expect(modal.getByText('未缓存 2')).toBeVisible()
    await expect(modal.getByText('已选 0')).toBeVisible()
    await expect(modal.locator('.cache-hint')).toContainText('还没有')
    await modal.getByRole('button', { name: '全选已匹配' }).click()
    await expect(modal.getByText('已选 2')).toBeVisible()
    await modal.getByRole('button', { name: '导入 2 章' }).click()
    await expect(page.getByText('已导入 2 章')).toBeVisible()

    // 导入后正文直接换成导入内容
    await modal.locator('.close-btn').click()
    await expect(page.getByText('导入后的第一章。').first()).toBeVisible()

    // 相邻章此前可能已被预加载：翻过去也必须看到新正文
    await page.locator('button.next-btn:not([disabled])').first().click()
    await expect(page.getByText('导入后的第二章。').first()).toBeVisible()

    // 再预览同一份文本：两章都与当前正文一致，默认不勾选
    await page.locator('button[title="正文历史"]').click()
    await modal.getByRole('button', { name: '导入 TXT' }).click()
    await modal.getByRole('textbox', { name: /粘贴整本小说文本/ }).fill(firstText)
    await modal.getByRole('button', { name: '解析并预览' }).click()
    await expect(modal.getByText('有差异 0')).toBeVisible()
    await expect(modal.getByText('已选 0')).toBeVisible()

    // 改第一章再预览：只有第一章有差异并被默认勾选，详情直接显示对比
    await modal
      .getByRole('textbox', { name: /粘贴整本小说文本/ })
      .fill(firstText.replace('导入后的第一章。', '再改一次第一章。'))
    await modal.getByRole('button', { name: '解析并预览' }).click()
    await expect(modal.getByText('有差异 1')).toBeVisible()
    await expect(modal.getByText('已选 1')).toBeVisible()
    await expect(modal.locator('.detail-diff .diff-line.add')).toContainText('再改一次第一章。')
  })

  test('mobile long-press on underline opens action menu and deletes it', async ({ page }) => {
    // 移动视口（isMobile 按 767px 断点）；长按靠手动 dispatch TouchEvent，
    // 不需要 context 级 hasTouch
    await page.setViewportSize({ width: 390, height: 844 })
    const { bookId, name } = await seedLocalTxtBook(page)
    // 预置一条划线书签（第一章正文含「第一章正文内容」）
    const saved = await page.evaluate(async (bookId) => {
      const token = localStorage.getItem('accessToken')
      const resp = await fetch('/reader3/saveBookmark', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          bookId,
          bookName: '测试书',
          bookAuthor: '本地导入',
          chapterIndex: 0,
          chapterPos: 1000,
          chapterName: '第一章 开始',
          bookText: '第一章正文内容',
          kind: 'underline',
          time: Date.now(),
        }),
      })
      // selectAction 默认 ignore（只读）；切到 popup 让划线可交互
      const cfg = JSON.parse(localStorage.getItem('readConfig') || '{}')
      cfg.selectAction = 'popup'
      localStorage.setItem('readConfig', JSON.stringify(cfg))
      return resp.ok
    }, bookId)
    expect(saved).toBe(true)
    await openBookFromShelf(page, name)

    const underline = page.locator('span.reader-underline', { hasText: '第一章正文内容' })
    await expect(underline).toHaveCount(1)

    // 长按：touchstart 后按住 700ms（菜单 500ms 触发），再抬手
    await underline.evaluate((el) => {
      const r = el.getBoundingClientRect()
      const touch = new Touch({
        identifier: 1,
        target: el,
        clientX: r.x + 5,
        clientY: r.y + 5,
      })
      el.dispatchEvent(
        new TouchEvent('touchstart', { touches: [touch], bubbles: true, cancelable: true })
      )
    })
    await page.waitForTimeout(700)
    await underline.evaluate((el) => {
      el.dispatchEvent(new TouchEvent('touchend', { bubbles: true, cancelable: true }))
    })

    const menu = page.locator('.selection-menu')
    await expect(menu).toBeVisible()
    await expect(menu.getByRole('button', { name: '删除划线' })).toBeVisible()
    await expect(menu.getByRole('button', { name: '加入高亮' })).toBeVisible()

    page.on('dialog', (dialog) => void dialog.accept())
    await menu.getByRole('button', { name: '删除划线' }).click()
    await expect(underline).toHaveCount(0)
  })

  test('chapter bookmark add/list/note/delete in catalog panel', async ({ page }) => {
    const { name } = await seedLocalTxtBook(page)
    await openBookFromShelf(page, name)
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()

    // 目录面板 → 书签 tab → 添加当前页书签
    await page.locator('.sidebar-item', { hasText: '目录' }).first().click()
    await page.locator('.tab', { hasText: '书签' }).click()
    await page.getByRole('button', { name: '添加当前页书签' }).click()
    const bookmarkItem = page.locator('.bookmark-item', { hasText: '第一章 开始' })
    await expect(bookmarkItem).toBeVisible()

    // 备注：hover 出操作区 → 编辑 → 保存 → 展示
    await bookmarkItem.hover()
    await bookmarkItem.locator('.bm-edit').click()
    await page.locator('.bm-note-editor textarea').fill('这一段值得回味')
    await page.getByRole('button', { name: '保存' }).click()
    await expect(page.locator('.bm-note', { hasText: '这一段值得回味' })).toBeVisible()

    // 删除（confirm 自动接）
    page.on('dialog', (dialog) => void dialog.accept())
    await bookmarkItem.hover()
    await bookmarkItem.locator('.bm-delete').click()
    await expect(page.locator('.bookmark-item')).toHaveCount(0)
  })

  test('book-wide search on local book reports empty coverage', async ({ page }) => {
    const { name } = await seedLocalTxtBook(page)
    await openBookFromShelf(page, name)
    await expect(page.getByText('第一章正文内容。').first()).toBeVisible()

    // 搜索面板（工具栏搜索按钮）→ 切「全书」（本地书不进缓存检索，应如实报覆盖度 0）
    await page.locator('button.tb-btn[title="搜索"]').click()
    const panel = page.locator('.reader-search-panel')
    await expect(panel).toBeVisible()
    await panel.getByRole('button', { name: '全书' }).click()
    await panel.locator('input').fill('古庙')
    await panel.getByRole('button', { name: '搜索', exact: true }).click()
    await expect(panel.locator('.search-status-text')).toContainText('没有已缓存的章节可供检索')
  })
})
