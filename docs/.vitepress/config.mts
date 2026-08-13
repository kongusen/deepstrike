import { defineConfig } from 'vitepress'
import { enNav, enSidebar, zhNav, zhSidebar } from './shared'

export default defineConfig({
  title: 'DeepStrike',
  description: 'A local Agent Process Runtime for durable, governed Agent work',
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
      description: '面向持久化、可治理 Agent 工作的本地 Agent Process Runtime',
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
      description: 'A local Agent Process Runtime for durable, governed Agent work',
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
