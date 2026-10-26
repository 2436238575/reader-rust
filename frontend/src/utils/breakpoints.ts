/**
 * 全站断点口径。CSS 媒体查询不能用变量，matchMedia 侧统一走这里的常量，
 * CSS 侧的口径说明见 styles/variables.css 顶部注释。
 *
 * - 主断点互补对 `(max-width: 767px)` / `(min-width: 768px)`：
 *   移动 ↔ 桌面分界（顶栏汉堡、整屏菜单、各管理弹层的整屏形态）。
 * - 次级 640px：紧凑手机档（顶栏、书架网格单列、首页/最近/书源管理头部的紧凑化）。
 * - 900px：书源管理页双栏 → 单栏堆叠（平板竖屏需要比手机更宽的分界，保留）。
 * - 420px：超窄小屏的阅读器控件加密（合进 640 会让 360-430px 主力手机都变密，保留）。
 */
export const MOBILE_LAYOUT_QUERY = '(max-width: 767px)'
export const DESKTOP_LAYOUT_QUERY = '(min-width: 768px)'
