#!/usr/bin/env node

import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, relative, resolve, sep } from 'node:path'
import { pathToFileURL } from 'node:url'
import { launchBrowser } from './cdp.mjs'
import {
  discoverPageRoutes,
  parseRouteShard,
  repositoryRoot,
  selectRouteShard,
  validatePageRoutes,
  viewportCatalog,
} from './route-catalog.mjs'

const sleep = (milliseconds) => new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds))
const MAX_SCREENSHOT_HEIGHT = 32_000
const MAX_SCREENSHOT_WIDTH = 3_840

function parseArguments(argv) {
  const options = {
    target: process.env.RSCTF_VISUAL_TARGET || 'http://127.0.0.1:8080',
    output: process.env.RSCTF_VISUAL_OUTPUT || join(repositoryRoot, 'visual-audit-output'),
    pageFilters: [],
    shard: undefined,
    viewports: Object.keys(viewportCatalog),
    explicitViewports: false,
    list: false,
  }
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === '--') continue
    if (argument === '--target') options.target = argv[++index]
    else if (argument === '--output') options.output = argv[++index]
    else if (argument === '--page') options.pageFilters.push(argv[++index])
    else if (argument === '--shard') options.shard = parseRouteShard(argv[++index])
    else if (argument === '--viewport') {
      if (!options.explicitViewports) options.viewports = []
      options.explicitViewports = true
      options.viewports.push(argv[++index])
    } else if (argument === '--desktop-only') options.viewports = ['desktop']
    else if (argument === '--mobile-only') options.viewports = ['mobile', 'compact']
    else if (argument === '--list') options.list = true
    else if (argument === '--help' || argument === '-h') {
      console.log(`Usage: node tests/visual/audit.mjs [options]

Options:
  --target URL       RSCTF origin (default http://127.0.0.1:8080)
  --output PATH      Generated artifact directory (relative to the repository root)
  --page FILTER      Audit route name/path containing FILTER; repeatable
  --shard INDEX/TOTAL
                      Audit one deterministic route shard (for example 1/2)
  --viewport NAME    Audit ultrawide, wide, desktop, notebook, laptop, tablet, mobile, or compact; repeatable
  --desktop-only     Capture only 1440x1100
  --mobile-only      Capture 390x844 and compact 320x568
  --list             List resolved routes without launching Chromium
`)
      process.exit(0)
    } else {
      throw new Error(`unknown argument: ${argument}`)
    }
  }
  options.viewports = [...new Set(options.viewports)]
  const unknownViewports = options.viewports.filter((name) => !viewportCatalog[name])
  if (unknownViewports.length) throw new Error(`unknown viewport: ${unknownViewports.join(', ')}`)
  return options
}

function numericContext(name, placeholder, listMode) {
  const value = process.env[name]
  if (listMode && !value) return placeholder
  const number = Number(value)
  return Number.isSafeInteger(number) && number > 0 ? number : undefined
}

function secret(name) {
  const file = process.env[`${name}_FILE`]
  if (file) {
    const value = readFileSync(file, 'utf8').trim()
    if (!value) throw new Error(`${name}_FILE is empty`)
    return value
  }
  return process.env[name]?.trim() || ''
}

function safeOrigin(value) {
  const url = new URL(value)
  if (url.username || url.password || url.search || url.hash) {
    throw new Error('visual target must be an origin without credentials, query, or fragment')
  }
  if (url.pathname !== '/' && url.pathname !== '') throw new Error('visual target must not contain a path')
  if (url.protocol !== 'https:' && !['localhost', '127.0.0.1', '::1'].includes(url.hostname)) {
    throw new Error('non-local visual targets must use HTTPS')
  }
  return url.origin
}

