/**
 * e2e 的零配置后端夹具：
 * - 显式给了 E2E_USERNAME/E2E_PASSWORD：照旧用外部实例（PLAYWRIGHT_BASE_URL）。
 * - 没给：globalSetup 自起一个临时后端（target/debug 里的构建产物，缺失时
 *   先跑一次 cargo build + 独立临时 storage/数据库/assets + 固定凭据
 *   reader1/password123 + 豁免限速），并把凭据写进 process.env。
 *
 * 自起端口固定为 18081（与 playwright.config.ts 的兜底 baseURL 一致）。
 * 三条防呆：启动前探测端口占用（避免连到别的实例）、就绪探测用固定凭据
 * 实际登录（200 且拿到令牌才算自己）、子进程提前退出立即报错并附 stderr。
 */

import { spawn, spawnSync, type ChildProcess } from 'node:child_process'
import { createConnection } from 'node:net'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { isAbsolute, join } from 'node:path'

const FIXTURE_PORT = 18081
const FIXTURE_USERNAME = 'reader1'
const FIXTURE_PASSWORD = 'password123'
/** 等待后端起好的上限：首次跑没构建产物时会先 cargo build，这里只量启动。 */
const STARTUP_TIMEOUT_MS = 30_000
const KILL_TIMEOUT_MS = 5_000

let serverProcess: ChildProcess | null = null
let serverTempDir = ''
let serverStderr: string[] = []

function serverExePath(): string {
  // 本机可能正有一个 `cargo run` 占着 target/debug 的 exe（Windows 会拒绝覆盖），
  // 用 E2E_SERVER_BIN 指向独立 target 目录里的同名产物即可继续跑 e2e。
  const override = process.env.E2E_SERVER_BIN
  if (override) {
    const resolved = isAbsolute(override) ? override : join(process.cwd(), override)
    if (!existsSync(resolved)) {
      throw new Error(`E2E_SERVER_BIN 指向的二进制不存在: ${resolved}`)
    }
    return resolved
  }
  // playwright 从仓库根目录跑，cwd 即项目根
  const root = process.cwd()
  const exe = join(root, 'target', 'debug', 'reader-rust.exe')
  if (existsSync(exe)) return exe
  return join(root, 'target', 'debug', 'reader-rust')
}

/** 构建产物缺失时先构建（CI/新克隆直接跑 e2e 不再报「文件不存在」）。 */
function ensureServerBuilt(): string {
  if (existsSync(serverExePath())) return serverExePath()
  // 输出直通终端：编译失败时用户能直接看到 cargo 的错误
  const result = spawnSync('cargo', ['build'], {
    cwd: process.cwd(),
    stdio: 'inherit',
    shell: process.platform === 'win32',
  })
  if (result.status !== 0 || !existsSync(serverExePath())) {
    throw new Error(
      'e2e 自起实例需要后端构建产物：`cargo build` 执行失败（见上方输出），' +
        '或设 E2E_USERNAME/E2E_PASSWORD 指向已有实例'
    )
  }
  return serverExePath()
}

function portIsOccupied(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = createConnection({ host: '127.0.0.1', port })
    const done = (occupied: boolean) => {
      socket.destroy()
      resolve(occupied)
    }
    socket.once('connect', () => done(true))
    socket.once('error', () => done(false))
    // 极端情况下回环连接也不通不报：按未占用处理
    socket.setTimeout(1_000, () => done(false))
  })
}

function stderrTail(): string {
  return serverStderr.join('').trim().split('\n').slice(-10).join('\n')
}

/**
 * 就绪探测：用固定凭据实际登录。
 * 只探 200/401 会把「端口上恰好有别的实例」当成自己起好了——固定凭据
 * 能成功登录才证明这是刚 bootstrap 出来的夹具实例。
 */
