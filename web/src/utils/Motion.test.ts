import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { DIALOG_TRANSITION, DRAWER_TRANSITION, POPOVER_TRANSITION, MOTION_EASE } from './Motion'

test('shared transitions are short, interruptible and leave lifecycle ownership to Mantine', () => {
  for (const transition of [DIALOG_TRANSITION, DRAWER_TRANSITION, POPOVER_TRANSITION]) {
    assert.ok(transition.duration > 0 && transition.duration <= 220)
    assert.ok(transition.exitDuration > 0 && transition.exitDuration <= transition.duration)
    assert.equal(transition.timingFunction, MOTION_EASE)
  }
  assert.equal(DIALOG_TRANSITION.transition.transitionProperty, 'opacity, transform')
  const theme = readFileSync('src/utils/ThemeOverride.ts', 'utf8')
  assert.match(theme, /respectReducedMotion: true/)
  assert.match(theme, /ModalRoot: Modal.Root.extend/)
  assert.doesNotMatch(theme, /blur: [1-9]/)
  const policy = readFileSync('src/utils/Motion.ts', 'utf8')
  assert.doesNotMatch(policy, /setTimeout|requestAnimationFrame|setInterval|onExited|onEntered/)
})

test('content entrances never hide cards, move page geometry or animate each result row', () => {
  const css = readFileSync('src/styles/Motion.css', 'utf8')
  assert.match(css, /prefers-reduced-motion: no-preference/)
  assert.match(css, /prefers-reduced-motion: reduce/)
  assert.match(css, /animation: none/)
  assert.doesNotMatch(
    css,
    /opacity:\s*0[;}]|visibility:|display:|animation-fill-mode:|animation-delay:|infinite|will-change:/
  )
  assert.doesNotMatch(css, /mantine-Card-root|mantine-Table-tr|nth-child|transition: all/)
  const page = css.match(/@keyframes rsctf-content-in \{([\s\S]*?)\n\}/)?.[1]
  assert.ok(page)
  assert.doesNotMatch(page, /transform|height|width|margin|padding/)
  const navbar = readFileSync('src/components/WithNavbar.tsx', 'utf8')
  assert.match(navbar, /transitionDuration=\{0\}/)
  assert.match(navbar, /data-motion=\{competition \? undefined : 'page'\}/)
  const event = readFileSync('src/components/WithGameTab.tsx', 'utf8')
  assert.ok(event.indexOf('data-motion="page"') > event.indexOf('data-event-workspace-header'))
  assert.match(event, /data-motion="page">\s*\{children\}/)
  const verdict = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
  assert.doesNotMatch(verdict, /if \((?:embedded|drawer) && !flagVerdict\)/)
  assert.equal((verdict.match(/opened=\{modalProps.opened && presentationMounted\}/g) ?? []).length, 2)
  assert.match(verdict, /requestAnimationFrame\(\(\) => setPresentationMounted\(true\)\)/)
  assert.match(verdict, /return \(\) => window.cancelAnimationFrame\(frame\)/)
})
