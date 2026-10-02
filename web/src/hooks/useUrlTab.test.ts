import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, useState } from 'react'
import { createMemoryRouter, RouterProvider } from 'react-router'
import { installTestDom } from '../test/installDom'
import { useUrlTab } from './useUrlTab'

test('URL tabs restore deep links, preserve nested state and hashes, and support history without losing drafts', async () => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let setSection: (value: string | null) => void = () => {}
  let setTab: (value: string | null) => void = () => {}
  let setDraft: (value: string) => void = () => {}
  const Probe = () => {
    const [section, updateSection] = useUrlTab(
      'section',
      ['suspicion', 'network-device', 'abnormal-solves'],
      'suspicion'
    )
    const [tab, updateTab] = useUrlTab('tab', ['analysis', 'submissions'], 'analysis')
    const [draft, updateDraft] = useState('unsaved')
    setSection = updateSection
    setTab = updateTab
    setDraft = updateDraft
    return createElement('output', null, `${tab}/${section}/${draft}`)
  }
  const routes = [{ path: '/review', element: createElement(Probe) }]
  const initial = '/review?section=abnormal-solves&other=keep#snapshot=12&file=%2Fetc%2Ftest'
  const router = createMemoryRouter(routes, { initialEntries: [initial] })
  let reloaded: ReturnType<typeof createMemoryRouter> | undefined
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  try {
    await act(async () => root.render(createElement(RouterProvider, { router })))
    assert.equal(container.textContent, 'analysis/abnormal-solves/unsaved')
    await act(async () => setDraft('my draft'))
    await act(async () => setSection('network-device'))
    assert.equal(container.textContent, 'analysis/network-device/my draft')
    assert.equal(router.state.location.hash, '#snapshot=12&file=%2Fetc%2Ftest')
    assert.equal(new URLSearchParams(router.state.location.search).get('other'), 'keep')
    await act(async () => setTab('submissions'))
    assert.equal(container.textContent, 'submissions/network-device/my draft')
    assert.equal(new URLSearchParams(router.state.location.search).get('section'), 'network-device')
    const key = router.state.location.key
    for (const value of ['submissions', null, 'https://untrusted.test', 'invalid']) {
      await act(async () => setTab(value))
      assert.equal(router.state.location.key, key, 'same/invalid choices do not add history')
    }
    await act(async () => router.navigate(-1))
    assert.equal(container.textContent, 'analysis/network-device/my draft')
    await act(async () => router.navigate(-1))
    assert.equal(container.textContent, 'analysis/abnormal-solves/my draft')
    await act(async () => router.navigate(1))
    assert.equal(container.textContent, 'analysis/network-device/my draft')

    const { pathname, search, hash } = router.state.location
    await act(async () => root.render(null))
    reloaded = createMemoryRouter(routes, { initialEntries: [pathname + search + hash] })
    await act(async () => root.render(createElement(RouterProvider, { router: reloaded! })))
    assert.equal(
      container.textContent,
      'analysis/network-device/unsaved',
      'copied URLs restore the section, never the draft'
    )
    await act(async () => reloaded!.navigate('/review?tab=invalid&section=invalid#challenge'))
    assert.equal(container.textContent, 'analysis/suspicion/unsaved')
    assert.equal(
      new URLSearchParams(reloaded.state.location.search).get('section'),
      'invalid',
      'mount does not rewrite history'
    )
    await act(async () => setSection('suspicion'))
    assert.equal(new URLSearchParams(reloaded.state.location.search).get('section'), 'suspicion')
    await act(async () => reloaded!.navigate('/review'))
    assert.equal(container.textContent, 'analysis/suspicion/unsaved', 'legacy bare URLs retain their default view')
    await act(async () => setSection('suspicion'))
    assert.equal(
      new URLSearchParams(reloaded.state.location.search).get('section'),
      'suspicion',
      'explicitly choosing the current fallback makes it shareable'
    )
  } finally {
    await act(async () => root.unmount())
    router.dispose()
    reloaded?.dispose()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
