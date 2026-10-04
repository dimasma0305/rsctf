import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, useState } from 'react'
import { installTestDom } from '../../test/installDom'
import { ChallengeReviewsRefresh } from './ChallengeReviewsRefresh'

test('challenge review refresh invokes both render-owned mutations', async () => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games/1/reviews' })
  const restoreDom = installTestDom(browser)
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const Harness = () => {
    const [reviews, setReviews] = useState(0)
    const [analytics, setAnalytics] = useState(0)
    return createElement(
      'div',
      null,
      createElement(ChallengeReviewsRefresh, {
        label: 'Refresh reviews',
        refreshReviews: () => setReviews((value) => value + 1),
        refreshAnalytics: () => setAnalytics((value) => value + 1),
      }),
      createElement('output', { id: 'reviews' }, String(reviews)),
      createElement('output', { id: 'analytics' }, String(analytics))
    )
  }

  try {
    await act(async () => root.render(createElement(HeadlessMantineProvider, null, createElement(Harness))))
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="Refresh reviews"]')
    assert.ok(button)
    await act(async () => button.click())
    assert.equal(container.querySelector('#reviews')?.textContent, '1')
    assert.equal(container.querySelector('#analytics')?.textContent, '1')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
