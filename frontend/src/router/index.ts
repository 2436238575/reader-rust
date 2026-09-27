import { createRouter, createWebHashHistory } from 'vue-router'

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
  ],
})

export default router
