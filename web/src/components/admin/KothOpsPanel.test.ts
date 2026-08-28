import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api from '../../Api'
import type { AdminKothHill, AdminKothObserverModel, AdminKothStateModel } from '../../hooks/useGame'
import { installTestDom } from '../../test/installDom'
import { KothOpsPanel } from './KothOpsPanel'

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason: unknown) => void
}

const deferred = <T>(): Deferred<T> => {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept
    reject = decline
  })
  return { promise, resolve, reject }
}

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
  await new Promise((resolve) => setTimeout(resolve, 0))
}

const hill = (challengeId: number, title: string): AdminKothHill => ({
  challengeId,
  title,
  isEnabled: true,
  controlRevision: 1,
  containerGuid: null,
  containerIp: null,
  containerPort: null,
  lastCheckStatus: 'Ok',
  currentHolderTeamName: null,
  currentHolderParticipationId: null,
  durablePhase: 'Active',
  cycleChampions: [],
  oldContainerId: null,
  replacementContainerId: null,
  resetAttempt: 0,
  readinessFailureCount: 0,
  lastReadinessError: null,
  canRetry: false,
  resetReceiptId: null,
  scoringReceiptId: null,
  claimSource: 'Api',
  apiObserverConfigured: true,
  apiObserverSecretHint: null,
  apiLastObservationAt: null,
  provisionalClaimantTeamName: null,
  provisionalClaimantParticipationId: null,
  provisionalConfirmationTicks: 0,
  cycleNumber: 1,
  cycleTick: 1,
  resetPhase: 'Active',
  isScorable: true,
  nextResetTicks: 20,
  cooldownParticipants: [],
})

const state = (hills: AdminKothHill[]): AdminKothStateModel => ({
  epochTicks: 10,
  cycleTicks: 60,
  championCooldownTicks: 5,
  claimConfirmationTicks: 2,
  tickSeconds: 10,
  scoringGeneratedAt: Date.now(),
  latestRound: 1,
  currentRoundEndsAt: null,
  scoringPaused: false,
  controlRevision: 1,
  scoringPausedAt: null,
  hills,
  teams: [],
})

const observer = (challengeId: number, context: string): AdminKothObserverModel => ({
  challengeId,
  revision: 1,
  claimSource: 'Api',
  configured: true,
  managedTargetReporting: false,
  secretHint: null,
  objectiveCount: null,
  objectiveIds: null,
  objectiveSchemaHash: null,
  createdAt: null,
  rotatedAt: null,
  lastUsedAt: null,
  lastObservationAt: null,
  contextPath: context,
  observationPath: `/observations/${challengeId}`,
})

