import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import useSWR, { SWRConfig } from 'swr'
import { installTestDom } from '../test/installDom'
import { OnceSWRConfig } from './useConfig'

const flush = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

test('one-shot SWR retries are canceled when their key changes or consumer unmounts', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  let reads = 0
  const fetcher = async () => {
    reads += 1
    throw { response: { status: 503 } }
  }
  const Probe: FC<{ swrKey: string }> = ({ swrKey }) => {
    useSWR(swrKey, fetcher, OnceSWRConfig)
    return null
  }
  const render = (swrKey: string) =>
    createElement(
      SWRConfig,
      { value: { provider: () => new Map(), dedupingInterval: 0 } },
      createElement(Probe, { swrKey })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => {
      root.render(render('/api/first'))
      await flush()
    })
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(3_000))
    assert.equal(reads, 2, 'the mounted one-shot reader still performs bounded recovery')

    await act(async () => {
      root.render(render('/api/second'))
      await flush()
    })
    assert.equal(reads, 3)
    await act(async () => root.unmount())
    context.mock.timers.tick(60_000)
    assert.equal(reads, 3)
  } finally {
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
