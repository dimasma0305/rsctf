import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture } from './event-readiness-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_READINESS_TARGET || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://intechfest.1pc.tf', 'https://tcp.1pc.tf'].includes(target))
const output = resolve(process.env.RSCTF_READINESS_OUTPUT || '../visual-audit-output/event-readiness')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const reports = [], writes = [], unknown = [], errors = [], reads = []
let scenario = 'normal', role = 'Admin'
let paused = []
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('Timed out: ' + expression + '; ' + await evaluate('document.body.innerText.slice(-1500)'))
}
const fulfill = async (requestId, response) => cdp.send('Fetch.fulfillRequest', {
  requestId, responseCode: response.status || 200,
  responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
  body: Buffer.from(JSON.stringify(response.body)).toString('base64'),
})
const visit = async (expected = '[data-readiness-snapshot]') => {
  await evaluate('window.__readinessOldDocument = true')
  await cdp.send('Page.navigate', { url: target + '/admin/games/19/readiness' })
  await wait(`!window.__readinessOldDocument && document.querySelector(${JSON.stringify(expected)})`)
}
const audit = async (name) => {
  await evaluate('Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))')
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const report = await evaluate(`(async () => ({overflow: document.documentElement.scrollWidth > innerWidth + 1, headings: document.querySelectorAll('h1').length, violations: (await axe.run(document, {runOnly: {type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id:v.id,nodes:v.nodes.map(n => ({target:n.target,summary:n.failureSummary}))}))}))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  reports.push({ name, ...report })
  console.log(name, JSON.stringify(report))
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  // All API/hub calls are fulfilled locally, including unexpected mutations.
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    try {
      const path = new URL(request.url).pathname
      if (!['GET', 'HEAD'].includes(request.method)) writes.push({ path, method: request.method })
      else reads.push(path)
      const response = fixture(path, request.method, scenario, role)
      if (response.unknown) unknown.push(response.unknown)
      if (response.hold) paused.push({ requestId, path, method: request.method })
      else await fulfill(requestId, response)
    } catch (error) { errors.push(String(error)) }
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `for(const id of ['guest','11111111-1111-4111-8111-111111111111'])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));` })
  for (const [name, width, height, language, scheme] of [
    ['desktop', 1440, 1000, 'en-US', 'dark'], ['compact', 320, 568, 'en-US', 'dark'],
    ['light-id', 390, 844, 'id-ID', 'light'],
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});` })
    await visit()
    await audit(name)
    assert.equal(await evaluate('document.querySelectorAll("[data-readiness-check]").length'), 5)
    assert.ok(await evaluate(`Array.from(document.querySelectorAll('[data-readiness-check]')).every(check => getComputedStyle(check).borderRadius === '0px')`),'checks are compact rows, not separate cards')
    assert.equal(await evaluate('document.querySelector("[data-readiness-check]").dataset.readinessCheck'), 'challenges')
    if (name === 'compact') {
      await evaluate(`document.querySelector('[data-readiness-check="builds"]').scrollIntoView({block:'center'})`)
      await audit('compact-checks')
      assert.ok(await evaluate(`Array.from(document.querySelectorAll('[data-readiness-check] a')).every(link => link.getBoundingClientRect().height >= 44)`))
    }
  }
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');` })
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false })
  role = 'Manager'
  await visit()
  await audit('event-manager')
  assert.equal(await evaluate(`!!document.querySelector('nav a[href="/admin/games/19/managers"]')`), false)
  role = 'Admin'
  scenario = 'loading'
  await visit('[data-readiness-loading]')
  await audit('loading')
  scenario = 'normal'
  for (const request of paused) await fulfill(request.requestId, fixture(request.path, request.method))
  paused = []
  await wait(`document.querySelector('[data-readiness-snapshot]')`)
  const before = reads.filter(path => /^\/api\/edit\/games\/19(?:\/challenges)?$/.test(path)).length
  scenario = 'failed'
  await evaluate(`document.querySelector('[data-readiness-refresh]').focus()`)
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key: 'Enter', windowsVirtualKeyCode: 13, ...(type === 'keyDown' ? { text: '\r', unmodifiedText: '\r' } : {}) })
  await wait(`document.querySelector('[data-readiness-error]') && document.body.innerText.includes('Showing previous data')`)
  assert.equal(reads.filter(path => /^\/api\/edit\/games\/19(?:\/challenges)?$/.test(path)).length - before, 2)
  await audit('failed-refresh-retains-stale-data')
  scenario = 'denied'
  await evaluate(`document.querySelector('[data-readiness-refresh]').click()`)
  await wait(`!document.querySelector('[data-readiness-snapshot]') && document.body.innerText.includes('manager assignment')`)
  await audit('permission-revoked-hides-cache')
  for (const [state, text] of [['missing', 'could not be found'], ['busy', 'Too many requests'], ['failed', 'server could not complete']]) {
    scenario = state
    await visit('[data-readiness-error]')
    await wait(`document.body.innerText.includes(${JSON.stringify(text)})`)
    assert.equal(await evaluate(`!!document.querySelector('[data-readiness-snapshot]')`), false)
    await audit(state)
  }
  scenario = 'empty'
  await visit()
  await wait(`document.body.innerText.includes('No enabled, approved challenges')`)
  await audit('empty-event')
  scenario = 'normal'
  await evaluate(`document.querySelector('[data-readiness-refresh]').click()`)
  await wait(`document.body.innerText.includes('failed latest build')`)
  await evaluate(`document.querySelector('a[aria-label="Review Latest build results settings"]').focus()`)
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key: 'Enter', windowsVirtualKeyCode: 13, ...(type === 'keyDown' ? { text: '\r', unmodifiedText: '\r' } : {}) })
  await wait(`location.pathname === '/admin/games/19/challenges'`)
  assert.deepEqual(writes, [])
  assert.deepEqual(unknown, [])
  assert.deepEqual(errors, [])
  assert.ok(reports.every(report => report.headings === 1 && !report.overflow && !report.violations.length))
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ target, reports, writes, unknown, errors, reads }, null, 2))
  await close()
}
