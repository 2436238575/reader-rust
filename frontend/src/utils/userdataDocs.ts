/**
 * 后端 userdata 文档名的统一出口（读 GET /v1/me/userdata:batch；
 * 写 PUT /v1/me/userdata:batch，但阅读统计/已读标记已不再写）。
 * 常量收在不依赖 store 的独立模块里，避免 app.ts 直接 import store 造成循环依赖。
 */

/** 最近阅读（utils/recentBooks） */
export const RECENT_BOOKS = 'recent-books'
/** 旧版已读标记文档：只读兼容，前端不再写入（新统计走 /v1/me/read-events:batch） */
export const READ_HISTORY = 'readChapterHistory'
