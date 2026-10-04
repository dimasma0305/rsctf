import { HeadlessMantineProvider, Modal } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import en from '../locales/en-US/challenge.json'
import { installTestDom } from '../test/installDom'
import { contrastRatio } from '../utils/ThemeContrast'
import { FlagVerdictOverlay } from './FlagVerdictOverlay'

test('verdict dialog has a visible title, immediate actions, and honest score presentation', async () => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restore = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en-US', resources: { 'en-US': { translation: { challenge: en } } } })
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  let dismissals = 0
  const render = (kind: 'success' | 'wrong', sequence: number, score?: number) =>
    createElement(
      HeadlessMantineProvider,
      null,
      createElement(
        I18nextProvider,
        { i18n },
        createElement(
          Modal.Root,
          {
            opened: true,
            onClose: () => undefined,
            closeOnEscape: false,
            withinPortal: false,
            transitionProps: { duration: 0 },
          },
          createElement(
            Modal.Content,
            null,
            createElement(FlagVerdictOverlay, {
              key: sequence,
              verdict: { kind, sequence },
              challengeTitle: 'Tower <not markup>',
              score,
              onDismiss: () => {
                dismissals++
              },
            })
          )
        )
      )
    )
  try {
    await act(async () => root.render(render('success', 1, 500)))
    const dialog = browser.document.querySelector('[role="dialog"]')
    const title = browser.document.getElementById(dialog?.getAttribute('aria-labelledby') ?? '')
    assert.equal(title?.textContent, 'Flag Accepted')
    assert.notEqual(title?.getAttribute('aria-hidden'), 'true')
    assert.match(dialog?.textContent ?? '', /Challenge value500 pts/)
    assert.match(dialog?.textContent ?? '', /Tower <not markup>/)
    assert.equal(dialog?.querySelector('not'), null)
    const action = dialog?.querySelector<HTMLButtonElement>('button[data-autofocus]')
    assert.equal(browser.document.activeElement, action)
    assert.equal(action?.disabled, false)
    await act(async () => action?.click())
    assert.equal(dismissals, 1, 'dismissal never waits for animation completion')
    assert.equal(dialog?.querySelectorAll('i').length, 12)

    await act(async () => root.render(render('wrong', 2, 500)))
    assert.match(browser.document.querySelector('[role="dialog"]')?.textContent ?? '', /Flag Denied/)
    assert.doesNotMatch(browser.document.querySelector('[role="dialog"]')?.textContent ?? '', /Challenge value|500/)
    assert.equal(browser.document.querySelectorAll('[data-flag-verdict] i').length, 0)
    assert.equal(browser.document.activeElement?.textContent, 'Try again')
    await act(async () =>
      browser.document.activeElement?.dispatchEvent(
        new browser.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })
      )
    )
    assert.equal(dismissals, 2)
    await act(async () =>
      browser.document.querySelector<HTMLButtonElement>('button[aria-label="Close result"]')?.click()
    )
    assert.equal(dismissals, 3)
    for (const score of [undefined, NaN, Infinity]) {
      await act(async () => root.render(render('success', 3, score)))
      assert.doesNotMatch(
        browser.document.querySelector('[role="dialog"]')?.textContent ?? '',
        /Challenge value|NaN|Infinity/
      )
    }
    await act(async () => root.render(render('success', 4, 0)))
    assert.match(browser.document.querySelector('[role="dialog"]')?.textContent ?? '', /Challenge value0 pts/)
  } finally {
    await act(async () => root.unmount())
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restore()
  }
})

test('verdict decoration is finite, opt-in motion and never delays text or actions', () => {
  const source = readFileSync('src/components/FlagVerdictOverlay.tsx', 'utf8')
  const css = readFileSync('src/styles/components/FlagVerdictOverlay.module.css', 'utf8')
  assert.doesNotMatch(source, /setTimeout|setInterval|requestAnimationFrame|Math.random|Audio\(/)
  assert.match(source, /removeScrollProps=\{\{ removeScrollBar: false \}\}/)
  assert.doesNotMatch(source, /lockScroll=\{false\}/)
  assert.match(css, /prefers-reduced-motion: no-preference/)
  assert.match(css, /prefers-reduced-motion: reduce/)
  assert.doesNotMatch(css, /infinite|filter: blur|height: 100dvh/)
  for (const match of css.matchAll(/animation:\s*([^;]+);/g)) {
    assert.doesNotMatch(match[1], /[\d.]s\b/)
    const times = [...match[1].matchAll(/([\d.]+)ms/g)].map((time) => Number(time[1]))
    assert.ok(times.length > 0 && times.reduce((a, b) => a + b, 0) <= 800)
  }
  for (const name of ['title', 'description', 'actionButton']) {
    const block = css.match(new RegExp(`\\.${name} \\{([\\s\\S]*?)\\n\\}`))?.[1]
    assert.ok(block)
    assert.doesNotMatch(block, /opacity:\s*0|animation:/)
  }
})

test('verdict light and dark accents keep text and actions contrast-safe', () => {
  for (const [accent, onAccent, surface] of [
    ['#087f5b', '#ffffff', '#ffffff'],
    ['#5eead4', '#092721', '#151f2e'],
    ['#c2255c', '#ffffff', '#ffffff'],
    ['#fda4af', '#3b101b', '#151f2e'],
  ]) {
    assert.ok(contrastRatio(accent, surface) >= 4.5)
    assert.ok(contrastRatio(onAccent, accent) >= 4.5)
  }
})

test('retry and success focus targets agree with the reactivated drawer focus trap', () => {
  const source = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
  assert.match(source, /data-autofocus=\{\(focusAfterVerdict === 'wrong' && !inputDisabled\) \|\| undefined\}/)
  assert.match(source, /data-autofocus=\{focusAfterVerdict === 'success' \|\| undefined\}/)
  assert.match(source, /setFocusAfterVerdict\(null\)\s*\}, \[challenge\?\.id, modalProps.opened\]\)/)
})
