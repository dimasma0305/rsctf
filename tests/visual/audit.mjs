#!/usr/bin/env node

import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, relative, resolve, sep } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { discoverPageRoutes, repositoryRoot, validatePageRoutes } from './route-catalog.mjs'

const sleep = (milliseconds) => new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds))

function parseArguments(argv) {
  const options = {
    target: process.env.RSCTF_VISUAL_TARGET || 'http://127.0.0.1:8080',
    output: process.env.RSCTF_VISUAL_OUTPUT || join(repositoryRoot, 'visual-audit-output'),
    pageFilters: [],
    viewports: ['desktop', 'mobile'],
    list: false,
  }
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === '--') continue
    if (argument === '--target') options.target = argv[++index]
    else if (argument === '--output') options.output = argv[++index]
    else if (argument === '--page') options.pageFilters.push(argv[++index])
    else if (argument === '--desktop-only') options.viewports = ['desktop']
    else if (argument === '--mobile-only') options.viewports = ['mobile']
    else if (argument === '--list') options.list = true
    else if (argument === '--help' || argument === '-h') {
      console.log(`Usage: node tests/visual/audit.mjs [options]

Options:
  --target URL       RSCTF origin (default http://127.0.0.1:8080)
  --output PATH      Generated artifact directory (relative to the repository root)
  --page FILTER      Audit route name/path containing FILTER; repeatable
  --desktop-only     Capture only 1440x1100
  --mobile-only      Capture only 390x844
  --list             List resolved routes without launching Chromium
`)
      process.exit(0)
    } else {
      throw new Error(`unknown argument: ${argument}`)
    }
  }
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
    .filter((element) => element.children.length === 0 && (element.textContent?.trim().length ?? 0) > 8)
    .filter((element) => {
      const style = getComputedStyle(element)
      const clips = ['hidden', 'clip'].includes(style.overflowX) || ['hidden', 'clip'].includes(style.overflowY)
      return clips && (element.scrollWidth > element.clientWidth + 2 || element.scrollHeight > element.clientHeight + 2)
    })
    .filter((element) => !element.getAttribute('title') && !element.getAttribute('aria-label'))
    .slice(0, 20)
    .map((element) => element.textContent.trim().slice(0, 140))

  const errorFallback =
    [...document.querySelectorAll('textarea')].some((textarea) =>
      textarea.labels?.[0]?.textContent?.includes('Diagnostic details')
    ) && document.body.innerText.includes('Try again')
  const html = document.documentElement
  const main = document.querySelector('#main-content')
  const mainText = `${main?.innerText ?? ''} ${main?.shadowRoot?.textContent ?? ''}`.trim()

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
    controls: focusable.length,
    unnamedControls,
    crowdedSmallTargets,
    overlaps: overlaps.slice(0, 20),
    clippedText,
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
        text: (
          (document.querySelector('#main-content')?.innerText ?? '') +
          ' ' +
          (document.querySelector('#main-content')?.shadowRoot?.textContent ?? '') +
          ' ' +
          (document.body?.innerText ?? '')
        ).trim().length
      })`
    )
    if (state.ready === 'complete' && state.main && state.text > 10) {
      await evaluate(cdp, 'document.fonts?.ready ?? Promise.resolve()', true)
      await sleep(900)
      return
    }
    await sleep(200)
  }
  throw new Error('page did not render #main-content within 25 seconds')
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
  if (result.unnamedControls.length) failures.push(`${result.unnamedControls.length} unnamed controls`)
  if (result.crowdedSmallTargets.length) {
    failures.push(`${result.crowdedSmallTargets.length} crowded controls below 24px`)
  }
  if (result.axe.violations.length) failures.push(`${result.axe.violations.length} axe violations`)
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
    '| Viewport | Page | Result | Axe | Overflow | Browser | Warnings |',
    '| --- | --- | --- | ---: | --- | ---: | ---: |',
  ]
  for (const result of report.results) {
    lines.push(
      `| ${result.viewport} | \`${result.route.sourceFile}\` | ${
        result.failures.length ? `FAIL: ${result.failures.join('; ')}` : 'PASS'
      } | ${result.axe.violations.length} | ${result.width.overflow ? 'yes' : 'no'} | ${
        result.server5xx.length + result.runtimeExceptions.length + result.consoleErrors.length
      } | ${result.clippedText.length} |`
    )
  }
  lines.push(
    '',
    '## Manual screenshot review',
    '',
    'Open `gallery.html` and inspect hierarchy, density, alignment, whitespace, truncation, empty states, and mobile reachability. Automated warnings identify clipped text that needs human judgment.',
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
          <span>${escapeHtml(result.viewport)} · ${status.toUpperCase()}</span>
        </header>
        <a href="${encodeURI(result.screenshot)}"><img src="${encodeURI(result.screenshot)}" alt="${escapeHtml(
          `${result.viewport} screenshot of ${result.route.path}`
        )}" loading="lazy"></a>
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
    main { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 18px; }
    .card { min-width: 0; overflow: hidden; border: 1px solid #29364c; border-radius: 14px; background: #101827; }
    .card.fail { border-color: #f04444; }
    .card.warn { border-color: #d99a20; }
    header { display: flex; justify-content: space-between; gap: 12px; padding: 14px; }
    header div { min-width: 0; }
    header strong, header small { display: block; overflow-wrap: anywhere; }
    header small, .summary { color: #aebbd2; }
    header span { flex: none; font-size: 12px; font-weight: 700; color: #d8e0ef; }
    a { display: block; background: #050914; }
    img { display: block; width: 100%; height: 480px; object-fit: contain; object-position: top; }
    ul { min-height: 22px; margin: 0; padding: 12px 30px 16px; color: #cdd7e8; }
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
      options.pageFilters.some((filter) => route.name.includes(filter) || route.path.includes(filter))
    )
  }
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

  const viewports = {
    desktop: { width: 1440, height: 1100, mobile: false },
    mobile: { width: 390, height: 844, mobile: true },
  }
  const { cdp, close, executable } = await launchBrowser()
  await Promise.all([
    cdp.send('Page.enable'),
    cdp.send('Runtime.enable'),
    cdp.send('Network.enable'),
    cdp.send('Log.enable'),
  ])
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: axeSource,
  })

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
      const viewport = viewports[viewportName]
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
          result = await evaluate(cdp, `(${accessibleDocumentAnalysis.toString()})()`, true)
        } catch (error) {
          result = {
            title: '',
            path: '',
            h1: [],
            headings: [],
            headingSkips: [],
            width: { viewport: viewport.width, document: 0, overflow: false },
            main: { present: false, textLength: 0, height: 0 },
            controls: 0,
            unnamedControls: [],
            crowdedSmallTargets: [],
            overlaps: [],
            clippedText: [],
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

        const metrics = await cdp.send('Page.getLayoutMetrics')
        const content = metrics.cssContentSize
        const screenshotName = `${viewportName}--${route.name}.png`
        const screenshotHeight = Math.min(16_000, Math.max(viewport.height, Math.ceil(content.height)))
        const screenshotWidth = Math.min(2_400, Math.max(viewport.width, Math.ceil(content.width)))
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
        result.screenshotTruncated = content.height > 16_000
        if (result.screenshotTruncated) {
          result.failures.push(`screenshot truncated at 16000px (page is ${Math.ceil(content.height)}px tall)`)
        }
        result.route = route
        result.viewport = viewportName
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
    await cdp.send('Network.clearBrowserCookies').catch(() => {})
    await cdp
      .send('Storage.clearDataForOrigin', {
        origin: target,
        storageTypes: 'all',
      })
      .catch(() => {})
    await close()
  }

  const report = {
    generatedAt: new Date().toISOString(),
    target,
    chromium: executable,
    routeCount: routes.length,
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

await main()
