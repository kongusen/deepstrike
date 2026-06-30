import { defineConfig } from 'vitepress'
import { enNav, enSidebar, zhNav, zhSidebar } from './shared'

export default defineConfig({
  title: 'DeepStrike',
  description: 'Cross-language agent runtime kernel',
  cleanUrls: true,
  ignoreDeadLinks: [/^https?:\/\/localhost/],

  head: [
    ['link', { rel: 'icon', type: 'image/png', href: '/banner.png' }],
  ],

  themeConfig: {
    logo: '/banner.png',
    socialLinks: [
      { icon: 'github', link: 'https://github.com/kongusen/deepstrike' },
      { icon: 'discord', link: 'https://discord.gg/cwS3RBYCv' },
    ],
    search: { provider: 'local' },
  },

  locales: {
    root: {
      label: '简体中文',
      lang: 'zh-CN',
      title: 'DeepStrike',
      description: '跨语言 Agent 运行时内核 — 可重放状态、受治理工具、动态工作流',
      themeConfig: {
        nav: zhNav,
        sidebar: zhSidebar,
        footer: {
          message: 'MIT License',
          copyright: 'Copyright © 2026 DeepStrike Authors',
        },
        docFooter: { prev: '上一页', next: '下一页' },
        outline: { label: '目录' },
      },
    },
    en: {
      label: 'English',
      lang: 'en-US',
      link: '/en/',
      title: 'DeepStrike',
      description: 'Cross-language agent runtime — replayable state, governed tools, dynamic workflows',
      themeConfig: {
        nav: enNav,
        sidebar: enSidebar,
        footer: {
          message: 'Released under the MIT License.',
          copyright: 'Copyright © 2026 DeepStrike Authors',
        },
        docFooter: { prev: 'Previous', next: 'Next' },
        outline: { label: 'On this page' },
      },
    },
  },
})
