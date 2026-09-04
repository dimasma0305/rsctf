import { HubConnectionBuilder, type HubConnection } from '@microsoft/signalr'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'
import { SCOREBOARD_PUSH_DEBOUNCE_MS, useScoreboardLiveRefresh } from './useScoreboardLiveRefresh'

class FakeHub {
  handlers = new Map<string, Array<(...args: unknown[]) => void>>()
  closeHandler: ((error?: Error) => void) | undefined
  reconnectingHandler: ((error?: Error) => void) | undefined
  reconnectedHandler: (() => void) | undefined
  stopCalls = 0

  start() {
    return Promise.resolve()
  }

  stop() {
    this.stopCalls += 1
    return Promise.resolve()
  }

  onclose(handler: (error?: Error) => void) {
    this.closeHandler = handler
  }

  onreconnecting(handler: (error?: Error) => void) {
    this.reconnectingHandler = handler
  }

  onreconnected(handler: () => void) {
    this.reconnectedHandler = handler
  }

  on(name: string, handler: (...args: unknown[]) => void) {
    const handlers = this.handlers.get(name) ?? []
    handlers.push(handler)
    this.handlers.set(name, handlers)
  }

  emit(name: string, ...args: unknown[]) {
    for (const handler of this.handlers.get(name) ?? []) handler(...args)
  }
}

const settle = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

test('scoreboard push refreshes immediately after a coalesced live event burst', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/7/scoreboard' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const hubs: FakeHub[] = []
  const originalBuild = HubConnectionBuilder.prototype.build
  let refreshes = 0
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  HubConnectionBuilder.prototype.build = function buildFakeHub() {
    const hub = new FakeHub()
    hubs.push(hub)
    return hub as unknown as HubConnection
  }

  const Probe: FC = () => {
    useScoreboardLiveRefresh(7, true, async () => {
      refreshes += 1
    })
    return null
  }

  try {
    await act(async () => {
      root.render(createElement(Probe))
      await settle()
    })
    assert.equal(hubs.length, 1)
    assert.equal(refreshes, 1, 'the connected transport closes the initial read-to-subscription race')

    await act(async () => {
      hubs[0].emit('ReceivedScoreboardChanged', { format: 'jeopardy' })
      hubs[0].emit('ReceivedScoreboardChanged', { format: 'attackDefense' })
      hubs[0].emit('ReceivedScoreboardChanged', { format: 'engines' })
      context.mock.timers.tick(SCOREBOARD_PUSH_DEBOUNCE_MS - 1)
    })
    assert.equal(refreshes, 1)

    await act(async () => {
      context.mock.timers.tick(1)
      await settle()
    })
    assert.equal(refreshes, 2, 'one authoritative HTTP refresh serves the complete event burst')
  } finally {
    await act(async () => {
      root.unmount()
      await settle()
    })
    assert.equal(hubs[0]?.stopCalls, 1)
    HubConnectionBuilder.prototype.build = originalBuild
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
