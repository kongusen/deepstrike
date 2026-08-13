import { defineConfig } from 'vitepress'
import { enNav, enSidebar, zhNav, zhSidebar } from './shared'

export default defineConfig({
  title: 'DeepStrike',
  description: 'A framework for Agents with tools, memory, collaboration, and durable sessions',
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
      description: '为 Agent 提供工具、记忆、协作、治理与可恢复 Session 的框架',
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
      description: 'A framework for Agents with tools, memory, collaboration, and durable sessions',
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
