import { defineConfig, devices } from '@playwright/test'

// 外部实例地址；不给时由 globalSetup 在 18081 自起临时后端（见 support/server.ts）
const baseURL = process.env.PLAYWRIGHT_BASE_URL || 'http://127.0.0.1:18081'

export default defineConfig({
  testDir: './tests/e2e',
  timeout: 60_000,
  workers: 1,
  expect: {
    timeout: 10_000,
  },
  fullyParallel: false,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [['html', { open: 'never' }], ['list']] : [['list']],
  // 无 E2E_USERNAME/E2E_PASSWORD 时自起临时后端（构建产物 + 独立 storage + 固定凭据）
  globalSetup: './tests/e2e/support/global-setup.ts',
  globalTeardown: './tests/e2e/support/global-teardown.ts',
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    viewport: { width: 1440, height: 960 },
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        channel: 'chrome',
        launchOptions: {
          args: ['--disable-crash-reporter', '--disable-crashpad', '--disable-breakpad'],
        },
      },
    },
  ],
})
