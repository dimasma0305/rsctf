import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'

test('shared ticker cancels alignment and survives rapid visibility changes', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const startedAt = 2_000_000_000_100
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(startedAt),
  })
  const { useTicker } = await import('./useTicker')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const Probe: FC = () => createElement('output', null, String(useTicker().valueOf()))
  const setHidden = (hidden: boolean) => {
    Object.defineProperty(browser.document, 'hidden', { configurable: true, value: hidden })
    browser.document.dispatchEvent(new browser.Event('visibilitychange'))
  }

  try {
    await act(async () => root.render(createElement(Probe)))
    const initialValue = container.textContent

    // Unmount before the whole-second alignment fires. A cancelled scheduler
    // must not advance its shared value or leave an interval behind.
    await act(async () => root.unmount())
    context.mock.timers.tick(2_000)

    const secondContainer = browser.document.createElement('div')
    browser.document.body.append(secondContainer)
    const secondRoot = createRoot(secondContainer)
    await act(async () => secondRoot.render(createElement(Probe)))
    assert.equal(secondContainer.textContent, initialValue)

    // hide -> show before alignment used to queue two callbacks. Hiding again
    // cleared only the last interval, so the orphan kept ticking in background.
    setHidden(true)
    setHidden(false)
    await act(async () => context.mock.timers.tick(1_000))
    setHidden(true)
    const hiddenValue = secondContainer.textContent
    await act(async () => context.mock.timers.tick(2_000))
    assert.equal(secondContainer.textContent, hiddenValue)

    await act(async () => secondRoot.unmount())
  } finally {
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