function accessibleDocumentAnalysis() {
  const roots = [document]
  for (let index = 0; index < roots.length; index += 1) {
    for (const element of roots[index].querySelectorAll('*')) {
      if (element.shadowRoot) roots.push(element.shadowRoot)
    }
  }
  const queryAll = (selector) => roots.flatMap((root) => [...root.querySelectorAll(selector)])
  const byId = (id) => roots.map((root) => root.querySelector(`#${CSS.escape(id)}`)).find(Boolean)
  const main = document.querySelector('#main-content')
  const parentAcrossShadow = (element) => element.parentElement ?? element.getRootNode()?.host ?? null
  const belongsToMain = (element) => {
    for (let current = element; current; current = parentAcrossShadow(current)) {
      if (current === main) return true
    }
    return false
  }
  const visible = (element) => {
    const rectangle = element.getBoundingClientRect()
    const style = getComputedStyle(element)
    return (
      rectangle.width > 0 &&
      rectangle.height > 0 &&
      style.display !== 'none' &&
      style.visibility !== 'hidden' &&
      Number(style.opacity) !== 0
    )
  }
  const visuallyHidden = (element) => {
    const rectangle = element.getBoundingClientRect()
    const style = getComputedStyle(element)
    return (
      rectangle.width <= 1 &&
      rectangle.height <= 1 &&
      (style.clip !== 'auto' || style.clipPath !== 'none') &&
      ['hidden', 'clip'].includes(style.overflow)
    )
  }
  const accessibleName = (element) => {
    const labelledBy = (element.getAttribute('aria-labelledby') ?? '')
      .split(/\s+/)
      .filter(Boolean)
      .map((id) => byId(id)?.textContent?.trim() ?? '')
      .join(' ')
      .trim()
    const labels = [...(element.labels ?? [])]
      .map((label) => label.textContent?.trim() ?? '')
      .join(' ')
      .trim()
    const imageAlt = element.querySelector?.('img[alt]')?.getAttribute('alt') ?? ''
    return (
      element.getAttribute('aria-label') ||
      labelledBy ||
      labels ||
      element.getAttribute('title') ||
      element.textContent?.trim() ||
      imageAlt ||
      element.getAttribute('placeholder') ||
      ''
    )
  }
  const focusable = [
    ...queryAll('a[href], button, input, select, textarea, summary, [role="button"], [role="link"], [tabindex]'),
  ].filter(
    (element) =>
      visible(element) && element.tabIndex >= 0 && !element.disabled && element.getAttribute('aria-hidden') !== 'true'
  )
  const rectangles = focusable.map((element) => {
    const rectangle = element.getBoundingClientRect()
    return {
      element,
      left: rectangle.left,
      top: rectangle.top,
      right: rectangle.right,
      bottom: rectangle.bottom,
      width: rectangle.width,
      height: rectangle.height,
      centerX: rectangle.left + rectangle.width / 2,
      centerY: rectangle.top + rectangle.height / 2,
    }
  })
  const unnamedControls = focusable
    .filter((element) => !accessibleName(element))
    .map((element) => element.outerHTML.slice(0, 220))
  const crowdedSmallTargets = rectangles
    .filter(({ element }) => !visuallyHidden(element))
    .filter(({ width, height }) => width < 24 || height < 24)
    .filter((target) =>
      rectangles.some(
        (other) => other !== target && Math.hypot(target.centerX - other.centerX, target.centerY - other.centerY) < 24
      )
    )
    .map(({ element, width, height }) => ({
      name: accessibleName(element).slice(0, 100),
      width: Math.round(width),
      height: Math.round(height),
    }))
  const overlaps = []
  for (let leftIndex = 0; leftIndex < rectangles.length; leftIndex += 1) {
    for (let rightIndex = leftIndex + 1; rightIndex < rectangles.length; rightIndex += 1) {
      const left = rectangles[leftIndex]
      const right = rectangles[rightIndex]
      if (left.element.contains(right.element) || right.element.contains(left.element)) continue
      const width = Math.max(0, Math.min(left.right, right.right) - Math.max(left.left, right.left))
      const height = Math.max(0, Math.min(left.bottom, right.bottom) - Math.max(left.top, right.top))
      const intersection = width * height
      const smaller = Math.min(left.width * left.height, right.width * right.height)
      if (smaller > 0 && intersection / smaller > 0.5) {
        overlaps.push({
          first: accessibleName(left.element).slice(0, 80),
          second: accessibleName(right.element).slice(0, 80),
        })
      }
    }
  }
  const sectionGaps = queryAll('[data-min-block-gap]')
    .filter(visible)
    .flatMap((element) => {
      const next = element.nextElementSibling
      const required = Number(element.getAttribute('data-min-block-gap'))
      if (!next || !visible(next) || !Number.isFinite(required) || required < 0) return []
      const currentRectangle = element.getBoundingClientRect()
      const nextRectangle = next.getBoundingClientRect()
      return [
        {
          section: element.getAttribute('data-layout-section') || element.tagName.toLowerCase(),
          required,
          actual: Math.round((nextRectangle.top - currentRectangle.bottom) * 100) / 100,
        },
      ]
    })
  const layoutRows = queryAll('[data-max-layout-rows]')
    .filter(visible)
    .map((element) => {
      const rowOffsets = []
      const children = [...element.children].filter(visible)
      for (const child of children) {
        const top = Math.round(child.getBoundingClientRect().top)
        if (!rowOffsets.some((other) => Math.abs(other - top) <= 2)) rowOffsets.push(top)
      }
      return {
        section: element.getAttribute('data-layout-section') || element.tagName.toLowerCase(),
        maximum: Number(element.getAttribute('data-max-layout-rows')),
        actual: rowOffsets.length,
        overflowingChildren: children.filter((child) => child.scrollWidth > child.clientWidth + 1).length,
      }
    })

  const headings = queryAll('h1, h2, h3, h4, h5, h6')
    .filter(visible)
    .map((heading) => ({
      level: Number(heading.tagName.slice(1)),
      text: heading.textContent?.trim().slice(0, 140) ?? '',
    }))
  const headingSkips = []
  for (let index = 1; index < headings.length; index += 1) {
    if (headings[index].level > headings[index - 1].level + 1) {
      headingSkips.push(`${headings[index - 1].level}->${headings[index].level}: ${headings[index].text}`)
    }
  }

  const clippedText = queryAll('main *, [role="main"] *')
    .filter(visible)
    .filter((element) => !visuallyHidden(element))
    .filter((element) => element.children.length === 0 && (element.textContent?.trim().length ?? 0) > 8)
    .filter((element) => {
      const style = getComputedStyle(element)
      const clips = ['hidden', 'clip'].includes(style.overflowX) || ['hidden', 'clip'].includes(style.overflowY)
      return clips && (element.scrollWidth > element.clientWidth + 2 || element.scrollHeight > element.clientHeight + 2)
    })
    .filter((element) => !element.getAttribute('title') && !element.getAttribute('aria-label'))
    .slice(0, 20)
    .map((element) => element.textContent.trim().slice(0, 140))

  const mainElements = queryAll('*').filter(belongsToMain)
  const containedHorizontally = (element) => {
    for (let parent = parentAcrossShadow(element); parent && parent !== main; parent = parentAcrossShadow(parent)) {
      const overflow = getComputedStyle(parent).overflowX
      if (['auto', 'scroll', 'hidden', 'clip'].includes(overflow)) return true
    }
    return false
  }
  const viewportEscapes = mainElements
    .filter(visible)
    .filter((element) => element.getAttribute('aria-hidden') !== 'true')
    .filter((element) => {
      const rectangle = element.getBoundingClientRect()
      return rectangle.left < -1 || rectangle.right > window.innerWidth + 1
    })
    .filter((element) => !containedHorizontally(element))
    .slice(0, 20)
    .map((element) => {
      const rectangle = element.getBoundingClientRect()
      return {
        element: element.outerHTML.slice(0, 180),
        left: Math.round(rectangle.left),
        right: Math.round(rectangle.right),
      }
    })
  const scrollRegions = [...new Set([main, ...mainElements].filter(Boolean))]
    .filter((element) => {
      const overflow = getComputedStyle(element).overflowY
      return ['auto', 'scroll'].includes(overflow) && element.scrollHeight > element.clientHeight + 2
    })
    .slice(0, 40)
    .map((element) => ({
      name:
        element.getAttribute('aria-label') ||
        element.getAttribute('id') ||
        [...element.classList].slice(0, 2).join('.') ||
        element.tagName.toLowerCase(),
      visibleHeight: Math.round(element.clientHeight),
      contentHeight: Math.round(element.scrollHeight),
    }))

  const errorFallback =
    [...document.querySelectorAll('textarea')].some((textarea) =>
      textarea.labels?.[0]?.textContent?.includes('Diagnostic details')
    ) && document.body.innerText.includes('Try again')
  const html = document.documentElement
  const mainText = `${main?.innerText ?? ''} ${main?.shadowRoot?.textContent ?? ''}`.trim()
  const pageContentElement = document.querySelector('[data-page-content]')
  const pageContentRectangle = pageContentElement?.getBoundingClientRect()
  const guideSurface = document.querySelector('[data-guide-surface="coachmark"]')
  const guideSpotlight = document.querySelector('[data-guide-layer="spotlight"]')
  const guideSurfaceRectangle = guideSurface?.getBoundingClientRect()
  const guideSpotlightRectangle = guideSpotlight?.getBoundingClientRect()
  let guide = null
  if (guideSurface && guideSurfaceRectangle && guideSpotlightRectangle) {
    const overlapWidth = Math.max(
      0,
      Math.min(guideSurfaceRectangle.right, guideSpotlightRectangle.right) -
        Math.max(guideSurfaceRectangle.left, guideSpotlightRectangle.left)
    )
    const overlapHeight = Math.max(
      0,
      Math.min(guideSurfaceRectangle.bottom, guideSpotlightRectangle.bottom) -
        Math.max(guideSurfaceRectangle.top, guideSpotlightRectangle.top)
    )
    const spotlightArea = guideSpotlightRectangle.width * guideSpotlightRectangle.height
    const targetCenter = document.elementFromPoint(
      guideSpotlightRectangle.left + guideSpotlightRectangle.width / 2,
      guideSpotlightRectangle.top + guideSpotlightRectangle.height / 2
    )
    const controlsOutsideViewport = [...guideSurface.querySelectorAll('button, a[href]')]
      .filter(visible)
      .filter((control) => {
        const rectangle = control.getBoundingClientRect()
        return rectangle.top < 0 || rectangle.left < 0 || rectangle.bottom > innerHeight || rectangle.right > innerWidth
      })
      .map((control) => accessibleName(control).slice(0, 80))
    const chromeOverlaps = [...document.querySelectorAll('[data-guide-boundary]')]
      .filter(visible)
      .filter((boundary) => {
        const rectangle = boundary.getBoundingClientRect()
        return !(
          guideSurfaceRectangle.right <= rectangle.left ||
          guideSurfaceRectangle.left >= rectangle.right ||
          guideSurfaceRectangle.bottom <= rectangle.top ||
          guideSurfaceRectangle.top >= rectangle.bottom
        )
      })
      .map((boundary) => boundary.getAttribute('data-guide-boundary'))
    guide = {
      areaRatio: (guideSurfaceRectangle.width * guideSurfaceRectangle.height) / (innerWidth * innerHeight),
      targetVisibleRatio: spotlightArea > 0 ? 1 - (overlapWidth * overlapHeight) / spotlightArea : 0,
      pointerTarget: targetCenter?.closest('[data-guide]')?.getAttribute('data-guide') ?? null,
      textCharacters: guideSurface.innerText.length,
      controlsOutsideViewport,
      chromeOverlaps,
    }
  }

  return window.axe.run(document).then((axeResult) => ({
    title: document.title,
    path: location.pathname,
    h1: headings.filter(({ level }) => level === 1),
    headings,
    headingSkips,
    width: {
      viewport: html.clientWidth,
      document: html.scrollWidth,
      overflow: html.scrollWidth > html.clientWidth + 1,
    },
    main: {
      present: Boolean(main),
      textLength: mainText.length,
      height: Math.round(main?.getBoundingClientRect().height ?? 0),
    },
    pageContent: pageContentRectangle
      ? {
          left: Math.round(pageContentRectangle.left),
          right: Math.round(pageContentRectangle.right),
          width: Math.round(pageContentRectangle.width),
          limit: getComputedStyle(pageContentElement).getPropertyValue('--page-content-width').trim(),
        }
      : null,
    controls: focusable.length,
    unnamedControls,
    crowdedSmallTargets,
    overlaps: overlaps.slice(0, 20),
    sectionGaps,
    layoutRows,
    clippedText,
    viewportEscapes,
    scrollRegions,
    guide,
    errorFallback,
    axe: {
      violations: axeResult.violations.map((violation) => ({
        id: violation.id,
        impact: violation.impact,
        help: violation.help,
        nodeCount: violation.nodes.length,
        nodes: violation.nodes.slice(0, 20).map((node) => ({
          target: node.target,
          html: node.html.slice(0, 500),
          failureSummary: node.failureSummary,
        })),
      })),
      passes: axeResult.passes.length,
      incomplete: axeResult.incomplete.map(({ id }) => id),
    },
  }))
}

