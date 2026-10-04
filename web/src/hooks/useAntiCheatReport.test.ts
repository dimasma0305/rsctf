import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { SWRConfig } from 'swr'
import type { CheatReport } from '@Api'
import { installTestDom } from '../test/installDom'
import { type AntiCheatReportReader, useAntiCheatReport } from './useAntiCheatReport'

const report = (generatedAt: number, sealedAt: number | null): CheatReport => ({
  generatedAt,
  sealedAt,
  ipAnalysis: [],
  abnormalSolves: [],
  collusionGroups: [],
  suspicionList: [],
  identityOverlaps: [],
})

test('conditional report polling reuses 304 snapshots and stops after sealing', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/4/monitor/cheat' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 0 })
  context.mock.method(Math, 'random', () => 0.5)
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  const validators: Array<string | undefined> = []

  const reader: AntiCheatReportReader = async (_gameId, etag) => {
    validators.push(etag)
    if (validators.length === 1) return { status: 200, data: report(1, null), etag: '"revision-1"' }
    if (validators.length === 2) return { status: 304 }
    return { status: 200, data: report(2, 120_000), etag: '"revision-2"' }
  }

  const Probe: FC<{ active: boolean }> = ({ active }) => {
    const { data } = useAntiCheatReport(4, active, reader)
    return createElement('output', null, data?.generatedAt ?? 'none')
  }
  const cache = new Map()
  const Scope: FC<{ active: boolean }> = ({ active }) =>
    createElement(
      SWRConfig,
      { value: { provider: () => cache, dedupingInterval: 0, isVisible: () => true, isOnline: () => true } },
      createElement(Probe, { active })
    )
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => root.render(createElement(Scope, { active: false })))
    await act(async () => context.mock.timers.tick(10 * 60_000))
    assert.equal(validators.length, 0)

    await act(async () => root.render(createElement(Scope, { active: true })))
    assert.equal(container.textContent, '1')
    assert.deepEqual(validators, [undefined])

    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(container.textContent, '1', 'a 304 must retain the decoded report')
    assert.deepEqual(validators, [undefined, '"revision-1"'])

    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(container.textContent, '2')
    assert.deepEqual(validators, [undefined, '"revision-1"', '"revision-1"'])

    await act(async () => context.mock.timers.tick(10 * 60_000))
    assert.equal(validators.length, 3, 'a sealed report must stop interval reads')
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
