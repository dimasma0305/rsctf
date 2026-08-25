#!/usr/bin/env node

import assert from 'node:assert/strict'
import { launchBrowser } from './cdp.mjs'

const viewports = {
  desktop: { width: 1440, height: 1100, mobile: false },
  mobile: { width: 390, height: 844, mobile: true },
  compact: { width: 320, height: 568, mobile: true },
}

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds))

const safeOrigin = (value) => {
  const url = new URL(value)
  if (url.username || url.password || url.search || url.hash || !['', '/'].includes(url.pathname)) {
    throw new Error('guide target must be an origin without credentials, path, query, or fragment')
  }
  if (url.protocol !== 'https:' && !['localhost', '127.0.0.1', '::1'].includes(url.hostname)) {
    throw new Error('non-local guide targets must use HTTPS')
  }
  return url.origin
}

const parseArguments = () => {
  const options = {
    target: process.env.RSCTF_VISUAL_TARGET || 'http://127.0.0.1:8080',
    viewport: 'mobile',
  }
  const argumentsList = process.argv.slice(2)
  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index]
    if (argument === '--') continue
    if (argument === '--target') options.target = argumentsList[++index]
    else if (argument === '--viewport') options.viewport = argumentsList[++index]
    else if (argument === '--help' || argument === '-h') {
      console.log('Usage: node tests/visual/guide-journey.mjs --target URL --viewport desktop|mobile|compact')
      process.exit(0)
    } else throw new Error(`unknown argument: ${argument}`)
  }
  if (!viewports[options.viewport]) throw new Error(`unknown viewport: ${options.viewport}`)
  return { ...options, target: safeOrigin(options.target) }
}