function expandScrollableContent() {
  const main = document.querySelector('#main-content')
  if (!main) return { expanded: [], contentHeight: document.documentElement.scrollHeight }

  const roots = [document]
  const allElements = []
  for (let index = 0; index < roots.length; index += 1) {
    for (const element of roots[index].querySelectorAll('*')) {
      allElements.push(element)
      if (element.shadowRoot) roots.push(element.shadowRoot)
    }
  }
  const parentAcrossShadow = (element) => element.parentElement ?? element.getRootNode()?.host ?? null
  const belongsToMain = (element) => {
    for (let current = element; current; current = parentAcrossShadow(current)) {
      if (current === main) return true
    }
    return false
  }
  const elements = [main, ...allElements.filter(belongsToMain)]

  // The paired viewport image preserves real fixed/sticky navigation. Remove
  // that chrome from the expanded image so it cannot cover content halfway
  // down a tall screenshot. Keep in-content controls, but lay them out normally.
  for (const element of allElements) {
    const position = getComputedStyle(element).position
    if (!['fixed', 'sticky'].includes(position)) continue
    if (belongsToMain(element)) {
      element.style.setProperty('position', 'static', 'important')
      element.style.setProperty('inset', 'auto', 'important')
      element.style.setProperty('transform', 'none', 'important')
    } else if (position === 'fixed') {
      element.style.setProperty('visibility', 'hidden', 'important')
    }
  }

  const expanded = new Map()
  for (let pass = 0; pass < 4; pass += 1) {
    for (const element of [...elements].reverse()) {
      if (['TEXTAREA', 'SELECT'].includes(element.tagName) || element.clientHeight < 1) continue
      const overflow = getComputedStyle(element).overflowY
      if (element !== main && !['auto', 'scroll'].includes(overflow)) continue
      if (element.scrollHeight <= element.clientHeight + 2) continue

      const visibleHeight = Math.round(element.clientHeight)
      const contentHeight = Math.ceil(element.scrollHeight)
      element.style.setProperty('height', `${contentHeight}px`, 'important')
      element.style.setProperty('max-height', 'none', 'important')
      element.style.setProperty('overflow-y', 'visible', 'important')
      expanded.set(element, {
        name:
          element.getAttribute('aria-label') ||
          element.getAttribute('id') ||
          [...element.classList].slice(0, 2).join('.') ||
          element.tagName.toLowerCase(),
        visibleHeight,
        contentHeight,
      })

      const scrollArea = element.closest?.('.mantine-ScrollArea-root')
      if (scrollArea && scrollArea !== element) {
        scrollArea.style.setProperty('height', `${contentHeight}px`, 'important')
        scrollArea.style.setProperty('max-height', 'none', 'important')
        scrollArea.style.setProperty('overflow', 'visible', 'important')
      }
    }
  }

  document.documentElement.style.setProperty('scroll-behavior', 'auto', 'important')
  document.body.style.setProperty('height', 'auto', 'important')
  window.scrollTo(0, 0)
  return {
    expanded: [...expanded.values()],
    contentHeight: document.documentElement.scrollHeight,
  }
}

