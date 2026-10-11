import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { createArenaTheater } from '../pages/games/[id]/arenaTheater'
import { installTestDom } from '../test/installDom'

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

const mount = () => {
  const host = document.createElement('div')
  host.innerHTML = '<div class="midrow"><button id="fsBtn">⛶</button><div id="arena" tabindex="0"></div></div>'
  document.body.append(host)
  return {
    host,
    stage: host.querySelector<HTMLElement>('.midrow')!,
    button: host.querySelector<HTMLButtonElement>('#fsBtn')!,
  }
}

test('theater overlay locks scrolling, recedes when idle, and Escape returns focus to its toggle', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack' })
  const restore = installTestDom(browser)
  try {
    const { host, stage, button } = mount()
    const changes: boolean[] = []
    const theater = createArenaTheater({
      host,
      stage,
      button,
      document,
      onChange: (on) => changes.push(on),
      idleMs: 20,
    })
    theater.enter()
    assert.equal(theater.isOn(), true)
    assert.equal(stage.classList.contains('theater'), true)
    assert.equal(host.dataset.theater, 'on')
    assert.equal(button.getAttribute('aria-label'), 'Exit theater mode')
    assert.equal(button.getAttribute('aria-pressed'), 'true')
    assert.equal(document.documentElement.style.overflow, 'hidden')
    assert.equal(stage.dataset.idle, 'false')
    await sleep(40)
    assert.equal(stage.dataset.idle, 'true')
    document.dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'Tab', bubbles: true }))
    assert.equal(stage.dataset.idle, 'false', 'a key anywhere wakes the chrome')
    await sleep(40)
    assert.equal(stage.dataset.idle, 'true')
    stage.dispatchEvent(new browser.Event('pointermove', { bubbles: true }))
    assert.equal(stage.dataset.idle, 'false')
    theater.enter()
    assert.deepEqual(changes, [true], 'entering twice is a no-op')

    document.dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    assert.equal(theater.isOn(), false)
    assert.equal(stage.classList.contains('theater'), false)
    assert.equal(host.dataset.theater, 'off')
    assert.equal(button.getAttribute('aria-label'), 'Enter theater mode')
    assert.equal(document.documentElement.style.overflow, '')
    assert.equal('idle' in stage.dataset, false)
    assert.equal(document.activeElement, button)
    assert.deepEqual(changes, [true, false])
    theater.destroy()
  } finally {
    restore()
  }
})

test('native fullscreen is an enhancement: leaving it from the browser side also leaves theater', () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack' })
  const restore = installTestDom(browser)
  try {
    const { host, stage, button } = mount()
    let current: Element | null = null
    const calls: string[] = []
    const theater = createArenaTheater({
      host,
      stage,
      button,
      document,
      onChange: () => {},
      fullscreen: {
        request: (element) => {
          calls.push('request')
          current = element
        },
        exit: () => {
          calls.push('exit')
          current = null
        },
        current: () => current,
      },
    })
    theater.enter()
    assert.deepEqual(calls, ['request'])
    assert.equal(document.documentElement.style.overflow, '', 'native fullscreen needs no scroll lock')
    document.dispatchEvent(new browser.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    assert.equal(theater.isOn(), true, 'the browser owns Escape while natively fullscreen')
    current = null
    document.dispatchEvent(new browser.Event('fullscreenchange'))
    assert.equal(theater.isOn(), false)
    assert.equal(document.activeElement, button)

    theater.enter()
    theater.exit()
    assert.deepEqual(calls, ['request', 'request', 'exit'])
    assert.equal(theater.isOn(), false)
    theater.destroy()
  } finally {
    restore()
  }
})

test('without native fullscreen the stage rides the popover top layer until theater ends', () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack' })
  const restore = installTestDom(browser)
  try {
    const { host, stage, button } = mount()
    const calls: string[] = []
    Object.assign(stage, {
      showPopover: () => calls.push('show'),
      hidePopover: () => calls.push('hide'),
    })
    const theater = createArenaTheater({ host, stage, button, document, onChange: () => {} })
    theater.enter()
    assert.deepEqual(calls, ['show'])
    assert.equal(stage.getAttribute('popover'), 'manual')
    assert.equal(document.documentElement.style.overflow, 'hidden')
    theater.exit()
    assert.deepEqual(calls, ['show', 'hide'])
    assert.equal(stage.hasAttribute('popover'), false)
    assert.equal(document.documentElement.style.overflow, '')
    theater.destroy()
  } finally {
    restore()
  }
})

test('a rejected fullscreen request falls back to the overlay instead of a clipped stage', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/27/attack' })
  const restore = installTestDom(browser)
  try {
    const { host, stage, button } = mount()
    const calls: string[] = []
    Object.assign(stage, {
      showPopover: () => calls.push('show'),
      hidePopover: () => calls.push('hide'),
    })
    const theater = createArenaTheater({
      host,
      stage,
      button,
      document,
      onChange: () => {},
      fullscreen: {
        request: () => Promise.reject(new Error('not a user gesture')),
        exit: () => calls.push('exit'),
        current: () => null,
      },
    })
    theater.enter()
    await Promise.resolve()
    await Promise.resolve()
    assert.deepEqual(calls, ['show'])
    assert.equal(document.documentElement.style.overflow, 'hidden')
    theater.exit()
    assert.deepEqual(calls, ['show', 'hide'], 'no native exit when fullscreen never engaged')
    theater.destroy()
  } finally {
    restore()
  }
})
