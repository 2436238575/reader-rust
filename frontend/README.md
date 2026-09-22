# frontend

Reader-Rust 的 Web 前端：Vue 3 + TypeScript + Vite + Pinia。

```bash
npm install
npm run dev        # 开发服务器，默认 http://localhost:5173，/reader3 已代理到后端
npm run build      # vue-tsc 类型检查 + 构建，输出到 dist/
npm run preview    # 预览构建产物
npm test           # vitest 单元测试
```

可用脚本只有以上四个，**没有 `lint` 和 `serve`**。

构建产物 `dist/` 由后端直接托管（后端默认 `WEB_ROOT=frontend/dist`）。开发服务器把 `/reader3` 反向代理到后端，代理目标端口需与后端 `SERVER_PORT` 一致（见 `vite.config.ts`）。

## 目录概览

```
src/
  views/        8 个路由页（Home / Reader / Explore / Recent / Rss / RssArticle / RssManage / AiBook）
  components/   31 个组件，含 reader/ 与 source-manager/ 子目录
  stores/       Pinia：reader / bookshelf / explore / source / rss / aiBook / app
  api/          14 个接口模块 + http.ts（axios 实例，baseURL=/reader3，自动注入凭证并拆包响应）
  utils/        PWA、简繁转换、TTS、加密工具
public/         PWA 资源（sw.js、site.webmanifest、offline.html）与图标
```

## 约定

- 后端响应统一为 `{ isSuccess, errorMsg, data }`，`http.ts` 的响应拦截器会自动拆掉外壳，业务代码直接拿 `data`
- 遇到 `errorMsg === 'NEED_LOGIN'` 或 HTTP 401 时派发 `need-login` 事件拉起登录框
- 修改代码后请跑 `npm run build`，类型错误会中断构建
- 与被测模块同目录放 `*.test.ts`（vitest）

工程全貌、接口清单与开发约定见仓库根目录的 `AGENTS.md`，前端架构说明见 `docs/maintainers/architecture.md`。