async function evaluate(cdp, expression, awaitPromise = false) {
  const response = await cdp.send('Runtime.evaluate', {
    expression,
    awaitPromise,
    returnByValue: true,
    userGesture: false,
  })
  if (response.exceptionDetails) {
    throw new Error(response.exceptionDetails.exception?.description ?? response.exceptionDetails.text)
  }
  return response.result.value
}

async function waitForRender(cdp) {
  const deadline = Date.now() + 25_000
  while (Date.now() < deadline) {
    const state = await evaluate(
      cdp,
      `({
        ready: document.readyState,
        main: Boolean(document.querySelector('#main-content')),
        h1:
          document.querySelectorAll('#main-content h1').length +
          (document.querySelector('#main-content')?.shadowRoot?.querySelectorAll('h1').length ?? 0),
        loadingOverlays: [
          ...document.querySelectorAll('.mantine-LoadingOverlay-root'),
          ...(document
            .querySelector('#main-content')
            ?.shadowRoot?.querySelectorAll('.mantine-LoadingOverlay-root') ?? []),
        ]
          .filter((element) => {
            const style = getComputedStyle(element)
            const rect = element.getBoundingClientRect()
            return style.visibility !== 'hidden' && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0
          }).length,
        text: (
          (document.querySelector('#main-content')?.innerText ?? '') +
          ' ' +
          (document.querySelector('#main-content')?.shadowRoot?.textContent ?? '') +
          ' ' +
          (document.body?.innerText ?? '')
        ).trim().length
      })`
    )
    if (state.ready === 'complete' && state.main && state.h1 === 1 && state.loadingOverlays === 0 && state.text > 10) {
      await evaluate(cdp, 'document.fonts?.ready ?? Promise.resolve()', true)
      await sleep(900)
      return
    }
    await sleep(200)
  }
  throw new Error('page did not finish rendering one h1 without a loading overlay within 25 seconds')
}

