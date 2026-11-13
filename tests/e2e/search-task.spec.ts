import { test, expect } from '@playwright/test'
import { gotoApp, loginViaPage, triggerGlobalSearch } from './support/app'
import { hasLogin, readE2EConfig } from './support/env'

const config = readE2EConfig()

/**
 * 搜索页任务模型接线：网络层用 Playwright 拦截成固定的任务/SSE/结果，
 * 验证前端确实走 `POST /v1/searches` + `/v1/tasks/{id}/events`，
 * 不再请求 `/reader3/searchBookMultiSSE`。
 */
test.describe('Standard E2E: search task model', () => {
  test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable search task regression.')

  test.beforeEach(async ({ page }) => {
    test.skip(!hasLogin(config), 'Set E2E_USERNAME and E2E_PASSWORD to enable search task regression.')
    await loginViaPage(page, config.username!, config.password!)
    await expect(page).toHaveURL(/#\//)
  })

  test('search consumes task events and renders result items', async ({ page }) => {
    const book = {
      name: '任务结果书',
      author: '任务作者',
      bookUrl: 'https://task.example/book/1',
      origin: 'https://task.example',
      bookId: 'book_1',
      bookSourceId: 1,
      intro: '任务模型返回的搜索结果',
      kind: '玄幻',
    }
    let legacySearchRequests = 0
    page.on('request', (request) => {
      if (request.url().includes('/reader3/searchBookMultiSSE')) legacySearchRequests += 1
    })

    // 预置两个书源（网络层 mock）：搜索范围默认「单个书源」，没有源时组件
    // 会停在条件不完整的早退分支（与生产首启无源一致）
    await page.route('**/v1/sources?limit=200', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          items: [
            {
              id: 'src_1',
              name: '任务源甲',
              url: 'https://task-source-a.example',
              group: null,
              enabled: true,
              enabledExplore: true,
              enabledCookieJar: true,
              customOrder: null,
              type: null,
              weight: null,
              respondTime: null,
              exploreUrl: null,
              updatedAt: null,
            },
            {
              id: 'src_2',
              name: '任务源乙',
              url: 'https://task-source-b.example',
              group: null,
              enabled: true,
              enabledExplore: true,
              enabledCookieJar: true,
              customOrder: null,
              type: null,
              weight: null,
              respondTime: null,
              exploreUrl: null,
              updatedAt: null,
            },
          ],
          nextCursor: null,
          total: 2,
        }),
      })
    )

    await page.route('**/v1/searches', (route) =>
      route.fulfill({
        status: 202,
        contentType: 'application/json',
        body: JSON.stringify({
          id: 'tsk_1',
          kind: 'search',
          state: 'queued',
          progress: { done: 0, total: 1, unit: 'source', message: null },
          createdAt: '2026-11-14T00:00:00Z',
          startedAt: null,
          finishedAt: null,
          resultHref: null,
          error: null,
        }),
      })
    )
    await page.route('**/v1/tasks/tsk_1/events*', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'text/event-stream',
        body:
          `id: 1\nevent: progress\ndata: {"done":0,"total":1,"unit":"source","message":"正在搜索书源"}\n\n` +
          `id: 2\nevent: item\ndata: {"items":[${JSON.stringify(book)}],"cursor":"c1"}\n\n` +
          `id: 3\nevent: done\ndata: {"state":"succeeded","resultHref":"/v1/tasks/tsk_1/result"}\n\n`,
      })
    )
    await page.route('**/v1/tasks/tsk_1/result*', (route) =>
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ items: [book], nextCursor: null, total: 1, truncated: false }),
      })
    )

    await gotoApp(page, '/')
    await triggerGlobalSearch(page, '任务')

    const card = page.locator('.book-card').filter({ hasText: book.name }).first()
    await expect(card).toBeVisible()
    // 收尾后不再显示「搜索中」
    await expect(page.locator('.searching-indicator')).toHaveCount(0)
    expect(legacySearchRequests, '搜索不应再请求 /reader3/searchBookMultiSSE').toBe(0)
  })
})
