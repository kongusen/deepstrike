import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'DeepStrike',
  description: 'Agent OS microkernel for cross-language agent runtimes',
  
  // Clean URLs are nicer: /getting-started/quick-start instead of /getting-started/quick-start.html
  cleanUrls: true,

  head: [
    ['link', { rel: 'icon', type: 'image/png', href: '/banner.png' }]
  ],

  themeConfig: {
    logo: '/banner.png',
    
    socialLinks: [
      { icon: 'github', link: 'https://github.com/kongusen/deepstrike' },
      { icon: 'discord', link: 'https://discord.gg/cwS3RBYCv' }
    ],

    nav: [
      { text: 'Home', link: '/' },
      { text: 'Agent OS', link: '/concepts/agent-os' },
      { text: 'Quick Start', link: '/getting-started/quick-start' },
      { text: 'Concepts', link: '/concepts/core-concepts' },
      { text: 'Guides', link: '/guides/sdk-nodejs' }
    ],

    sidebar: [
      {
        text: '⚡ Getting Started',
        items: [
          { text: 'Introduction', link: '/getting-started/' },
          { text: 'Quick Start', link: '/getting-started/quick-start' }
        ]
      },
      {
        text: '🧠 Concepts',
        items: [
          { text: 'Overview', link: '/concepts/' },
          { text: 'Agent OS (0.2.6+)', link: '/concepts/agent-os' },
          { text: 'Core Concepts', link: '/concepts/core-concepts' },
          { text: 'Context Slots & Compression', link: '/concepts/context-slots-compression' }
        ]
      },
      {
        text: '🔌 SDK Guides',
        items: [
          { text: 'Overview', link: '/guides/' },
          { text: 'Node.js / TS SDK', link: '/guides/sdk-nodejs' },
          { text: 'Python SDK', link: '/guides/sdk-python' },
          { text: 'Rust SDK', link: '/guides/sdk-rust' },
          { text: 'Providers & Streams', link: '/guides/providers' },
          { text: 'Collaboration & Pools', link: '/guides/collaboration' }
        ]
      },
      {
        text: '🏛️ Architecture',
        items: [
          { text: 'Overview', link: '/architecture/' },
          { text: 'Runtime & Kernel Design', link: '/architecture/overview' }
        ]
      },
      {
        text: '📚 Reference',
        items: [
          { text: 'Overview', link: '/reference/' },
          { text: 'Kernel ABI', link: '/reference/kernel-abi' },
          { text: 'Runtime V2 Lifecycle', link: '/reference/runtime-v2-lifecycle' },
          { text: 'SDK OS Parity', link: '/sdk-os-parity' }
        ]
      },
      {
        text: '⚙️ Operations',
        items: [
          { text: 'Overview', link: '/operations/' },
          { text: 'Release Runbook', link: '/operations/release-runbook' },
          { text: 'Release SOP', link: '/operations/release-sop' }
        ]
      }
    ],

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright © 2026 DeepStrike Authors'
    }
  }
})