async function auditChallengeCategoryScroller(cdp, route, viewport) {
  if (!viewport.mobile || viewport.width > 390 || !/^\/games\/\d+\/challenges$/.test(route.path)) return null

  const initial = await evaluate(
    cdp,
    `(() => {
      const list = document.querySelector('[data-challenge-category-tabs]')
      if (!list) return null
      const rectangle = list.getBoundingClientRect()
      const tabs = [...list.querySelectorAll('[role="tab"]')]
      return {
        tabCount: tabs.length,
        overflowX: getComputedStyle(list).overflowX,
        bounded:
          rectangle.left >= -1 &&
          rectangle.right <= document.documentElement.clientWidth + 1 &&
          list.clientWidth <= document.documentElement.clientWidth + 1,
        clientWidth: list.clientWidth,
        scrollWidth: list.scrollWidth,
        maximumScroll: Math.max(0, list.scrollWidth - list.clientWidth),
        rectangle: {
          left: rectangle.left,
          right: rectangle.right,
          top: rectangle.top,
          bottom: rectangle.bottom,
        },
      }
    })()`
  )
  if (!initial) return null

  const overflowRequired = initial.maximumScroll > 1
  if (!overflowRequired || initial.tabCount < 2) {
    return {
      ...initial,
      overflowRequired,
      touchReachedLast: true,
      keyboardReachedLast: true,
    }
  }

  await evaluate(cdp, `document.querySelector('[data-challenge-category-tabs]').scrollLeft = 0`)
  const touchWidth = Math.max(1, initial.rectangle.right - initial.rectangle.left - 24)
  const touchAttempts = Math.min(12, Math.ceil(initial.maximumScroll / touchWidth) + 2)
  const touchY = initial.rectangle.top + (initial.rectangle.bottom - initial.rectangle.top) / 2
  const touchStartX = initial.rectangle.right - 12
  const touchEndX = initial.rectangle.left + 12
  for (let attempt = 0; attempt < touchAttempts; attempt += 1) {
    await cdp.send('Input.dispatchTouchEvent', {
      type: 'touchStart',
      touchPoints: [{ x: touchStartX, y: touchY }],
    })
    for (let step = 1; step <= 6; step += 1) {
      await cdp.send('Input.dispatchTouchEvent', {
        type: 'touchMove',
        touchPoints: [
          {
            x: touchStartX + ((touchEndX - touchStartX) * step) / 6,
            y: touchY,
          },
        ],
      })
      await sleep(12)
    }
    await cdp.send('Input.dispatchTouchEvent', {
      type: 'touchEnd',
      touchPoints: [],
    })
    await sleep(40)
    const atEnd = await evaluate(
      cdp,
      `(() => {
        const list = document.querySelector('[data-challenge-category-tabs]')
        return list.scrollLeft >= list.scrollWidth - list.clientWidth - 2
      })()`
    )
    if (atEnd) break
  }
  const touch = await evaluate(
    cdp,
    `(() => {
      const list = document.querySelector('[data-challenge-category-tabs]')
      const last = [...list.querySelectorAll('[role="tab"]')].at(-1)
      const listRectangle = list.getBoundingClientRect()
      const lastRectangle = last?.getBoundingClientRect()
      return {
        scrollLeft: list.scrollLeft,
        reachedLast:
          Boolean(lastRectangle) &&
          lastRectangle.left >= listRectangle.left - 1 &&
          lastRectangle.right <= listRectangle.right + 1,
      }
    })()`
  )

  await evaluate(
    cdp,
    `(() => {
      const list = document.querySelector('[data-challenge-category-tabs]')
      const first = list.querySelector('[role="tab"]')
      list.scrollLeft = 0
      first.focus({ preventScroll: true })
    })()`
  )
  for (let index = 1; index < initial.tabCount; index += 1) {
    await cdp.send('Input.dispatchKeyEvent', {
      type: 'rawKeyDown',
      key: 'ArrowRight',
      code: 'ArrowRight',
      windowsVirtualKeyCode: 39,
      nativeVirtualKeyCode: 39,
    })
    await cdp.send('Input.dispatchKeyEvent', {
      type: 'keyUp',
      key: 'ArrowRight',
      code: 'ArrowRight',
      windowsVirtualKeyCode: 39,
      nativeVirtualKeyCode: 39,
    })
    await sleep(20)
  }
  const keyboard = await evaluate(
    cdp,
    `(() => {
      const list = document.querySelector('[data-challenge-category-tabs]')
      const last = [...list.querySelectorAll('[role="tab"]')].at(-1)
      const listRectangle = list.getBoundingClientRect()
      const lastRectangle = last?.getBoundingClientRect()
      return {
        scrollLeft: list.scrollLeft,
        reachedLast:
          document.activeElement === last &&
          Boolean(lastRectangle) &&
          lastRectangle.left >= listRectangle.left - 1 &&
          lastRectangle.right <= listRectangle.right + 1,
      }
    })()`
  )

  return {
    ...initial,
    overflowRequired,
    touchScrollLeft: touch.scrollLeft,
    touchReachedLast: touch.reachedLast,
    keyboardScrollLeft: keyboard.scrollLeft,
    keyboardReachedLast: keyboard.reachedLast,
  }
}

function failuresFor(result, expectedPath) {
  const failures = []
  if (result.path !== expectedPath) failures.push(`redirected to ${result.path}`)
  if (!result.main.present || result.main.textLength < 10) failures.push('main content is empty')
  if (result.h1.length !== 1) failures.push(`expected one h1, found ${result.h1.length}`)
  if (result.headingSkips.length) failures.push(`${result.headingSkips.length} heading-level skips`)
  if (result.width.overflow) {
    failures.push(`page overflow ${result.width.document}px/${result.width.viewport}px`)
  }
  if (result.viewportEscapes.length) {
    failures.push(`${result.viewportEscapes.length} elements escape the viewport`)
  }
  if (result.challengeCategoryTabs) {
    const tabs = result.challengeCategoryTabs
    if (!tabs.bounded) failures.push('challenge category tabs escape the compact viewport')
    if (tabs.overflowRequired && !['auto', 'scroll'].includes(tabs.overflowX)) {
      failures.push(`challenge category tabs use overflow-x ${tabs.overflowX} instead of a scroller`)
    }
    if (tabs.overflowRequired && !tabs.touchReachedLast) {
      failures.push('final challenge category is not reachable with a touch swipe')
    }
    if (tabs.overflowRequired && !tabs.keyboardReachedLast) {
      failures.push('final challenge category is not reachable with the keyboard')
    }
  }
  if (result.unnamedControls.length) failures.push(`${result.unnamedControls.length} unnamed controls`)
  if (result.crowdedSmallTargets.length) {
    failures.push(`${result.crowdedSmallTargets.length} crowded controls below 24px`)
  }
  for (const gap of result.sectionGaps ?? []) {
    if (gap.actual + 0.5 < gap.required) {
      failures.push(`${gap.section} section gap is ${gap.actual}px; expected at least ${gap.required}px`)
    }
  }
  for (const rows of result.layoutRows ?? []) {
    if (!Number.isFinite(rows.maximum) || rows.maximum < 1 || rows.actual > rows.maximum) {
      failures.push(`${rows.section} uses ${rows.actual} rows; expected at most ${rows.maximum}`)
    }
    if (rows.overflowingChildren) {
      failures.push(`${rows.section} has ${rows.overflowingChildren} overflowing children`)
    }
  }
  if (result.axe.violations.length) failures.push(`${result.axe.violations.length} axe violations`)
  if (result.guide) {
    const guideAreaBudget = result.width.viewport <= 320 ? 0.45 : result.width.viewport <= 768 ? 0.34 : 0.25
    if (result.guide.areaRatio > guideAreaBudget) {
      failures.push(
        `guide covers ${(result.guide.areaRatio * 100).toFixed(1)}% of the viewport; budget is ${(guideAreaBudget * 100).toFixed(0)}%`
      )
    }
    if (result.guide.targetVisibleRatio < 0.9) {
      failures.push(`guide obscures ${((1 - result.guide.targetVisibleRatio) * 100).toFixed(1)}% of its target`)
    }
    if (!result.guide.pointerTarget) failures.push('guide target is not pointer-accessible at its center')
    if (result.guide.controlsOutsideViewport.length) {
      failures.push(`${result.guide.controlsOutsideViewport.length} guide controls are outside the viewport`)
    }
    if (result.guide.chromeOverlaps.length) {
      failures.push(`guide overlaps persistent UI: ${result.guide.chromeOverlaps.join(', ')}`)
    }
    if (result.guide.textCharacters > 280) {
      failures.push(`guide coach-mark contains ${result.guide.textCharacters} characters; budget is 280`)
    }
  }
  if (result.server5xx.length) failures.push(`${result.server5xx.length} HTTP 5xx responses`)
  if (result.runtimeExceptions.length) failures.push(`${result.runtimeExceptions.length} runtime exceptions`)
  if (result.consoleErrors.length) failures.push(`${result.consoleErrors.length} console errors`)
  if (result.errorFallback) failures.push('React error fallback rendered')
  return failures
}

