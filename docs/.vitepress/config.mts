import { defineConfig } from 'vitepress'

const repository = process.env.GITHUB_REPOSITORY ?? 'dimasma0305/rsctf'
const repositoryUrl = `https://github.com/${repository}`

export default defineConfig({
  title: 'rsctf',
  titleTemplate: ':title · rsctf docs',
  description: 'Clear guides for installing, running, organizing, and playing CTF events with rsctf.',
  lang: 'en-US',
  base: process.env.DOCS_BASE || '/',
  cleanUrls: true,
  lastUpdated: true,
  metaChunk: true,
  head: [
    ['link', { rel: 'icon', href: `${process.env.DOCS_BASE || '/'}logo.svg`, type: 'image/svg+xml' }],
    ['meta', { name: 'theme-color', content: '#f8fafc', media: '(prefers-color-scheme: light)' }],
    ['meta', { name: 'theme-color', content: '#0b1120', media: '(prefers-color-scheme: dark)' }],
    ['meta', { name: 'color-scheme', content: 'light dark' }],
  ],
  markdown: {
    lineNumbers: true,
    math: true,
    theme: { light: 'github-light', dark: 'github-dark' },
    config(md) {
      const extractNativeMath = (renderedMath: string) =>
        renderedMath.match(
          /<mjx-assistive-mml\b[^>]*>(<math\b[\s\S]*?<\/math>)<\/mjx-assistive-mml>/
        )?.[1]

      const renderMathInline = md.renderer.rules.math_inline
      if (renderMathInline) {
        md.renderer.rules.math_inline = (tokens, idx, options, env, self) => {
          const renderedMath = renderMathInline(tokens, idx, options, env, self)
          return extractNativeMath(renderedMath) ?? renderedMath
        }
      }

      const renderMathBlock = md.renderer.rules.math_block
      if (renderMathBlock) {
        md.renderer.rules.math_block = (tokens, idx, options, env, self) => {
          const renderedMath = renderMathBlock(tokens, idx, options, env, self)
          const nativeMath = extractNativeMath(renderedMath)

          if (!nativeMath) return `<div class="journal-math">${renderedMath}</div>`

          const focusableMath = nativeMath.replace('<math', '<math tabindex="0"')
          return `<div class="journal-math">${focusableMath}</div>`
        }
      }
    },
  },
  sitemap: process.env.DOCS_HOSTNAME ? { hostname: process.env.DOCS_HOSTNAME } : undefined,
  themeConfig: {
    logo: { src: '/logo.svg', alt: 'rsctf documentation home' },
    siteTitle: 'rsctf docs',
    search: {
      provider: 'local',
      options: {
        detailedView: true,
        translations: {
          button: { buttonText: 'Search documentation', buttonAriaLabel: 'Search documentation' },
          modal: {
            displayDetails: 'Display detailed results',
            resetButtonTitle: 'Reset search',
            backButtonTitle: 'Close search',
            noResultsText: 'No pages found for',
            footer: {
              selectText: 'to select',
              navigateText: 'to navigate',
              closeText: 'to close',
            },
          },
        },
      },
    },
    nav: [
      { text: 'Get started', link: '/getting-started/' },
      { text: 'Players', link: '/players/' },
      { text: 'Organizers', link: '/organizers/' },
      { text: 'Deploy', link: '/deploy/' },
      { text: 'Reference', link: '/reference/configuration' },
    ],
    sidebar: {
      '/getting-started/': [
        {
          text: 'Get started',
          items: [
            { text: 'Choose your path', link: '/getting-started/' },
            { text: 'Install with the wizard', link: '/getting-started/install-wizard' },
            { text: 'First login and setup', link: '/getting-started/first-login' },
          ],
        },
      ],
      '/players/': [
        {
          text: 'Player guide',
          items: [
            { text: 'Start here', link: '/players/' },
            { text: 'Accounts and teams', link: '/players/accounts-and-teams' },
            { text: 'Jeopardy games', link: '/players/jeopardy' },
            { text: 'Attack & Defense', link: '/players/attack-defense' },
            { text: 'King of the Hill', link: '/players/koth' },
            { text: 'KotH scoring paper', link: '/players/koth-scoring-handbook' },
            { text: 'Rules and fair play', link: '/players/rules' },
          ],
        },
      ],
      '/organizers/': [
        {
          text: 'Organizer guide',
          items: [
            { text: 'Run your first event', link: '/organizers/' },
            { text: 'Create games', link: '/organizers/games' },
            { text: 'Create challenges', link: '/organizers/challenges' },
            { text: 'Import the sample repository', link: '/organizers/sample-repository' },
            { text: 'Operate a live event', link: '/organizers/live-event' },
          ],
        },
      ],
      '/deploy/': [
        {
          text: 'Deployment',
          items: [
            { text: 'Choose a deployment', link: '/deploy/' },
            { text: 'Docker Compose', link: '/deploy/docker' },
            { text: 'Kubernetes with Helm', link: '/deploy/kubernetes' },
            { text: 'Scale the single binary', link: '/deploy/scaling' },
            { text: 'Private challenge workers', link: '/deploy/workers' },
            { text: 'Reverse proxy and HTTPS', link: '/deploy/reverse-proxy' },
            { text: 'Back up and update', link: '/deploy/operations' },
            { text: 'Security checklist', link: '/deploy/security' },
            { text: 'GitHub integration', link: '/deploy/github' },
          ],
        },
      ],
      '/reference/': [
        {
          text: 'Reference',
          items: [
            { text: 'Configuration', link: '/reference/configuration' },
            { text: 'Installer options', link: '/reference/installer' },
            { text: 'Health and troubleshooting', link: '/reference/troubleshooting' },
            { text: 'BYOC SSH internals', link: '/reference/byoc-ssh' },
          ],
        },
      ],
    },
    socialLinks: [{ icon: 'github', link: repositoryUrl }],
    editLink: {
      pattern: `${repositoryUrl}/edit/main/docs/:path`,
      text: 'Improve this page on GitHub',
    },
    lastUpdated: { text: 'Last updated', formatOptions: { dateStyle: 'medium' } },
    outline: { level: [2, 3], label: 'On this page' },
    docFooter: { prev: 'Previous guide', next: 'Next guide' },
    footer: {
      message:
        `RSCTF is licensed under the MIT License. Review the <a href="${repositoryUrl}/blob/main/LICENSING.md">licensing guide</a> ` +
        `and the vendored <a href="${repositoryUrl}/blob/main/web/src/lib/creepjs/LICENSE">CreepJS license</a>.`,
      copyright: 'Documentation for the rsctf community.',
    },
  },
})
