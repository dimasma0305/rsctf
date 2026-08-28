import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api, { type AntiCheatFindingRow, type FusedEvidenceBreakdown } from '../../Api'
import { installTestDom } from '../../test/installDom'
import { FusedEvidencePanel } from './FusedEvidencePanel'

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
