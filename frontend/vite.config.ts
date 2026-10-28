import { defineConfig, loadEnv } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'
import pkg from './package.json' with { type: 'json' }

/// 部署前缀：根路径部署留空即可；用 nginx 反代到 `/read/` 这类子路径时设
/// `VITE_BASE_PATH=/read/`（构建产物里的资源与接口地址都会带上前缀）。
function normalizeBase(value?: string) {
  const trimmed = (value || '').trim()
  if (!trimmed || trimmed === '/') return '/'
  return `/${trimmed.replace(/^\/+|\/+$/g, '')}/`
}

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const base = normalizeBase(env.VITE_BASE_PATH)
  // 代理键要跟着 base 走：子路径部署时前端请求的是 `/read/reader3/...`；
  // 转发前剥掉前缀，与文档里的 nginx 写法（`proxy_pass http://后端/;`）保持一致
  const proxy = {
    [`${base}reader3`]: {
      target: 'http://127.0.0.1:8080',
      changeOrigin: true,
      rewrite: (path: string) =>
        base === '/' || !path.startsWith(base) ? path : `/${path.slice(base.length)}`,
    },
  }

  return {
    base,
    plugins: [vue()],
    define: {
      __APP_VERSION__: JSON.stringify(`v${pkg.version}`),
    },
    resolve: {
      alias: {
        '@': resolve(import.meta.dirname, 'src'),
      },
    },
    server: {
      port: 5173,
      proxy,
    },
    // `vite preview` 也带代理，方便本地验证子路径部署
    preview: {
      proxy,
    },
  }
})