function filterConsoleErrors(messages, route) {
  return [...new Set(messages)].filter((message) => {
    if (message.includes('favicon') || message.includes('net::ERR_ABORTED')) return false
    if (message.includes('connection was stopped during negotiation')) return false
    if (route.auth === 'anonymous' && /status of 401\b/.test(message)) return false
    return true
  })
}

function applyLayoutConsistencyChecks(results) {
  const groups = new Map()
  for (const result of results) {
    if (!result.route.layoutGroup || !result.pageContent) continue
    const key = `${result.viewport}:${result.route.layoutGroup}`
    const group = groups.get(key) ?? []
    group.push(result)
    groups.set(key, group)
  }

  for (const group of groups.values()) {
    if (group.length < 2) continue
    const limits = new Set(group.map((result) => result.pageContent.limit))
    if (limits.size === 1) continue
    const details = group.map((result) => `${result.route.path}=${result.pageContent.limit}`).join(', ')
    for (const result of group) {
      result.failures.push(`inconsistent ${result.route.layoutGroup} content limit: ${details}`)
    }
  }
}

function markdownReport(report) {
  const lines = [
    '# RSCTF full-page visual audit',
    '',
    `- Target: \`${report.target}\``,
    `- Generated: ${report.generatedAt}`,
    `- Chromium: \`${report.chromium}\``,
    `- Pages: ${report.routeCount}`,
    `- Renders: ${report.results.length}`,
    `- Failed renders: ${report.summary.failed}`,
    `- Automated warnings: ${report.summary.warnings}`,
    '',
    '| Viewport | Page | Result | Content width | Axe | Overflow | Scroll regions | Browser | Warnings |',
    '| --- | --- | --- | ---: | ---: | --- | ---: | ---: | ---: |',
  ]
  for (const result of report.results) {
    lines.push(
      `| ${result.viewport} | \`${result.route.sourceFile}\` | ${
        result.failures.length ? `FAIL: ${result.failures.join('; ')}` : 'PASS'
      } | ${
        result.pageContent ? `${result.pageContent.width}px / ${result.pageContent.limit}` : '-'
      } | ${result.axe.violations.length} | ${
        result.width.overflow ? 'yes' : 'no'
      } | ${result.scrollRegions.length} | ${
        result.server5xx.length + result.runtimeExceptions.length + result.consoleErrors.length
      } | ${result.clippedText.length} |`
    )
  }
  lines.push(
    '',
    '## Manual screenshot review',
    '',
    'Open `gallery.html` and inspect hierarchy, density, alignment, whitespace, truncation, empty states, and mobile reachability. Each route has its real viewport beside a full-content capture with nested vertical scroll regions expanded. Automated warnings identify clipped text that needs human judgment.',
    ''
  )
  return `${lines.join('\n')}\n`
}

function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;')
}

