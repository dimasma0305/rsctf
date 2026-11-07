import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { createMemoryRouter, Outlet, RouterProvider } from 'react-router'
import { SWRConfig } from 'swr'
import { installTestDom } from '../test/installDom'
import { isIndexablePage } from '../utils/PageMetadata'
import { usePageTitle, useResetPageMetadata } from './usePageTitle'

test('indexing is limited to public landing pages and confirmed public details', () => {
  for (const path of [
    '/admin/settings',
    '/account/login',
    '/teams',
    '/games/19/scoreboard',
    '/posts/abcdefgh/edit',
    '/missing',
  ]) {
    assert.equal(isIndexablePage(path, true), false, path)
  }
  assert.equal(isIndexablePage('/games/19'), false)
  assert.equal(isIndexablePage('/games/19', true), true)
  assert.equal(isIndexablePage('/posts/abcdefgh', true), true)
  assert.equal(isIndexablePage('/games/'), true)
})

test('route metadata drops private previews, query tokens and stale canonical links', async () => {
  const browser = new Window({ url: 'https://alias.example/' })
  const restoreDom = installTestDom(browser)
  const globals = globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
  const previousActEnvironment = globals.IS_REACT_ACT_ENVIRONMENT
  globals.IS_REACT_ACT_ENVIRONMENT = true
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const Layout = () => {
    useResetPageMetadata()
    return createElement(Outlet)
  }
  const Public = () => {
    usePageTitle('Open CTF', {
      description: 'Play <security> & learn',
      imageUrl: '/assets/posterhash/poster',
      indexable: true,
    })
    return null
  }
  const Private = () => {
    usePageTitle('Secret team', { description: 'Private information', indexable: true })
    return null
  }
  const About = () => {
    usePageTitle('About')
    return null
  }
  const router = createMemoryRouter(
    [
      {
        element: createElement(Layout),
        children: [
          { path: '/games/19', element: createElement(Public) },
          { path: '/admin/teams', element: createElement(Private) },
          { path: '/about', element: createElement(About) },
        ],
      },
    ],
    { initialEntries: ['/games/19?token=secret#private'] }
  )
  const config = {
    title: 'Example',
    description: 'Public site description',
    publicUrl: 'https://ctf.example/',
    logoUrl: '/assets/brand/logo',
  }
  const meta = (name: string) =>
    browser.document.querySelector(`meta[name="${name}"],meta[property="${name}"]`)?.getAttribute('content')
  try {
    await act(async () =>
      root.render(
        createElement(
          SWRConfig,
          { value: { provider: () => new Map(), fallback: { '/api/config': config }, fetcher: async () => config } },
          createElement(RouterProvider, { router })
        )
      )
    )
    assert.equal(meta('robots'), 'index, follow')
    assert.equal(meta('description'), 'Play <security> & learn')
    assert.equal(meta('og:title'), 'Open CTF - Example::CTF')
    assert.equal(meta('og:url'), 'https://ctf.example/games/19')
    assert.equal(meta('og:image'), 'https://ctf.example/assets/posterhash/poster')
    assert.equal(
      browser.document.querySelector('link[rel="canonical"]')?.getAttribute('href'),
      'https://ctf.example/games/19'
    )
    await act(async () => {
      await router.navigate('/admin/teams?token=secret')
    })
    assert.equal(meta('robots'), 'noindex, nofollow')
    assert.equal(meta('og:title'), 'Example::CTF')
    assert.equal(meta('description'), 'Public site description')
    assert.equal(meta('og:url'), undefined)
    assert.equal(browser.document.querySelector('link[rel="canonical"]'), null)
    await act(async () => {
      await router.navigate('/about')
    })
    assert.equal(meta('robots'), 'index, follow')
    assert.equal(meta('og:title'), 'About - Example::CTF')
    assert.equal(meta('description'), 'Public site description')
    assert.equal(meta('og:image'), 'https://ctf.example/assets/brand/logo')
    assert.equal(browser.document.querySelectorAll('meta[name="description"]').length, 1)
    assert.equal(browser.document.head.innerHTML.includes('token=secret'), false)
  } finally {
    await act(async () => root.unmount())
    router.dispose()
    restoreDom()
    globals.IS_REACT_ACT_ENVIRONMENT = previousActEnvironment
    await browser.happyDOM.abort()
  }
})
