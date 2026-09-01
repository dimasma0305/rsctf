import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { act, createElement, type FC } from 'react'
import { installTestDom } from '../test/installDom'
import { CompletionPollSWRConfig, useCompletionPolling } from './useCompletionPolling'

test('one and one hundred building cards use the same single compact polling owner', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games/7/challenges' })
  const restoreDom = installTestDom(browser)
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const { default: useSWR, SWRConfig } = await import('swr')
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const measure = async (buildingCards: number) => {
    const cache = new Map()
    let reads = 0
    const Probe: FC = () => {
      const query = useSWR(
        `/api/edit/games/7/challenges/buildstatuses?fixture=${buildingCards}`,
        async () => {
          reads += 1
          return reads < 3
            ? Array.from({ length: buildingCards }, (_, index) => ({ challengeId: index + 1, buildStatus: 'Building' }))
            : []
        },
        CompletionPollSWRConfig
      )
      const active = query.data === undefined || query.data.length > 0
      useCompletionPolling({
        key: active ? `/api/edit/games/7/challenges/buildstatuses?fixture=${buildingCards}` : '',
        phase: 'challenge-list',
        enabled: active,
        data: query.data,
        error: query.error,
        isValidating: query.isValidating,
        mutate: query.mutate,
        successDelay: () => 2_000,
        random: () => 0.5,
      })
      return null
    }
    await act(async () =>
      root.render(createElement(SWRConfig, { value: { provider: () => cache, dedupingInterval: 0 } }, createElement(Probe)))
    )
    assert.equal(reads, 1)
    await act(async () => context.mock.timers.tick(2_000))
    assert.equal(reads, 2)
    await act(async () => context.mock.timers.tick(2_000))
    assert.equal(reads, 3)
    await act(async () => context.mock.timers.tick(60_000))
    assert.equal(reads, 3, 'terminal status must remove the only timer owner')
    await act(async () => root.render(createElement('div')))
    return reads
  }

  try {
    assert.equal(await measure(1), 3)
    assert.equal(await measure(100), 3)
  } finally {
    await act(async () => root.unmount())
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('control-plane pages consume compact owners and never restore per-card intervals', () => {
  const cards = readFileSync('src/components/admin/ChallengeEditCard.tsx', 'utf8')
  const list = readFileSync('src/pages/admin/games/[id]/challenges/Index.tsx', 'utf8')
  const detail = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')
  const audit = readFileSync('src/components/admin/ChallengeAuditModal.tsx', 'utf8')
  const workers = readFileSync('src/pages/admin/workers.tsx', 'utf8')
  const builds = readFileSync('src/pages/admin/builds.tsx', 'utf8')

  assert.doesNotMatch(cards, /setInterval|setTimeout/)
  assert.match(list, /useEditGetChallengeBuildStatuses/)
  assert.match(detail, /useEditGetChallengeBuildStatus/)
  assert.doesNotMatch(detail, /setInterval/)
  assert.match(audit, /editGetChallengeAuditMeta/)
  assert.match(audit, /useEditGetChallengeBuildStatus/)
  assert.doesNotMatch(audit, /AuditMeta tick|setInterval/)
  for (const rebuildSurface of [cards, detail, audit]) {
    assert.match(rebuildSurface, /buildFlight/)
    assert.match(rebuildSurface, /createOperationId\(\)/)
    assert.match(rebuildSurface, /startControlJob/)
    assert.match(rebuildSurface, /waitForControlJob/)
  }
  assert.match(workers, /refreshFlight/)
  assert.match(workers, /useCompletionPolling/)
  assert.doesNotMatch(workers, /setInterval/)
  assert.match(builds, /keepMounted=\{false\}/)
  assert.match(builds, /useCompletionPolling/)
  assert.match(builds, /reenqueueFlight/)
  assert.match(builds, /adminReenqueueBuild\(\s*row\.id,\s*operationId/)
  assert.match(builds, /startControlJob/)
  assert.match(builds, /waitForControlJob/)
  assert.doesNotMatch(builds, /refreshInterval:\s*[25]000/)
  assert.match(list, /bulkBuildFlight/)
  assert.match(list, /adminBulkRebuildFailed\(\s*numId,\s*operationId/)
  assert.match(list, /startControlJob/)
  assert.match(list, /waitForControlJob/)

  const api = readFileSync('src/Api.ts', 'utf8')
  assert.match(api, /editRebuildChallengeImage:[\s\S]*"Idempotency-Key": operationId/)
  assert.match(api, /adminBulkRebuildFailed:[\s\S]*"Idempotency-Key": operationId/)
  assert.match(api, /adminReenqueueBuild:[\s\S]*"Idempotency-Key": operationId/)
  assert.match(api, /this\.request<ControlJobModel, RequestResponse>/)

  const controlJobs = readFileSync('src/utils/ControlJobs.ts', 'utf8')
  assert.match(controlJobs, /getControlJobByOperation\(operationId, \{ signal \}\)/)
  assert.match(controlJobs, /signal\?\.removeEventListener\('abort', onAbort\)/)
})
