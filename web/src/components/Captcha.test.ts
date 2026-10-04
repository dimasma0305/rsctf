import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, createRef } from 'react'
import { SWRConfig } from 'swr'
import { CaptchaProvider, type ClientCaptchaInfoModel } from '../Api'
import { installTestDom } from '../test/installDom'
import type { CaptchaInstance } from './Captcha'

test('first submit waits for a disabled captcha policy instead of rejecting it', async () => {
  const browser = new Window({ url: 'https://rsctf.test/account/login' })
  const restoreDom = installTestDom(browser)
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const captchaRef = createRef<CaptchaInstance>()
  let resolveConfig: ((value: ClientCaptchaInfoModel) => void) | undefined
  const config = new Promise<ClientCaptchaInfoModel>((resolve) => {
    resolveConfig = resolve
  })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const { Captcha } = await import('./Captcha')
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)

  try {
    await act(async () => {
      root.render(
        createElement(
          SWRConfig,
          {
            value: {
              provider: () => new Map(),
              fetcher: async (key: string) => {
                assert.equal(key, '/api/captcha')
                return config
              },
            },
          },
          createElement(HeadlessMantineProvider, null, createElement(Captcha, { action: 'login', ref: captchaRef }))
        )
      )
    })

    assert.ok(captchaRef.current)
    let settled = false
    const resultPromise = captchaRef.current.getToken().then((result) => {
      settled = true
      return result
    })
    await Promise.resolve()
    assert.equal(settled, false)

    await act(async () => {
      assert.ok(resolveConfig)
      resolveConfig({ type: CaptchaProvider.None })
    })

    assert.deepEqual(await resultPromise, { valid: true })
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('captcha policy refresh failures remain closed', async () => {
  const { resolveCaptchaInfo } = await import('./Captcha')
  const info = await resolveCaptchaInfo(undefined, async () => {
    throw new Error('offline')
  })

  assert.equal(info, undefined)
})
