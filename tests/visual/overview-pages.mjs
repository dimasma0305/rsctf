// Screenshots and UI interactions against read-only fixtures, including live-origin assets.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { fixture, guideSetup } from './overview-pages-fixtures.mjs'

const target = process.env.RSCTF_OVERVIEW_TARGET || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://tcp.1pc.tf', 'https://intechfest.1pc.tf'].includes(target))
const output = resolve(process.env.RSCTF_OVERVIEW_OUTPUT || '../visual-audit-output/overview-local')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const reports = [], errors = [], mutations = [], requests = []
let scenario = 'normal', role = 'Admin'
const evaluate = async expression => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async expression => {
  for (let i = 0; i < 100; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error('Timed out: ' + expression)
}
const press = async key => {
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', {
    type, key, windowsVirtualKeyCode: { Enter: 13, Escape: 27, Tab: 9, ArrowRight: 39 }[key],
    ...(key === 'Enter' && type === 'keyDown' ? { text: '\r' } : {}),
  })
}
const inspect = async name => {
  await evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))`)
  await evaluate(`Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))`)
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const result = await evaluate(`(async () => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1, headings: document.querySelectorAll('h1').length, violations: (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'best-practice'] } })).violations.map(v => ({ id: v.id, nodes: v.nodes.map(n => ({ target: n.target, summary: n.failureSummary })) })) }))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  reports.push({ name, ...result })
  console.log(name, JSON.stringify(result))
}
const visit = async (path, ready) => {
  await cdp.send('Storage.clearDataForOrigin', { origin: target, storageTypes: 'all' })
  await cdp.send('Page.navigate', { url: target + path })
  await wait(ready)
}

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: guideSetup })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const url = new URL(request.url)
    requests.push(url.pathname + url.search)
    if (!['GET', 'HEAD'].includes(request.method)) mutations.push({ path: url.pathname, method: request.method })
    const result = fixture(url.pathname + url.search, request.method, scenario, role)
    if (result.hold) return
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: result.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(result.body)).toString('base64') })
  })
  for (const [name, width, height, language, scheme] of [
    ['desktop', 1440, 1100, 'en-US', 'dark'],
    ['wide', 1920, 1080, 'en-US', 'dark'],
    ['tablet', 768, 1024, 'en-US', 'dark'],
    ['mobile', 390, 844, 'en-US', 'dark'],
    ['compact', 320, 568, 'en-US', 'dark'],
    ['light-id', 390, 844, 'id-ID', 'light'],
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: name === 'light-id' ? 'reduce' : 'no-preference' }] })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('language', JSON.stringify('${language}')); localStorage.setItem('mantine-color-scheme-value', '${scheme}');` })
    await visit('/', `document.querySelectorAll('[data-post-card]').length === 2`)
    await inspect(name + '-home')
    assert.equal(await evaluate(`document.querySelectorAll('[data-home-overview] [data-post-card][data-layout="feed"] h3').length`), 2)
    assert.equal(await evaluate(`document.querySelectorAll('[data-workspace-links] a').length`), 3)
    if (width >= 1440) assert.ok(await evaluate(`document.querySelector('#news-feed-title').getBoundingClientRect().top < 220`), 'Home must lead with real content')
    await visit('/games', `document.querySelectorAll('#event-catalog-results [data-guide="event-card"]').length === 3`)
    await inspect(name + '-games')
    assert.equal(await evaluate(`document.querySelectorAll('[data-event-catalog] h2').length`), 3)
    assert.ok(await evaluate(`Array.from(document.querySelectorAll('#event-catalog-results [data-status], #event-catalog-results [data-membership]')).every(label => getComputedStyle(label).position !== 'absolute' && label.scrollWidth <= label.clientWidth + 1 && label.scrollHeight <= label.clientHeight + 1)`), 'Event status and membership labels must wrap without clipping in the narrow poster column')
    if (width >= 1440) assert.ok(await evaluate(`document.querySelector('#event-catalog-results [data-guide="event-card"]').getBoundingClientRect().top < 410`), 'Event cards must not be pushed below decorative chrome')
    await visit('/admin/dashboard', `document.querySelector('[data-dashboard-stats]') && !document.querySelector('.mantine-LoadingOverlay-root')`)
    await inspect(name + '-dashboard')
    assert.ok(await evaluate(`(() => { const rows = [...document.querySelectorAll('[data-dashboard-stats] > a')]; return rows.length === 3 && rows.every(row => Math.abs(row.getBoundingClientRect().top - rows[0].getBoundingClientRect().top) < 1 && row.getBoundingClientRect().height >= 44); })()`), 'Dashboard totals stay in one keyboard-accessible row at 320px')
    if (width >= 1440) assert.ok(await evaluate(`document.querySelector('[data-dashboard-stats]').getBoundingClientRect().top < 300`), 'Dashboard totals must precede decorative panels')
    if (width === 320) assert.ok(await evaluate(`document.querySelector('[data-dashboard-stats]').getBoundingClientRect().bottom < innerHeight - 80`), 'Dashboard totals must be visible above the mobile dock')
  }
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('language', JSON.stringify('en-US')); localStorage.setItem('mantine-color-scheme-value', 'dark');` })
  await visit('/', `document.querySelector('[data-workspace-links] a[href="/games"]')`)
  await evaluate(`document.querySelector('[data-workspace-links] a[href="/games"]').focus()`)
  await press('Tab')
  assert.equal(await evaluate(`document.activeElement.getAttribute('href')`), '/teams')
  assert.ok(await evaluate(`document.activeElement.matches(':focus-visible') && parseFloat(getComputedStyle(document.activeElement).outlineWidth) >= 2`))
  await evaluate(`document.querySelector('[data-workspace-links] a[href="/games"]').focus()`)
  await press('Enter')
  await wait(`location.pathname === '/games' && document.querySelector('input[aria-controls="event-catalog-results"]')`)
  await evaluate(`document.querySelector('input[aria-controls="event-catalog-results"]').focus()`)
  await cdp.send('Input.insertText', { text: 'Packet' })
  await wait(`document.querySelectorAll('#event-catalog-results [data-guide="event-card"]').length === 1`)
  await inspect('search-result')
  await press('Escape')
  await wait(`document.querySelectorAll('#event-catalog-results [data-guide="event-card"]').length === 3`)
  assert.ok(await evaluate(`document.activeElement.matches('input[aria-controls="event-catalog-results"]')`))
  await evaluate(`document.querySelector('input[value="joined"]').click()`)
  await wait(`document.querySelectorAll('#event-catalog-results [data-guide="event-card"]').length === 2`)
  await inspect('joined-filter')

  await visit('/admin/dashboard', `document.querySelector('[data-dashboard-stats]')`)
  const beforeRefresh = requests.filter(path => path === '/api/admin/dashboard').length
  await evaluate(`document.querySelector('button[aria-label="Refresh dashboard"]').focus()`)
  await press('Enter')
  await wait(`!document.querySelector('button[aria-label="Refresh dashboard"][data-loading]')`)
  assert.ok(requests.filter(path => path === '/api/admin/dashboard').length > beforeRefresh)
  await evaluate(`document.querySelector('[role="tab"]').focus()`)
  await press('ArrowRight')
  await wait(`document.querySelector('[role="tab"][aria-selected="true"]').textContent.includes('Writeups')`)
  await inspect('dashboard-writeups')

  for (const state of ['empty', 'loading', 'error']) {
    scenario = state
    await visit('/games', state === 'loading' ? `document.querySelector('#event-catalog-results[aria-busy="true"]')` : state === 'error' ? `document.querySelector('[role="alert"]')` : `document.querySelector('#event-catalog-results') && !document.querySelector('#event-catalog-results[aria-busy="true"]')`)
    await inspect('games-' + state)
    if (state === 'error') {
      scenario = 'normal'
      await evaluate(`document.querySelector('[role="alert"] button').focus()`)
      await press('Enter')
      await wait(`document.querySelectorAll('#event-catalog-results [data-guide="event-card"]').length === 3`)
    }
  }
  scenario = 'empty'
  await visit('/', `document.querySelector('[data-home-overview]') && !document.querySelector('.mantine-Skeleton-root')`)
  await inspect('home-empty')
  scenario = 'trend'
  await visit('/admin/dashboard', `document.querySelector('[data-admin-overview] canvas, [data-admin-overview] svg[_echarts_instance_]') || document.querySelector('[_echarts_instance_]')`)
  await inspect('dashboard-trend')
  scenario = 'normal'; role = 'Guest'
  await visit('/games', `document.querySelectorAll('#event-catalog-results [data-guide="event-card"]').length === 3`)
  assert.equal(await evaluate(`document.querySelectorAll('input[value="joined"]').length`), 0)
  await inspect('guest-games')
  assert.equal(mutations.length, 0)
  assert.deepEqual(errors, [])
  assert.ok(reports.every(report => !report.overflow && report.headings === 1 && report.violations.length === 0))
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, errors, blockedMutations: mutations }, null, 2))
  await close()
}
