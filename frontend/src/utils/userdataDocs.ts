/**
 * 后端 userdata 文档名（/reader3/getUserdata・saveUserdata）的统一出口。
 * 首屏三份文档由 stores/app.ts 在登录后批量预读（api/userdata 的 prefetchUserdata）；
 * 常量收在不依赖 store 的独立模块里，避免 app.ts 直接 import store 造成循环依赖。
 */

/** 阅读统计（utils/readingStats） */
export const READING_STATS = 'reading-stats'
/** 最近阅读（utils/recentBooks） */
export const RECENT_BOOKS = 'recent-books'
/** 已读标记（stores/reader 的 readChapterKeys 跨浏览器同步） */
export const READ_HISTORY = 'readChapterHistory'
