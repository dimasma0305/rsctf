import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api, { type AntiCheatFindingRow, type FusedEvidenceBreakdown } from '../../Api'
import { installTestDom } from '../../test/installDom'
import { FusedEvidencePanel, fusedEvidenceMatchesScope } from './FusedEvidencePanel'

const flush = async () => {
  await Promise.resolve()
  await Promise.resolve()
  await new Promise((resolve) => setTimeout(resolve, 0))
}

const finding = (id: number, participationId: number, detectorCode: string): AntiCheatFindingRow => ({
  id,
  gameId: 7,
  participationId,
  detectorCode,
  detectorVersion: 1,
  evidenceFamily: 1,
  evidenceTier: 1,
  scoreDelta: 10,
  evidenceKey: `finding:${id}`,
  occurredAtUtc: Date.now(),
  details: { source: detectorCode },
  shadow: false,
  createdAtUtc: Date.now(),
})

const breakdown = (participationId: number, findings: AntiCheatFindingRow[]): FusedEvidenceBreakdown => ({
  participationId,
  total: findings.length * 10,
  band: 'investigate',
  bandLabel: 'Investigate',
  reviewerConfirmed: false,
  independentActionableFamilies: findings.length ? 1 : 0,
  existingScore: 0,
  findingScore: findings.length * 10,
  families: [],
  findings,
  relationships: [],
})

test('fused evidence accepts only rows declared for the selected game and participation', () => {
  const exact = breakdown(22, [finding(2, 22, 'EXACT')])
  assert.equal(fusedEvidenceMatchesScope(exact, 7, 22), true)
  assert.equal(fusedEvidenceMatchesScope({ ...exact, participationId: 11 }, 7, 22), false)
  assert.equal(fusedEvidenceMatchesScope({ ...exact, findings: [{ ...exact.findings[0], gameId: 8 }] }, 7, 22), false)
  assert.equal(
    fusedEvidenceMatchesScope({ ...exact, findings: [{ ...exact.findings[0], participationId: 11 }] }, 7, 22),
    false
  )
})

