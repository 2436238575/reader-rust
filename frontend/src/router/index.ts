import { createRouter, createWebHashHistory } from 'vue-router'
import { readAccessToken } from '../utils/secureAccess'

const router = createRouter({
  // hash 路由本身不依赖服务端回落，但基址要跟着部署前缀走
  history: createWebHashHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: () => import('../views/HomeView.vue'),
    },
    {
      path: '/search',
      name: 'search',
      component: () => import('../views/SearchView.vue'),
    },
    {
      path: '/login',
      name: 'login',
      component: () => import('../views/LoginView.vue'),
    },
    {
      path: '/reader',
      name: 'reader',
      component: () => import('../views/ReaderView.vue'),
    },
    {
      path: '/ai-book',
      name: 'ai-book',
      component: () => import('../views/AiBookView.vue'),
    },
    {
      path: '/explore',
      name: 'explore',
      component: () => import('../views/ExploreView.vue'),
    },
    {
      path: '/recent',
      name: 'recent',
      component: () => import('../views/RecentView.vue'),
    },
    {
      path: '/sources',
      name: 'sources',
      component: () => import('../views/SourcesView.vue'),
    },
    {
      path: '/webdav',
      name: 'webdav',
      component: () => import('../views/WebdavView.vue'),
    },
  ],
})

// 无令牌直达任意页面都会被 API 401 兜底，但体验上是白屏转圈后才弹登录框；
// 路由守卫提前拦到 /login。登录页自身放行。
router.beforeEach((to) => {
  if (to.name === 'login') return true
  if (!readAccessToken(localStorage)) {
    return { name: 'login', query: { redirect: to.fullPath } }
  }
  return true
})

export default router