const main = async () => {
  const options = parseArguments()
  const viewport = viewports[options.viewport]
  const { cdp, close } = await launchBrowser()

  const evaluate = async (expression) => {
    const result = await cdp.send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
    })
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.text || 'browser evaluation failed')
    return result.result.value
  }
  const waitFor = async (expression, description, timeoutMilliseconds = 12_000) => {
    const deadline = Date.now() + timeoutMilliseconds
    while (Date.now() < deadline) {
      if (await evaluate(expression)) return
      await sleep(100)
    }
    throw new Error(`timed out waiting for ${description}`)
  }
  const clickTarget = async (target) => {
    const clicked = await evaluate(`(() => {
      const target = ${JSON.stringify(target)}
      const element = [...document.querySelectorAll('[data-guide="' + CSS.escape(target) + '"]')].find((candidate) => {
        const rectangle = candidate.getBoundingClientRect()
        const style = getComputedStyle(candidate)
        return rectangle.width > 0 && rectangle.height > 0 && style.display !== 'none' && style.visibility !== 'hidden'
      })
      if (!element) return false
      element.click()
      return true
    })()`)
    assert.equal(clicked, true, `${target} must be clickable`)
  }
  const clickHref = async (href) => {
    const clicked = await evaluate(`(() => {
      const href = ${JSON.stringify(href)}
      const element = [...document.querySelectorAll('a[href]')].find(
        (candidate) => candidate.getAttribute('href') === href
      )
      if (!element) return false
      element.click()
      return true
    })()`)
    assert.equal(clicked, true, `${href} must be reachable`)
  }
  const snapshot = () =>
    evaluate(`(() => {
      const surface = document.querySelector('[data-guide-surface="coachmark"]')
      const cursor = document.querySelector('[data-guide-layer="cursor"]')
      const spotlight = document.querySelector('[data-guide-layer="spotlight"]')
      if (!surface || !cursor || !spotlight) return null
      const surfaceRectangle = surface.getBoundingClientRect()
      const spotlightRectangle = spotlight.getBoundingClientRect()
      const boundaries = [...document.querySelectorAll('[data-guide-boundary]')]
        .filter((boundary) => {
          const rectangle = boundary.getBoundingClientRect()
          return !(
            surfaceRectangle.right <= rectangle.left ||
            surfaceRectangle.left >= rectangle.right ||
            surfaceRectangle.bottom <= rectangle.top ||
            surfaceRectangle.top >= rectangle.bottom
          )
        })
        .map((boundary) => boundary.getAttribute('data-guide-boundary'))
      const center = document.elementsFromPoint(
        spotlightRectangle.left + spotlightRectangle.width / 2,
        spotlightRectangle.top + spotlightRectangle.height / 2
      ).find((element) => !element.closest('[data-guide-layer]') && !element.closest('[data-guide-surface]'))
      const preferences = JSON.parse(localStorage.getItem('rsctf-player-guide:guest') || 'null')
      const activeTarget = [...document.querySelectorAll('[data-guide="' + CSS.escape(surface.dataset.guideTarget || '') + '"]')]
        .find((candidate) => candidate.getBoundingClientRect().width > 0)
      const scrollAncestors = []
      for (let parent = activeTarget?.parentElement; parent; parent = parent.parentElement) {
        if (parent.scrollHeight > parent.clientHeight + 1) {
          scrollAncestors.push({
            className: String(parent.className).slice(0, 100),
            clientHeight: parent.clientHeight,
            scrollHeight: parent.scrollHeight,
            scrollTop: parent.scrollTop,
          })
        }
      }
      const overlapWidth = Math.max(
        0,
        Math.min(surfaceRectangle.right, spotlightRectangle.right) -
          Math.max(surfaceRectangle.left, spotlightRectangle.left)
      )
      const overlapHeight = Math.max(
        0,
        Math.min(surfaceRectangle.bottom, spotlightRectangle.bottom) -
          Math.max(surfaceRectangle.top, spotlightRectangle.top)
      )
      const spotlightArea = spotlightRectangle.width * spotlightRectangle.height
      return {
        path: location.pathname,
        step: preferences?.activeTourStep ?? null,
        target: surface.dataset.guideTarget ?? null,
        placement: surface.dataset.guidePlacement ?? null,
        cursorVisible: cursor.getBoundingClientRect().width > 0,
        pointerTarget: center?.closest('[data-guide]')?.getAttribute('data-guide') ?? null,
        areaRatio: (surfaceRectangle.width * surfaceRectangle.height) / (innerWidth * innerHeight),
        targetVisibleRatio:
          spotlightArea > 0 ? 1 - (overlapWidth * overlapHeight) / spotlightArea : 0,
        surfaceRectangle: {
          top: Math.round(surfaceRectangle.top),
          bottom: Math.round(surfaceRectangle.bottom),
        },
        targetRectangle: {
          top: Math.round(spotlightRectangle.top),
          bottom: Math.round(spotlightRectangle.bottom),
        },
        scrollAncestors,
        controlsInsideViewport: [...surface.querySelectorAll('button, a[href]')].every((control) => {
          const rectangle = control.getBoundingClientRect()
          return rectangle.left >= 0 && rectangle.top >= 0 && rectangle.right <= innerWidth && rectangle.bottom <= innerHeight
        }),
        boundaryOverlaps: boundaries,
      }
    })()`)
  const requireCheckpoint = async (step, target) => {
    await waitFor(
      `(() => {
        const surface = document.querySelector('[data-guide-surface="coachmark"]')
        const preferences = JSON.parse(localStorage.getItem('rsctf-player-guide:guest') || 'null')
        return preferences?.activeTourStep === ${JSON.stringify(step)} &&
          surface?.dataset.guideTarget === ${JSON.stringify(target)} &&
          Boolean(document.querySelector('[data-guide-layer="cursor"]'))
      })()`,
      `${step} checkpoint on ${target}`
    )
    await sleep(600)
    const state = await snapshot()
    console.log(JSON.stringify(state))
    assert.ok(state, 'guide HUD must be visible')
    assert.equal(state.step, step)
    assert.equal(state.target, target)
    assert.equal(state.pointerTarget, target)
    assert.equal(state.cursorVisible, true)
    const areaBudget = viewport.width <= 320 ? 0.45 : viewport.width <= 768 ? 0.34 : 0.25
    assert.ok(state.areaRatio <= areaBudget, `guide area ${state.areaRatio} must stay within ${areaBudget}`)
    assert.ok(state.targetVisibleRatio >= 0.9, 'coachmark must not cover the highlighted target')
    assert.equal(state.controlsInsideViewport, true)
    assert.deepEqual(state.boundaryOverlaps, [])
  }
  const requireAccountFormUsable = async (targetName) => {
    const state = await evaluate(`(() => {
      const form = document.querySelector('[data-guide-interaction-scope]')
      if (!form) return null
      const target = form.querySelector('[data-guide="' + CSS.escape(${JSON.stringify(targetName)}) + '"]')
      if (!target) return null
      const controls = [target, ...form.querySelectorAll('input, button, a[href]')]
        .filter((control, index, all) => all.indexOf(control) === index)
        .filter((control) => control === target || (!target.contains(control) && !control.contains(target)))
        .filter((control) => {
          const rectangle = control.getBoundingClientRect()
          return rectangle.width > 0 && rectangle.height > 0 && rectangle.bottom > 0 && rectangle.top < innerHeight
        })
        .slice(0, 2)
      return controls.map((control) => {
        const rectangle = control.getBoundingClientRect()
        const topElement = document.elementsFromPoint(
          rectangle.left + rectangle.width / 2,
          rectangle.top + rectangle.height / 2
        ).find((element) => !element.closest('[data-guide-layer]') && !element.closest('[data-guide-surface]'))
        return {
          insideViewport: rectangle.top >= 0 && rectangle.bottom <= innerHeight,
          pointerAccessible: topElement === control || control.contains(topElement),
        }
      })
    })()`)
    assert.ok(state, 'account form interaction scope must exist')
    assert.equal(state.length, 2, 'account guide must expose two usable account controls')
    assert.ok(
      state.every((control) => control.insideViewport),
      'account controls must remain in the viewport'
    )
    assert.ok(
      state.every((control) => control.pointerAccessible),
      'guide layers must not block account controls'
    )
  }
  const currentAccountTarget = async () => {
    await waitFor(
      `(() => {
        const target = document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget
        return target === 'account-access' || target === 'account-oauth'
      })()`,
      'a configured account action'
    )
    return evaluate(`document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget`)
  }

  try {
    await Promise.all([cdp.send('Page.enable'), cdp.send('Runtime.enable')])
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: viewport.width,
      height: viewport.height,
      deviceScaleFactor: 1,
      mobile: viewport.mobile,
    })
    await cdp.send('Page.navigate', { url: `${options.target}/games` })

    if (viewport.mobile) {
      await requireCheckpoint('welcome', 'more-navigation')
      await clickTarget('more-navigation')
    }
    await requireCheckpoint('welcome', 'guide-navigation')
    await clickTarget('guide-navigation')

    const accountLauncher = viewport.mobile ? 'more-navigation' : 'account-menu'
    await requireCheckpoint('account', accountLauncher)
    await clickTarget(accountLauncher)
    await requireCheckpoint('account', 'account-login')
    await clickTarget('account-login')
    const loginTarget = await currentAccountTarget()
    await requireCheckpoint('account', loginTarget)
    await requireAccountFormUsable(loginTarget)

    await clickHref('/account/register')
    const registerTarget = await currentAccountTarget()
    await requireCheckpoint('account', registerTarget)
    await requireAccountFormUsable(registerTarget)

    const finalState = await snapshot()
    assert.equal(finalState.path, '/account/register')
    console.log(`Guide journey passed: ${options.viewport} ${options.target}`)
  } finally {
    await close()
  }
}

await main()
