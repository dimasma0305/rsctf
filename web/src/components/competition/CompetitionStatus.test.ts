import { HeadlessMantineProvider } from '@mantine/core'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { I18nextProvider } from 'react-i18next'
import type { AdStateModel } from '@Api'
import { CompetitionStatus } from './CompetitionStatus'

const now = 100_000
const state: AdStateModel = {
  currentRound: 178,
  startRound: 42,
  epochTicks: 8,
  roundEndsAt: now - 1000,
  flagsReady: true,
  flagDeliveryFailures: 17,
  scoringPaused: false,
  services: [],
}
const render = async (snapshot: AdStateModel | undefined, hasAd = true) => {
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', resources: { en: { translation: {} } } })
  return renderToStaticMarkup(
    createElement(
      HeadlessMantineProvider,
      null,
      createElement(
        I18nextProvider,
        { i18n },
        createElement(CompetitionStatus, {
          state: snapshot,
          nowMs: now,
          hasAd,
          hasKoth: true,
          onAdToolkit() {},
          onKothToolkit() {},
        })
      )
    )
  )
}

test('round strip preserves reported epoch and flag failures without presenting a stale zero as a live countdown', async () => {
  const html = await render(state)
  assert.match(html, /<dd>178<\/dd>/)
  assert.match(html, /<dd>18<\/dd>/)
  assert.match(html, /<dd>1\/8<\/dd>/)
  assert.match(html, /17 flag deliveries need attention/)
  assert.match(html, /Awaiting round update/)
  assert.doesNotMatch(html, />0s</)
  assert.match(html, /role="status"/)
  assert.match(html, /A&amp;D Toolkit/)
  assert.match(html, /KotH Toolkit/)
})

test('round strip keeps paused, warmup, loading and KotH-only states honest', async () => {
  const paused = await render({ ...state, scoringPaused: true, roundEndsAt: now + 40_000, scoringPausedAt: now })
  assert.match(paused, /40s/)
  assert.match(paused, /Scoring paused/)
  assert.doesNotMatch(paused, /Awaiting round update/)
  const loading = await render(undefined)
  assert.doesNotMatch(loading, /Warmup|17 flag deliveries/)
  const warmup = await render({
    ...state,
    currentRound: 0,
    startRound: null,
    flagDeliveryFailures: 0,
    roundEndsAt: null,
  })
  assert.match(warmup, /Warmup/)
  assert.doesNotMatch(warmup, /Awaiting round update|<dt>Epoch/)
  const koth = await render(state, false)
  assert.doesNotMatch(koth, /flag deliveries|A&amp;D Toolkit|<dt>Epoch/)
})

test('round presentation cannot add polling or duplicate route-owned state reads', () => {
  const source = readFileSync('src/components/competition/CompetitionStatus.tsx', 'utf8')
  assert.doesNotMatch(source, /useAdState|useSWR|setInterval|setTimeout|fetch\(/)
  const page = readFileSync('src/pages/games/[id]/Challenges.tsx', 'utf8')
  assert.match(page, /!archived && hasAdEngine && \(\s*<CompetitionStatus/)
  assert.match(page, /state=\{adState\}/)
})