test('fused evidence rejects a late participation and resets the finding-bound review draft', async () => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games/7/monitor' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const originalBreakdown = api.eventSecurity.fusedBreakdown
  let resolveOld: ((value: { data: FusedEvidenceBreakdown }) => void) | undefined
  let resolveNew: ((value: { data: FusedEvidenceBreakdown }) => void) | undefined
  api.eventSecurity.fusedBreakdown = ((_gameId: number, participationId: number) =>
    new Promise((resolve) => {
      if (participationId === 11) resolveOld = resolve
      else resolveNew = resolve
    })) as typeof api.eventSecurity.fusedBreakdown

  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const render = (participationId: number) =>
    createElement(
      HeadlessMantineProvider,
      null,
      createElement(I18nextProvider, { i18n }, createElement(FusedEvidencePanel, { gameId: 7, participationId }))
    )

  try {
    await act(async () => {
      root.render(render(11))
      await flush()
    })
    await act(async () => {
      root.render(render(22))
      await flush()
    })
    await act(async () => {
      resolveOld?.({ data: breakdown(11, [finding(1, 11, 'OLD-PARTICIPATION')]) })
      await flush()
    })
    assert.doesNotMatch(container.textContent || '', /OLD-PARTICIPATION/)

    const newFindings = [finding(2, 22, 'NEW-A'), finding(3, 22, 'NEW-B')]
    await act(async () => {
      resolveNew?.({ data: breakdown(22, newFindings) })
      await flush()
    })
    assert.match(container.textContent || '', /NEW-A/)
    assert.match(container.textContent || '', /NEW-B/)

    const reviewA = browser.document.querySelector<HTMLButtonElement>('button[aria-label="Review NEW-A"]')
    assert.ok(reviewA)
    await act(async () => reviewA.click())
    const noteA = browser.document.querySelector<HTMLTextAreaElement>('textarea[data-finding-review-note="2"]')
    assert.ok(noteA)
    await act(async () => {
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLTextAreaElement.prototype, 'value')?.set
      assert.ok(setValue)
      setValue.call(noteA, 'draft for A only')
      noteA.dispatchEvent(new browser.Event('input', { bubbles: true }))
      await flush()
    })
    assert.equal(noteA.value, 'draft for A only')

    const reviewB = browser.document.querySelector<HTMLButtonElement>('button[aria-label="Review NEW-B"]')
    assert.ok(reviewB)
    await act(async () => {
      reviewB.click()
      await flush()
    })
    const noteB = browser.document.querySelector<HTMLTextAreaElement>('textarea[data-finding-review-note="3"]')
    assert.ok(noteB)
    assert.equal(noteB.value, '', "finding B must never inherit finding A's draft")
  } finally {
    api.eventSecurity.fusedBreakdown = originalBreakdown
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('finding review closes dirty drafts deliberately, admits one Unicode save, and exposes failed reload', async () => {
  const browser = new Window({ url: 'https://rsctf.test/admin/games/7/monitor' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const originalBreakdown = api.eventSecurity.fusedBreakdown
  const originalReview = api.eventSecurity.reviewFinding
  const reviewed = finding(4, 44, 'UNICODE')
  reviewed.latestReviewStatus = 2
  let loadCount = 0
  api.eventSecurity.fusedBreakdown = (async () => {
    loadCount += 1
    if (loadCount > 1) throw new Error('Committed, but refresh unavailable')
    return { data: breakdown(44, [reviewed]) }
  }) as typeof api.eventSecurity.fusedBreakdown
  let resolveReview: (() => void) | undefined
  const reviewBodies: unknown[] = []
  api.eventSecurity.reviewFinding = (async (_gameId: number, _findingId: number, body: unknown) => {
    reviewBodies.push(body)
    await new Promise<void>((resolve) => {
      resolveReview = resolve
    })
    return { data: undefined }
  }) as typeof api.eventSecurity.reviewFinding

  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const reviewButton = () => browser.document.querySelector<HTMLButtonElement>('button[aria-label="Review UNICODE"]')
  const note = () => browser.document.querySelector<HTMLTextAreaElement>('textarea[data-finding-review-note="4"]')
  const record = () =>
    Array.from(browser.document.querySelectorAll<HTMLButtonElement>('button')).find((button) =>
      button.textContent?.includes('Record review')
    )
  const setNote = async (value: string) => {
    const input = note()
    const setter = Object.getOwnPropertyDescriptor(browser.HTMLTextAreaElement.prototype, 'value')?.set
    assert.ok(input)
    assert.ok(setter)
    await act(async () => {
      setter.call(input, value)
      input.dispatchEvent(new browser.Event('input', { bubbles: true }))
      await flush()
    })
  }

  try {
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(FusedEvidencePanel, { gameId: 7, participationId: 44 })
          )
        )
      )
      await flush()
      await flush()
    })

    await act(async () => reviewButton()?.click())
    await setNote('discard this draft')
    await act(async () => {
      reviewButton()?.click()
      await flush()
    })
    await act(async () => {
      reviewButton()?.click()
      await flush()
    })
    assert.equal(note()?.value, '', 'an intentional close/reopen must not revive another review draft')

    const unicodeNote = 'é'.repeat(4_000)
    await setNote(unicodeNote)
    await act(async () => {
      record()?.click()
      record()?.click()
      await Promise.resolve()
    })
    assert.equal(reviewBodies.length, 1, 'duplicate save actions share one immutable owner')
    assert.deepEqual(reviewBodies[0], { status: 'confirmed', note: unicodeNote })

    await act(async () => {
      resolveReview?.()
      await flush()
      await flush()
    })
    assert.equal(note(), null, 'a committed review clears only its own draft')
    assert.match(browser.document.querySelector('[role="alert"]')?.textContent ?? '', /refresh unavailable/)
    assert.match(
      container.textContent ?? '',
      /UNICODE/,
      'a failed post-commit reload keeps the last owned evidence visible'
    )
  } finally {
    api.eventSecurity.fusedBreakdown = originalBreakdown
    api.eventSecurity.reviewFinding = originalReview
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
