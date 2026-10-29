// Delayed reads and actual polling must not move an already-visible challenge toolbar.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = process.env.RSCTF_WORKSPACE_PREVIEW ? null : await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const target = process.env.RSCTF_WORKSPACE_PREVIEW || server.origin
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_LAYOUT_OUTPUT || resolve(repositoryRoot, 'visual-audit-output/layout-stability'))
mkdirSync(output, { recursive: true })
const now = Date.now()
const profile = { userId: '11111111-1111-4111-8111-111111111111', role: 'Admin', userName: 'Fixture' }
const rank = { id: 7, name: 'Test team', rank: 7, score: 100, solvedCount: 0, solvedChallenges: [] }
const game = { id: 901, title: 'Layout stability fixture', start: now - 3600000, end: now + 86400000, serverTime: now, status: 'Accepted', practiceMode: false, divisions: [] }
const challenges = { Web: [{ id: 1, title: 'Defend this service', category: 'Web', type: 'AttackDefense', score: 100, solved: 0 }, { id: 2, title: 'Capture the hill', category: 'Web', type: 'KingOfTheHill', score: 100, solved: 0 }] }
let state, releaseState, releaseCatalog
const responses = {
  '/api/account/profile': profile,
  '/api/config': { title: 'RSCTF', allowRegister: true, enableBrowserFingerprint: false },
  '/api/game/901': game,
  '/api/game/901/details': { challenges, rank, challengeCount: 2 },
  '/api/game/901/details/participant': { rank },
  '/api/game/901/notices': [],
  '/api/game/901/ad/token': { exists: false, canManage: false, revision: 1 },
  '/api/game/901/ad/ssh/key': { exists: false },
}
const { cdp, close } = await launchBrowser().catch(async error => { await server?.close(); throw error })
const reports = [], audits = [], errors = [], unknown = new Set()
const evaluate = async expression => {
  const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const waitFor = async expression => {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise(r => setTimeout(r, 200))
  }
  throw new Error(`Timed out: ${expression}`)
}
const geometry = `(() => {
  const rect = selector => { const e = document.querySelector(selector); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y + scrollY, width: r.width, height: r.height }; };
  return { toolbar: rect('[data-challenge-toolbar]'), status: rect('[data-competition-status]'), tools: rect('[data-competition-status] button'), overflow: document.documentElement.scrollWidth > innerWidth + 1 };
})()`
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', e => errors.push(e.exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if(location.origin===${JSON.stringify(target)}) {
    localStorage.clear(); localStorage.setItem('language', JSON.stringify('en-US'));
    localStorage.setItem('challenge-explorer-view', JSON.stringify('list'));
    localStorage.setItem('rsctf-player-guide:${profile.userId}', JSON.stringify({interactiveEnabled:false,completedVersion:1,seenFeatures:[]}));
  }` })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: `${target}/api/*` }, { urlPattern: `${target}/hub*` }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname.toLowerCase()
    let value = responses[path], status = 200
    if (!['GET', 'HEAD'].includes(request.method)) { status = 405; value = {} }
    else if (path === '/api/game/901/ad/state') { await releaseState; value = state }
    else if (path === '/api/game/901/details') await releaseCatalog
    else if (value === undefined) { unknown.add(path); value = [] }
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: status, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(value)).toString('base64') })
  })
  for (const width of [1600, 768, 390, 320]) {
    let release, showCatalog
    releaseState = new Promise(resolve => { release = resolve })
    releaseCatalog = new Promise(resolve => { showCatalog = resolve })
    state = { currentRound: 178, startRound: 42, epochTicks: 8, roundEndsAt: Date.now() + 60000, flagsReady: true, flagDeliveryFailures: 0, scoringPaused: false, services: [] }
    game.serverTime = Date.now()
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 900, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Page.navigate', { url: `${target}/games/901/challenges` })
    await waitFor(`document.querySelector('[data-challenge-loading]')`)
    const skeleton = await evaluate(`(() => {const r=document.querySelector('[data-challenge-loading]').firstElementChild.getBoundingClientRect(); return {height:r.height,width:r.width,viewport:innerWidth};})()`)
    assert.ok(skeleton.height < 220 && skeleton.width <= skeleton.viewport, 'loading uses the compact explorer toolbar')
    const loadingShot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
    writeFileSync(`${output}/${width}-loading.png`, Buffer.from(loadingShot.data, 'base64'))
    showCatalog()
    await waitFor(`document.querySelector('[data-challenge-toolbar]') && document.querySelector('[data-competition-status]')`)
    await evaluate('document.fonts.ready')
    const loading = await evaluate(geometry)
    release()
    await waitFor(`document.querySelector('[data-competition-status] dd')?.textContent === '178'`)
    const loaded = await evaluate(geometry)
    reports.push({ width, phase: 'initial state', before: loading, after: loaded })
    await evaluate(`document.getElementById('challenge-search').focus()`)
    for (const phase of ['awaiting', 'failures', 'recovered']) {
      Object.assign(state, { currentRound: state.currentRound + 1, roundEndsAt: phase === 'awaiting' ? Date.now() - 5000 : Date.now() + 60000, flagDeliveryFailures: phase === 'failures' ? 12 : 0, flagsReady: phase !== 'failures' })
      await waitFor(`document.querySelector('[data-competition-status] dd')?.textContent === '${state.currentRound}'`)
      const after = await evaluate(geometry)
      reports.push({ width, phase, before: loaded, after })
      assert.equal(await evaluate('document.activeElement.id'), 'challenge-search', 'polling preserves the focused search field')
    }
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
    writeFileSync(`${output}/${width}.png`, Buffer.from(shot.data, 'base64'))
    // Explicit disclosure is allowed to expand; its live updates keep the same height.
    await evaluate(`document.querySelector('[data-round-notices-toggle]').focus()`)
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r' })
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 })
    await waitFor(`document.querySelector('[data-round-messages]').open`)
    const expanded = await evaluate(geometry)
    Object.assign(state, { currentRound: state.currentRound + 1, flagDeliveryFailures: 12, flagsReady: false })
    await waitFor(`document.querySelector('[data-competition-status] dd')?.textContent === '${state.currentRound}'`)
    reports.push({ width, phase: 'expanded notices update', before: expanded, after: await evaluate(geometry) })
    await evaluate(readFileSync(resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'), 'utf8'))
    audits.push({ width, violations: await evaluate(`(async () => (await axe.run(document, {runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id:v.id,targets:v.nodes.map(n=>n.target)})))()`) })
  }
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, audits, errors, unknown: [...unknown] }, null, 2))
  await close()
  await server?.close()
}
assert.deepEqual(errors, [])
assert.deepEqual([...unknown], [])
assert.deepEqual(audits.filter(a => a.violations.length), [])
for (const { width, phase, before, after } of reports) {
  assert.equal(after.overflow, false, `${width} ${phase}: overflow`)
  for (const part of ['toolbar', 'status', 'tools']) for (const key of ['x', 'y', 'width', 'height']) {
    assert.ok(Math.abs(before[part][key] - after[part][key]) <= 1, `${width} ${phase}: ${part}.${key} moved ${before[part][key]} → ${after[part][key]}`)
  }
}
console.log(`PASS: ${reports.length} delayed-read and polling layout checks`)
