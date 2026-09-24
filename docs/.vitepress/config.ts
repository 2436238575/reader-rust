import { defineConfig } from 'vitepress'

export default defineConfig({
  base: '/reader-rust/',
  title: 'Reader-Rust',
  description: '阅读3.0 - Rust 版书源阅读服务器',
  lang: 'zh-CN',

  themeConfig: {
    nav: [
      { text: '指南', link: '/guide/' },
      { text: 'API', link: '/api/' },
      { text: '书源', link: '/book-source/' },
      { text: '参考', link: '/reference/' },
      { text: '维护者', link: '/maintainers/' },
      { text: 'GitHub', link: 'https://github.com/givenge/reader-rust' }
    ],

    sidebar: {
      '/guide/': [
        {
          text: '入门',
          items: [
            { text: '简介', link: '/guide/' },
            { text: '快速开始', link: '/guide/quickstart' },
            { text: '配置', link: '/guide/configuration' },
            { text: '功能特性', link: '/guide/features' }
          ]
        },
        {
          text: '使用',
          items: [
            { text: '用户手册', link: '/guide/user-manual' },
            { text: 'AI 资料', link: '/guide/ai-book' }
          ]
        },
        {
          text: '部署',
          items: [
            { text: 'Docker 部署', link: '/guide/docker' },
            { text: '手动部署', link: '/guide/manual-deploy' }
          ]
        },
        {
          text: '质量',
          items: [{ text: '测试流程', link: '/guide/testing' }]
        }
      ],
      '/api/': [
        {
          text: 'API 参考',
          items: [
            { text: '概述', link: '/api/' },
            { text: '书源管理', link: '/api/book-source' },
            { text: '书籍搜索', link: '/api/search' },
            { text: '章节内容', link: '/api/chapter' },
            { text: '评论（章评/段评）', link: '/api/review' },
            { text: '缓存管理', link: '/api/cache' },
            { text: '书架分组', link: '/api/book-group' },
            { text: '书签', link: '/api/bookmark' },
            { text: '替换规则', link: '/api/replace-rule' },
            { text: '用户管理', link: '/api/user' },
            { text: 'RSS 订阅', link: '/api/rss' },
            { text: 'WebDAV', link: '/api/webdav' },
            { text: 'AI', link: '/api/ai' }
          ]
        }
      ],
      '/book-source/': [
        {
          text: '书源开发',
          items: [
            { text: '概述', link: '/book-source/' },
            { text: '规则语法', link: '/book-source/rules' },
            { text: '搜索规则', link: '/book-source/search-rule' },
            { text: '书籍信息', link: '/book-source/book-info' },
            { text: '目录规则', link: '/book-source/toc-rule' },
            { text: '正文规则', link: '/book-source/content-rule' },
            { text: 'JavaScript', link: '/book-source/javascript' }
          ]
        }
      ],
      '/reference/': [
        {
          text: '参考规格',
          items: [
            { text: '概述', link: '/reference/' },
            { text: '书源规则兼容规格', link: '/reference/book-source-rules' }
          ]
        }
      ],
      '/maintainers/': [
        {
          text: '维护者',
          items: [
            { text: '概述', link: '/maintainers/' },
            { text: '架构说明', link: '/maintainers/architecture' },
            { text: '发布流程', link: '/maintainers/release' },
            { text: '开发约定', link: '/maintainers/development' }
          ]
        }
      ],
      '/archive/': [
        {
          text: '归档文档',
          items: [
            { text: '说明', link: '/archive/' },
            { text: '书源管理重构设计', link: '/archive/specs/2026-05-11-source-manager-refactor-design' },
            { text: '本地 TXT 导入设计', link: '/archive/specs/2026-06-08-local-txt-books-design' },
            { text: '书源管理重构计划', link: '/archive/plans/2026-05-11-source-manager-refactor' },
            { text: '前端开发计划', link: '/archive/frontend/frontend-development-plan' }
          ]
        }
      ]
    },

    search: {
      provider: 'local'
    },

    outline: { label: '本页目录', level: [2, 3] },

    docFooter: { prev: '上一篇', next: '下一篇' },

    darkModeSwitchLabel: '主题',
    lightModeSwitchTitle: '切换到亮色模式',
    darkModeSwitchTitle: '切换到暗色模式',
    sidebarMenuLabel: '目录',
    returnToTopLabel: '回到顶部',

    socialLinks: [
      { icon: 'github', link: 'https://github.com/givenge/reader-rust' }
    ],

    editLink: {
      text: '在 GitHub 上编辑此页',
      pattern: 'https://github.com/givenge/reader-rust/edit/master/docs/:path'
    },

    footer: {
      message: '本项目仅提供书源管理、内容解析与阅读缓存能力，不存储、不分发任何受版权保护的书籍内容。',
      copyright: '基于 reader 重构 · Rust 版阅读服务端'
    }
  }
})
