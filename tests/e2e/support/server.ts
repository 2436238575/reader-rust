/**
 * e2e 的零配置后端夹具：
 * - 显式给了 E2E_USERNAME/E2E_PASSWORD：照旧用外部实例（PLAYWRIGHT_BASE_URL）。
 * - 没给：globalSetup 自起一个临时后端（target/debug 里的构建产物 +
 *   独立临时 storage + 固定凭据 reader1/password123 + 豁免限速），
 *   并把凭据写进 process.env（globalSetup 与 spec 同进程，readE2EConfig 读得到）。
 *
 * 自起端口固定为 18081（与 playwright.config.ts 的兜底 baseURL 一致）；
 * 被占用时报错，请改用显式凭据指向自己的实例。
 */

import { spawn, type ChildProcess } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const FIXTURE_PORT = 18081
const FIXTURE_USERNAME = 'reader1'
const FIXTURE_PASSWORD = 'password123'

let serverProcess: ChildProcess | null = null
let serverTempDir = ''

function serverExePath(): string {
  // playwright 从仓库根目录跑，cwd 即项目根
  const root = process.cwd()
  const exe = join(root, 'target', 'debug', 'reader-rust.exe')
  if (existsSync(exe)) return exe
  return join(root, 'target', 'debug', 'reader-rust')
}

async function waitForServer(url: string, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(`${url}/reader3/getUserInfo`)
      if (resp.status === 200 || resp.status === 401) return true
    } catch {
      // 还没起来
    }
    await new Promise((resolve) => setTimeout(resolve, 400))
  }
  return false
}

/** globalSetup：无显式凭据时自起临时后端。 */
export async function startServerIfNeeded(): Promise<void> {
  if (process.env.E2E_USERNAME && process.env.E2E_PASSWORD) return
  if (process.env.PLAYWRIGHT_BASE_URL) {
    throw new Error(
      'PLAYWRIGHT_BASE_URL 指向外部实例时必须同时给 E2E_USERNAME/E2E_PASSWORD；' +
        '想零配置自起临时后端就去掉 PLAYWRIGHT_BASE_URL'
    )
  }
  const exe = serverExePath()
  if (!existsSync(exe)) {
    throw new Error(
      'e2e 无凭据时需要先 `cargo build` 出 target/debug/reader-rust.exe，' +
        '或设 E2E_USERNAME/E2E_PASSWORD 指向自己的实例'
    )
  }
  serverTempDir = mkdtempSync(join(tmpdir(), 'reader-e2e-'))
  serverProcess = spawn(exe, [], {
    env: {
      ...process.env,
      SERVER_HOST: '127.0.0.1',
      SERVER_PORT: String(FIXTURE_PORT),
      STORAGE_DIR: serverTempDir,
      ADMIN_USERNAME: FIXTURE_USERNAME,
      ADMIN_PASSWORD: FIXTURE_PASSWORD,
      RATE_LIMIT_DISABLED: 'true',
      LOG_LEVEL: 'warn',
    },
    stdio: 'ignore',
  })
  const ok = await waitForServer(`http://127.0.0.1:${FIXTURE_PORT}`, 15_000)
  if (!ok) {
    serverProcess.kill()
    throw new Error(`临时后端在 ${FIXTURE_PORT} 没能起来（端口被占？）`)
  }
  process.env.E2E_USERNAME = FIXTURE_USERNAME
  process.env.E2E_PASSWORD = FIXTURE_PASSWORD
}

/** globalTeardown：杀进程 + 清临时 storage。 */
export async function stopServerIfStarted(): Promise<void> {
  if (serverProcess) {
    serverProcess.kill()
    serverProcess = null
  }
  if (serverTempDir) {
    rmSync(serverTempDir, { recursive: true, force: true })
    serverTempDir = ''
  }
}