function gallery(report) {
  const cards = report.results
    .map((result) => {
      const status = result.failures.length ? 'fail' : result.clippedText.length ? 'warn' : 'pass'
      const findings = [...result.failures, ...result.clippedText.map((text) => `Clipped: ${text}`)]
      return `<article class="card ${status}">
        <header>
          <div><strong>${escapeHtml(result.route.sourceFile)}</strong><small>${escapeHtml(result.route.path)}</small></div>
          <span>${escapeHtml(result.viewport)} ${result.viewportSize.width}×${result.viewportSize.height} · content ${
            result.pageContent ? `${result.pageContent.width}px / ${result.pageContent.limit}` : '-'
          } · ${status.toUpperCase()}</span>
        </header>
        <div class="screenshots">
          <figure>
            <figcaption>Actual viewport</figcaption>
            <a href="${encodeURI(result.viewportScreenshot)}"><img src="${encodeURI(
              result.viewportScreenshot
            )}" alt="${escapeHtml(`${result.viewport} viewport screenshot of ${result.route.path}`)}" loading="lazy"></a>
          </figure>
          <figure>
            <figcaption>Expanded full content · ${result.expandedScrollRegions.length} scroll region(s)</figcaption>
            <a href="${encodeURI(result.screenshot)}"><img src="${encodeURI(result.screenshot)}" alt="${escapeHtml(
              `${result.viewport} full-content screenshot of ${result.route.path}`
            )}" loading="lazy"></a>
          </figure>
        </div>
        <ul>${findings.length ? findings.map((item) => `<li>${escapeHtml(item)}</li>`).join('') : '<li>Clean automated audit</li>'}</ul>
      </article>`
    })
    .join('\n')
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>RSCTF visual audit gallery</title>
  <style>
    :root { color-scheme: dark; font: 15px/1.45 system-ui, sans-serif; background: #080d18; color: #edf2ff; }
    body { margin: 0; padding: 24px; }
    h1 { margin: 0 0 4px; }
    .summary { margin: 0 0 24px; color: #aebbd2; }
    main { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 640px), 1fr)); gap: 18px; }
    .card { min-width: 0; overflow: hidden; border: 1px solid #29364c; border-radius: 14px; background: #101827; }
    .card.fail { border-color: #f04444; }
    .card.warn { border-color: #d99a20; }
    header { display: flex; justify-content: space-between; gap: 12px; padding: 14px; }
    header div { min-width: 0; }
    header strong, header small { display: block; overflow-wrap: anywhere; }
    header small, .summary { color: #aebbd2; }
    header span { flex: none; font-size: 12px; font-weight: 700; color: #d8e0ef; }
    .screenshots { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); background: #050914; }
    figure { min-width: 0; margin: 0; border-top: 1px solid #29364c; }
    figure + figure { border-left: 1px solid #29364c; }
    figcaption { padding: 7px 10px; color: #aebbd2; font-size: 12px; }
    a { display: block; background: #050914; }
    img { display: block; width: 100%; height: 480px; object-fit: contain; object-position: top; }
    ul { min-height: 22px; margin: 0; padding: 12px 30px 16px; color: #cdd7e8; }
    @media (max-width: 680px) {
      .screenshots { grid-template-columns: 1fr; }
      figure + figure { border-left: 0; }
    }
  </style>
</head>
<body>
  <h1>RSCTF full-page visual audit</h1>
  <p class="summary">${report.results.length} renders · ${report.summary.failed} failed · ${report.summary.warnings} warnings · ${escapeHtml(
    report.generatedAt
  )}</p>
  <main>${cards}</main>
</body>
</html>`
}

async function main() {
  const options = parseArguments(process.argv.slice(2))
  const listContext = {
    gameId: '{gameId}',
    challengeId: '{challengeId}',
    postId: '{postId}',
  }
  const context = options.list
    ? listContext
    : {
        gameId: numericContext('RSCTF_VISUAL_GAME_ID', '{gameId}', false),
        challengeId: numericContext('RSCTF_VISUAL_CHALLENGE_ID', '{challengeId}', false),
        postId: process.env.RSCTF_VISUAL_POST_ID?.trim(),
      }
  let routes = discoverPageRoutes(context)
  if (options.pageFilters.length) {
    routes = routes.filter((route) =>
      options.pageFilters.some((filter) => {
        if (!filter.startsWith('=')) return route.name.includes(filter) || route.path.includes(filter)
        const exact = filter.slice(1)
        return route.name === exact || route.path === exact
      })
    )
  }
  routes = selectRouteShard(routes, options.shard)
  if (!routes.length) throw new Error('no visual routes matched')

  if (options.list) {
    for (const route of routes) {
      console.log(`${route.auth.padEnd(9)} ${route.path.padEnd(64)} ${route.sourceFile}`)
    }
    return
  }

  const routeProblems = validatePageRoutes(routes)
  if (routeProblems.length) throw new Error(routeProblems.join('\n'))
  const adminToken = secret('RSCTF_VISUAL_ADMIN_JWT')
  const playerToken = secret('RSCTF_VISUAL_PLAYER_JWT')
  if (routes.some((route) => route.auth === 'admin') && !adminToken) {
    throw new Error('RSCTF_VISUAL_ADMIN_JWT or RSCTF_VISUAL_ADMIN_JWT_FILE is required')
  }
  if (routes.some((route) => route.auth === 'player') && !playerToken) {
    throw new Error('RSCTF_VISUAL_PLAYER_JWT or RSCTF_VISUAL_PLAYER_JWT_FILE is required')
  }

  const target = safeOrigin(options.target)
  const axePath = join(repositoryRoot, 'web', 'node_modules', 'axe-core', 'axe.min.js')
  if (!existsSync(axePath)) throw new Error('axe-core is missing; run pnpm --dir web install')
  const axeSource = readFileSync(axePath, 'utf8')
  const outputRoot = join(repositoryRoot, 'visual-audit-output')
  const output = resolve(repositoryRoot, options.output)
  if (output !== outputRoot && !output.startsWith(`${outputRoot}${sep}`)) {
    throw new Error('visual output must stay within the repository visual-audit-output directory')
  }
  rmSync(output, { recursive: true, force: true })
  mkdirSync(output, { recursive: true })

  const { cdp, close, executable } = await launchBrowser()
  let terminating = false
  const stopForSignal = (exitCode) => {
    if (terminating) return
    terminating = true
    void close().finally(() => process.exit(exitCode))
  }
  const onInterrupt = () => stopForSignal(130)
  const onTerminate = () => stopForSignal(143)
  const onHangup = () => stopForSignal(129)
  process.once('SIGINT', onInterrupt)
  process.once('SIGTERM', onTerminate)
  process.once('SIGHUP', onHangup)
  await Promise.all([
    cdp.send('Page.enable'),
    cdp.send('Runtime.enable'),
    cdp.send('Network.enable'),
    cdp.send('Log.enable'),
  ])
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: axeSource,
  })
  if (process.env.RSCTF_VISUAL_DISABLE_GUIDE === '1') {
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `localStorage.setItem('rsctf-player-guide:guest', JSON.stringify({ interactiveEnabled: false, completedVersion: 1, seenFeatures: [] }))`,
    })
  }

  let server5xx = []
  let runtimeExceptions = []
  let consoleErrors = []
  cdp.on('Network.responseReceived', ({ response }) => {
    if (response.status >= 500) server5xx.push({ status: response.status, url: response.url })
  })
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => {
    runtimeExceptions.push(exceptionDetails.exception?.description ?? exceptionDetails.text)
  })
  cdp.on('Runtime.consoleAPICalled', ({ type, args }) => {
    if (type !== 'error') return
    consoleErrors.push(args.map((argument) => argument.value ?? argument.description ?? '').join(' '))
  })
  cdp.on('Log.entryAdded', ({ entry }) => {
    if (entry.level === 'error') consoleErrors.push(`${entry.source}: ${entry.text}`)
  })

  const results = []
  try {
    for (const viewportName of options.viewports) {
      const viewport = viewportCatalog[viewportName]
      await cdp.send('Emulation.setDeviceMetricsOverride', {
        width: viewport.width,
        height: viewport.height,
        deviceScaleFactor: 1,
        mobile: viewport.mobile,
        screenOrientation: { type: 'portraitPrimary', angle: 0 },
      })
      await cdp.send('Emulation.setTouchEmulationEnabled', {
        enabled: viewport.mobile,
        maxTouchPoints: viewport.mobile ? 5 : 1,
      })

      for (const [index, route] of routes.entries()) {
        server5xx = []
        runtimeExceptions = []
        consoleErrors = []
        await cdp.send('Network.clearBrowserCookies')
        await cdp.send('Storage.clearDataForOrigin', {
          origin: target,
          storageTypes: 'all',
        })
        const token = route.auth === 'admin' ? adminToken : route.auth === 'player' ? playerToken : ''
        if (token) {
          const cookie = await cdp.send('Network.setCookie', {
            name: 'RSCTF_Token',
            value: token,
            url: target,
            secure: target.startsWith('https://'),
            httpOnly: true,
            sameSite: 'Lax',
            expires: Math.floor(Date.now() / 1000) + 3600,
          })
          if (!cookie.success) throw new Error(`Chromium rejected the ${route.auth} cookie`)
        }

        process.stdout.write(
          `[${viewportName} ${String(index + 1).padStart(2, '0')}/${routes.length}] ${route.path} ... `
        )
        let result
        try {
          await cdp.send('Page.navigate', { url: `${target}${route.urlPath}` })
          await waitForRender(cdp)
          await evaluate(cdp, 'window.scrollTo(0, 0)')
          const challengeCategoryTabs = await auditChallengeCategoryScroller(cdp, route, viewport)
          result = await evaluate(cdp, `(${accessibleDocumentAnalysis.toString()})()`, true)
          result.challengeCategoryTabs = challengeCategoryTabs
        } catch (error) {
          result = {
            title: '',
            path: '',
            h1: [],
            headings: [],
            headingSkips: [],
            width: { viewport: viewport.width, document: 0, overflow: false },
            main: { present: false, textLength: 0, height: 0 },
            pageContent: null,
            controls: 0,
            unnamedControls: [],
            crowdedSmallTargets: [],
            overlaps: [],
            sectionGaps: [],
            layoutRows: [],
            clippedText: [],
            viewportEscapes: [],
            scrollRegions: [],
            challengeCategoryTabs: null,
            guide: null,
            errorFallback: false,
            axe: { violations: [], passes: 0, incomplete: [] },
            auditError: error instanceof Error ? error.message : String(error),
          }
        }
        result.server5xx = [...server5xx]
        result.runtimeExceptions = [...new Set(runtimeExceptions)]
        result.consoleErrors = filterConsoleErrors(consoleErrors, route)
        result.failures = result.auditError
          ? [`audit error: ${result.auditError}`]
          : failuresFor(result, route.expectedPath)

        const viewportScreenshotName = `${viewportName}--${route.name}--viewport.png`
        const viewportScreenshot = await cdp.send('Page.captureScreenshot', {
          format: 'png',
          fromSurface: true,
          captureBeyondViewport: false,
        })
        writeFileSync(join(output, viewportScreenshotName), Buffer.from(viewportScreenshot.data, 'base64'))

        const expansion = await evaluate(cdp, `(${expandScrollableContent.toString()})()`)
        await sleep(50)
        const metrics = await cdp.send('Page.getLayoutMetrics')
        const content = metrics.cssContentSize
        const screenshotName = `${viewportName}--${route.name}--full.png`
        const screenshotHeight = Math.min(MAX_SCREENSHOT_HEIGHT, Math.max(viewport.height, Math.ceil(content.height)))
        const screenshotWidth = Math.min(MAX_SCREENSHOT_WIDTH, Math.max(viewport.width, Math.ceil(content.width)))
        const screenshot = await cdp.send('Page.captureScreenshot', {
          format: 'png',
          fromSurface: true,
          captureBeyondViewport: true,
          clip: {
            x: 0,
            y: 0,
            width: screenshotWidth,
            height: screenshotHeight,
            scale: 1,
          },
        })
        writeFileSync(join(output, screenshotName), Buffer.from(screenshot.data, 'base64'))
        result.screenshot = screenshotName
        result.viewportScreenshot = viewportScreenshotName
        result.expandedScrollRegions = expansion.expanded
        result.screenshotTruncated = content.height > MAX_SCREENSHOT_HEIGHT
        if (result.screenshotTruncated) {
          result.failures.push(
            `screenshot truncated at ${MAX_SCREENSHOT_HEIGHT}px (page is ${Math.ceil(content.height)}px tall)`
          )
        }
        result.route = route
        result.viewport = viewportName
        result.viewportSize = viewport
        results.push(result)
        console.log(
          result.failures.length
            ? `FAIL ${result.failures.join('; ')}`
            : result.clippedText.length
              ? `PASS (${result.clippedText.length} visual warning(s))`
              : 'PASS'
        )
      }
    }
  } finally {
    process.off('SIGINT', onInterrupt)
    process.off('SIGTERM', onTerminate)
    process.off('SIGHUP', onHangup)
    await cdp.send('Network.clearBrowserCookies').catch(() => {})
    await cdp
      .send('Storage.clearDataForOrigin', {
        origin: target,
        storageTypes: 'all',
      })
      .catch(() => {})
    await close()
  }

  applyLayoutConsistencyChecks(results)
  const report = {
    generatedAt: new Date().toISOString(),
    target,
    chromium: executable,
    routeCount: routes.length,
    shard: options.shard,
    viewports: options.viewports,
    summary: {
      failed: results.filter((result) => result.failures.length).length,
      warnings: results.reduce((total, result) => total + result.clippedText.length, 0),
    },
    results,
  }
  writeFileSync(join(output, 'report.json'), `${JSON.stringify(report, null, 2)}\n`)
  writeFileSync(join(output, 'report.md'), markdownReport(report))
  writeFileSync(join(output, 'gallery.html'), gallery(report))
  console.log(
    `Visual audit: ${results.length} renders, ${report.summary.failed} failed, ${report.summary.warnings} warning(s)`
  )
  console.log(`Artifacts: ${relative(repositoryRoot, output)}`)
  if (report.summary.failed) process.exitCode = 1
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main()

export { auditChallengeCategoryScroller }
