import { MantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { installTestDom } from '../test/installDom'
import { IconTabs } from './IconTabs'

test('controlled section tabs follow URL state during rapid Back navigation; uncontrolled tabs retain keyboard selection', async () => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const requested: string[] = []
  const tabs = [
    { tabKey: 'general', label: 'General' },
    { tabKey: 'security', label: 'Security' },
  ]
  const render = (active?: number) =>
    root.render(
      createElement(
        MantineProvider,
        null,
        createElement(IconTabs, {
          key: active === undefined ? 'uncontrolled' : 'controlled',
          active,
          tabs,
          idPrefix: 'test',
          onTabChange: (_index, key) => requested.push(key),
        })
      )
    )
  const selected = () => container.querySelector('[role=tab][aria-selected=true]')?.id
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  try {
    await act(async () => render(0))
    await act(async () => container.querySelector<HTMLButtonElement>('#test-tab-security')?.click())
    assert.deepEqual(requested, ['security'])
    assert.equal(selected(), 'test-tab-general', 'a pending navigation cannot disagree with the rendered panel')
    await act(async () => render(0))
    assert.equal(selected(), 'test-tab-general', 'Back before the transition commits cannot leave a stale highlight')
    await act(async () => render(1))
    assert.equal(selected(), 'test-tab-security')
    await act(async () => render(0))
    assert.equal(selected(), 'test-tab-general')

    await act(async () => render())
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('#test-tab-general')
        ?.dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'End', bubbles: true }) as unknown as KeyboardEvent)
    )
    assert.equal(selected(), 'test-tab-security')
    await act(async () => container.querySelector<HTMLButtonElement>('#test-tab-general')?.click())
    assert.equal(selected(), 'test-tab-general')
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