test('KotH dialogs reject reversed reads, expose retryable errors, and mutate only the owned hill', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games/7/ad-ops' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const originalRequest = api.request
  const reads = new Map<string, Deferred<any>[]>()
  const mutations: Array<{ path: string; body: any; method: string }> = []
  const warning = context.mock.method(console, 'warn', () => undefined)

  api.request = (async (params: any) => {
    if (params.method === 'POST' || params.method === 'DELETE') {
      mutations.push({ path: params.path, body: params.body, method: params.method })
      return {
        data: {
          ...observer(Number(params.path.match(/koth\/(\d+)/)?.[1]), '/mutated'),
          revision: params.body.expectedRevision + 1,
          operationId: params.body.operationId,
        },
      }
    }
    const read = deferred<any>()
    reads.set(params.path, [...(reads.get(params.path) ?? []), read])
    return await read.promise
  }) as typeof api.request

  const hills = [hill(1, 'Hill A'), hill(2, 'Hill B')]
  let mutateCount = 0
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const render = async (gameId: number, currentHills: AdminKothHill[]) => {
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(KothOpsPanel, {
              gameId,
              koth: state(currentHills),
              onShell: () => undefined,
              onToggleHill: () => undefined,
              busyHill: null,
              onMutate: async () => {
                mutateCount += 1
              },
            })
          )
        )
      )
      await flush()
    })
  }
  const row = (title: string) =>
    Array.from(browser.document.querySelectorAll<HTMLTableRowElement>('tr')).find((item) =>
      item.textContent?.includes(title)
    )
  const rowButton = (title: string, label: RegExp) =>
    Array.from(row(title)?.querySelectorAll<HTMLButtonElement>('button') ?? []).find((button) =>
      label.test(button.textContent ?? '')
    )
  const dialog = (kind: 'receipts' | 'observer') => {
    const scope = browser.document.querySelector<HTMLElement>(`[data-koth-dialog="${kind}"]`)
    return scope?.matches('[role="dialog"]') ? scope : scope?.querySelector<HTMLElement>('[role="dialog"]')
  }
  const closeTopDialog = async () => {
    await act(async () => {
      browser.document.dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      await flush()
    })
  }
  const read = (path: string, index = 0) => {
    const value = reads.get(path)?.[index]
    assert.ok(value, `missing read ${path} #${index}`)
    return value
  }

  try {
    await render(7, hills)

    await act(async () => {
      rowButton('Hill A', /^Receipts$/)?.click()
      rowButton('Hill B', /^Receipts$/)?.click()
      await flush()
    })
    const receiptA = '/api/edit/games/7/ad/koth/1/receipts'
    const receiptB = '/api/edit/games/7/ad/koth/2/receipts'
    await act(async () => {
      read(receiptB).resolve({
        data: {
          challengeId: 2,
          cycleNumber: 1,
          receipts: [{ id: 2, phase: 'B-RECEIPT', attempt: 1, receipt: {}, filesystemDiff: null, createdAt: 1 }],
        },
      })
      await flush()
    })
    assert.match(dialog('receipts')?.textContent ?? '', /Hill B/)
    assert.match(dialog('receipts')?.textContent ?? '', /B-RECEIPT/)
    await act(async () => {
      read(receiptA).resolve({
        data: {
          challengeId: 1,
          cycleNumber: 1,
          receipts: [{ id: 1, phase: 'A-RECEIPT', attempt: 1, receipt: {}, filesystemDiff: null, createdAt: 1 }],
        },
      })
      await flush()
    })
    assert.doesNotMatch(dialog('receipts')?.textContent ?? '', /A-RECEIPT/)

    await closeTopDialog()
    await act(async () => {
      rowButton('Hill A', /Manage scoring/)?.click()
      rowButton('Hill B', /Manage scoring/)?.click()
      await flush()
    })
    const observerA = '/api/edit/games/7/ad/koth/1/observer'
    const observerB = '/api/edit/games/7/ad/koth/2/observer'
    await act(async () => {
      read(observerA).resolve({ data: observer(1, '/context/A') })
      await flush()
    })
    assert.match(dialog('observer')?.textContent ?? '', /Hill B/)
    assert.doesNotMatch(dialog('observer')?.textContent ?? '', /context\/A/)
    await act(async () => {
      read(observerB).resolve({ data: observer(2, '/context/B') })
      await flush()
    })
    assert.match(dialog('observer')?.textContent ?? '', /context\/B/)

    const rotate = Array.from(dialog('observer')?.querySelectorAll<HTMLButtonElement>('button') ?? []).find((button) =>
      /Rotate secret/.test(button.textContent ?? '')
    )
    assert.ok(rotate)
    await act(async () => {
      rotate.click()
      await flush()
    })
    assert.equal(mutations.length, 1)
    assert.equal(mutations[0].path, observerB)
    assert.equal(mutations[0].body.expectedRevision, 1)
    assert.equal(mutateCount, 1)

    await closeTopDialog()
    await act(async () => {
      rowButton('Hill A', /Manage scoring/)?.click()
      await flush()
      read(observerA, 1).reject(new Error('Referee service unavailable'))
      await flush()
    })
    const failedDialog = dialog('observer')
    assert.match(failedDialog?.textContent ?? '', /Hill A/)
    assert.match(failedDialog?.querySelector('[role="alert"]')?.textContent ?? '', /Referee service unavailable/)
    assert.ok(
      Array.from(failedDialog?.querySelectorAll<HTMLButtonElement>('button') ?? []).some((button) =>
        /Retry/.test(button.textContent ?? '')
      ),
      'the owned load error must expose a retry action'
    )

    await render(7, [hills[1]])
    assert.doesNotMatch(browser.document.body.textContent ?? '', /Leaderboard scoring — Hill A/)
    assert.equal(warning.mock.callCount(), 0)
  } finally {
    api.request = originalRequest
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