async function waitForFixture(url: string, child: ChildProcess): Promise<boolean> {
  const deadline = Date.now() + STARTUP_TIMEOUT_MS
  while (Date.now() < deadline) {
    // 子进程提前退出：端口被抢/启动失败，马上失败而不是空等
    if (child.exitCode !== null) return false
    try {
      const resp = await fetch(`${url}/reader3/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: FIXTURE_USERNAME, password: FIXTURE_PASSWORD }),
      })
      if (resp.status === 200) {
        const payload = (await resp.json()) as {
          data?: { accessToken?: string; refreshToken?: string }
        }
        // 双令牌都拿到才说明是自己刚 bootstrap 的新后端（旧产物没有 refreshToken）
        if (payload?.data?.accessToken && payload?.data?.refreshToken) return true
      }
    } catch {
      // 还没起来
    }
    await new Promise((resolve) => setTimeout(resolve, 300))
  }
  return false
}

function waitForExit(child: ChildProcess, timeoutMs: number): Promise<void> {
  return new Promise((resolve) => {
    if (child.exitCode !== null) return resolve()
    const timer = setTimeout(resolve, timeoutMs)
    child.once('exit', () => {
      clearTimeout(timer)
      resolve()
    })
  })
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
  const exe = ensureServerBuilt()

  // 端口被占时明确报错：否则客户端的请求会静默打到别人的实例上
  if (await portIsOccupied(FIXTURE_PORT)) {
    throw new Error(
      `端口 ${FIXTURE_PORT} 已被占用（可能是另一个实例），无法自起临时后端；` +
        '请停掉占用进程，或设 E2E_USERNAME/E2E_PASSWORD + PLAYWRIGHT_BASE_URL 指向它'
    )
  }

  serverTempDir = mkdtempSync(join(tmpdir(), 'reader-e2e-'))
  serverStderr = []
  serverProcess = spawn(exe, [], {
    env: {
      ...process.env,
      SERVER_HOST: '127.0.0.1',
      SERVER_PORT: String(FIXTURE_PORT),
      // 运行期数据全部收进临时目录：STORAGE_DIR 只管缓存/日志/数据，
      // DATABASE_URL 与 ASSETS_DIR 的默认值仍指向仓库 storage/，不设就会写 dev 库
      STORAGE_DIR: serverTempDir,
      DATABASE_URL: `sqlite:${join(serverTempDir, 'reader.db')}?mode=rwc`,
      ASSETS_DIR: join(serverTempDir, 'assets'),
      ADMIN_USERNAME: FIXTURE_USERNAME,
      ADMIN_PASSWORD: FIXTURE_PASSWORD,
      RATE_LIMIT_DISABLED: 'true',
      LOG_LEVEL: 'warn',
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  // 收 stderr 尾巴：启动失败时错误信息里带上真实原因（此前 stdio: ignore 全吞）
  serverProcess.stderr?.on('data', (chunk: Buffer) => {
    serverStderr.push(chunk.toString())
    if (serverStderr.length > 50) serverStderr.shift()
  })

  const url = `http://127.0.0.1:${FIXTURE_PORT}`
  const ok = await waitForFixture(url, serverProcess)
  if (!ok) {
    const tail = stderrTail()
    await stopServerIfStarted()
    throw new Error(
      `临时后端在 ${FIXTURE_PORT} 没能起来（端口被占，或构建产物过旧不支持双令牌？）` +
        (tail ? `\n后端 stderr 尾部：\n${tail}` : '\n后端没有 stderr 输出')
    )
  }
  process.env.E2E_USERNAME = FIXTURE_USERNAME
  process.env.E2E_PASSWORD = FIXTURE_PASSWORD
}

/** globalTeardown：杀进程等它退出，再清临时 storage（Windows 下未退出时删目录会 EBUSY）。 */
export async function stopServerIfStarted(): Promise<void> {
  if (serverProcess) {
    const child = serverProcess
    serverProcess = null
    child.kill()
    await waitForExit(child, KILL_TIMEOUT_MS)
  }
  if (serverTempDir) {
    rmSync(serverTempDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 })
    serverTempDir = ''
  }
}
