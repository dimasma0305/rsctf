// Exercise the built client with invented, read-only data; never send API writes.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture as adminFixture } from './admin-navigation-fixtures.mjs'
import { createCompetitionFixture } from './competition-fixtures.mjs'
import { gradingFixture } from './writeup-grading-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const target = new URL(process.env.RSCTF_URL_NAV_TARGET || 'http://127.0.0.1:18080').origin
const output = resolve('visual-audit-output', process.env.RSCTF_URL_NAV_OUTPUT || 'url-navigation')
mkdirSync(output, { recursive: true })
const player = createCompetitionFixture()
const grading = gradingFixture()
const now = Date.now()
const report = { generatedAt: now, lastReconciledAt: now, sealedAt: now, ipAnalysis: [], abnormalSolves: [], collusionGroups: [], suspicionList: [], identityOverlaps: [] }
const { cdp, close } = await launchBrowser()
const errors = [], unknown = [], writes = [], results = []
const reads = new Map()
let playerMode = false
let anonymous = false
const evaluate = async (expression) => {
  const response = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (response.exceptionDetails) throw new Error(response.exceptionDetails.exception?.description || response.exceptionDetails.text)
  return response.result?.value
}
const wait = async (expression) => {
  for (let attempt = 0; attempt < 150; attempt++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise((done) => setTimeout(done, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('location.href')}; ${await evaluate('JSON.stringify(Array.from(document.querySelectorAll("[role=tab]")).map(e => [e.id, e.getAttribute("aria-selected")]))')}; ${await evaluate('document.body.innerText.slice(-700)')}`)
}
const tab = (value) => `[role="tab"][id$="-tab-${value}"]`
const selected = (selector) => `document.querySelector(${JSON.stringify(selector)})?.getAttribute('aria-selected') === 'true'`
const click = async (selector) => {
  await wait(`document.querySelector(${JSON.stringify(selector)})`)
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).focus(); document.querySelector(${JSON.stringify(selector)}).click()`)
}
const visit = async (path, ready) => {
  // Force a fresh document even when only the fragment differs from this URL.
  await cdp.send('Page.navigate', { url: 'about:blank' })
  await cdp.send('Page.navigate', { url: target + path })
  await wait(ready)
}
const audit = async (name) => {
  await evaluate('document.fonts.ready')
  await evaluate('Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))')
  await new Promise((done) => setTimeout(done, 500))
  await evaluate(readFileSync('web/node_modules/axe-core/axe.min.js', 'utf8'))
  const result = await evaluate(`(async () => ({url: location.pathname + location.search + location.hash, overflow: document.documentElement.scrollWidth > innerWidth + 1, violations: (await axe.run(document, {runOnly: {type: 'tag', values: ['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id: v.id, targets: v.nodes.map(n => n.target)}))}))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  results.push({ name, ...result })
  console.log(name, JSON.stringify(result))
}

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const url = new URL(request.url)
    if (request.method === 'GET') reads.set(url.pathname, (reads.get(url.pathname) || 0) + 1)
    let response
    if (url.pathname.startsWith('/hub')) response = { body: { error: 'Read-only navigation fixture' } }
    else if (!['GET', 'HEAD'].includes(request.method)) {
      writes.push(url.pathname)
      response = { status: 405, body: {} }
    } else if (anonymous && !['/api/config', '/api/captcha'].includes(url.pathname)) response = { status: 401, body: { status: 401 } }
    else if (playerMode) response = player.fixture(url.pathname + url.search, request.method)
    else if (url.pathname === '/api/account/stats') response = { body: { totalSolves: 0, totalFirstBloods: 0, gamesParticipated: 0, solvesByCategory: {}, games: [] } }
    else if (url.pathname === '/api/game/19') response = { body: { ...adminFixture('/api/edit/games/19').body, start: now - 3600000, end: now + 3600000 } }
    else if (url.pathname === '/api/game/19/cheatreport') response = { body: report }
    else if (url.pathname === '/api/game/19/cheatinfo') response = { body: [] }
    else if (url.pathname === '/api/game/19/cheatinfo/page') response = { body: { data: [], nextBefore: null, checkpointId: 0, hasMore: false } }
    else if (url.pathname.toLowerCase() === '/api/edit/games/19/ad/services/2/snapshots') response = { body: [] }
    else if (url.pathname === '/api/admin/games/19/vpn-overrides') response = { body: { policyRevision: 1, activeLimit: 5, overrides: [] } }
    else response = grading(url.pathname + url.search, request.method)
    if (response.unknown || response.status === 404) unknown.push(url.pathname)
    try {
      await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: response.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(response.body)).toString('base64') })
    } catch (error) {
      // A forced document reload can retire an intercepted old-document read.
      if (!error.message.includes('Invalid InterceptionId')) throw error
    }
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (window === window.top && location.origin === ${JSON.stringify(target)}) { localStorage.setItem('language', JSON.stringify('en-US')); localStorage.setItem('mantine-color-scheme-value', 'dark'); for (const id of ['guest', '${player.profile.userId}']) localStorage.setItem('rsctf-player-guide:' + id, JSON.stringify({interactiveEnabled:false, completedVersion:5, seenFeatures:[], activeTourStep:null, tourPaused:true})); }` })
  for (const [viewport, width, height] of [['desktop',1440,1100], ['compact',320,568]]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
    const cheatPath = '/games/19/monitor/cheatcheck?section=abnormal-solves&unrelated=keep#anchor'
    await visit(cheatPath, selected(tab('abnormal-solves')))
    await audit(viewport + '-abnormal-solves')
    const reportReads = reads.get('/api/game/19/cheatreport')
    await click(tab('network-device'))
    await wait(selected(tab('network-device')))
    assert.equal(await evaluate("new URLSearchParams(location.search).get('section')"), null)
    assert.equal(await evaluate('location.hash'), '#anchor&section=network-device')
    assert.equal(await evaluate("new URLSearchParams(location.search).get('unrelated')"), 'keep')
    await evaluate('history.back()')
    await wait(selected(tab('abnormal-solves')))
    await evaluate('history.forward()')
    await wait(selected(tab('network-device')))
    assert.equal(reads.get('/api/game/19/cheatreport'), reportReads, 'evidence navigation retains its existing report request owner')
    const copied = await evaluate('location.pathname + location.search + location.hash')
    await visit(copied, selected(tab('network-device')))
    await audit(viewport + '-network-device')
    await evaluate(`document.querySelector(${JSON.stringify(tab('network-device'))}).focus()`)
    for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key: 'ArrowRight', windowsVirtualKeyCode: 39 })
    await wait(selected(tab('abnormal-solves')))
    const submissions = `[role=tablist]:has(${tab('analysis')}) ${tab('submissions')}`
    await click(submissions)
    await wait(selected(submissions))
    await click(tab('analysis'))
    await wait(selected(tab('abnormal-solves')))
    for (const [path, value] of [
      ['/admin/dashboard#activity=writeups', 'writeups'],
      ['/admin/builds#tab=images', 'images'],
      ['/admin/settings#section=email', 'email'],
      ['/admin/games/19/info#section=security', 'security'],
      ['/admin/games/19/writeups#tab=ranking', 'ranking'],
    ]) {
      await visit(path, selected(tab(value)))
      await audit(viewport + '-' + value)
      if (value === 'security') {
        await click('#game-info-tab-general')
        await wait(selected('#game-info-tab-general'))
        await click('#game-info-panel input[type=text]')
        await cdp.send('Input.insertText', { text: 'Unsaved URL navigation draft' })
        const draft = await evaluate('document.querySelector("#game-info-panel input[type=text]").value')
        await click('#game-info-tab-security')
        await wait(selected('#game-info-tab-security'))
        await evaluate('history.back()')
        await wait(selected('#game-info-tab-general'))
        assert.equal(await evaluate('document.querySelector("#game-info-panel input[type=text]").value'), draft)
      }
      if (value === 'email') {
        await click('#settings-tab-platform')
        await wait('document.querySelector("#settings-panel input[placeholder=RS]")')
        await click('#settings-panel input[placeholder=RS]')
        await cdp.send('Input.insertText', { text: 'Unsaved hash settings draft' })
        const draft = await evaluate('document.querySelector("#settings-panel input[placeholder=RS]").value')
        const requests = reads.get('/api/admin/config')
        await click('#settings-tab-email')
        await evaluate('history.back()')
        await wait(selected('#settings-tab-platform'))
        assert.equal(await evaluate('document.querySelector("#settings-panel input[placeholder=RS]").value'), draft)
        assert.equal(reads.get('/api/admin/config'), requests)
      }
    }
    await visit('/account/profile?tab=stats', selected(tab('stats')))
    await wait('document.body.innerText.includes("No solves yet")')
    await audit(viewport + '-profile-stats')
    await click(tab('profile'))
    await wait('document.querySelector("[data-profile-form] input")')
    await click('[data-profile-form] input')
    await cdp.send('Input.insertText', { text: 'Unsaved profile draft' })
    const profileDraft = await evaluate('document.querySelector("[data-profile-form] input").value')
    await click(tab('stats'))
    assert.equal(await evaluate('location.search'), '')
    assert.equal(await evaluate('location.hash'), '#tab=stats')
    await evaluate('history.back()')
    await wait(selected(tab('profile')))
    assert.equal(await evaluate('document.querySelector("[data-profile-form] input").value'), profileDraft)
    await visit('/account/profile#tab=stats', selected(tab('stats')))
    await visit('/admin/games/19/adops#view=koth', 'document.querySelector("input[value=koth]")?.checked')
    await audit(viewport + '-koth')
    await click('input[value=ad]')
    await wait('document.querySelector("input[value=ad]")?.checked')
    assert.equal(await evaluate("new URLSearchParams(location.hash.slice(1)).get('view')"), 'ad')
    await visit('/admin/games/19/adops#view=ad&snapshotTab=history&snapshot=2', 'document.querySelector("[role=dialog] input[value=history]")?.checked')
    await audit(viewport + '-snapshot-history')
    await click('[role=dialog] input[value=changes]')
    await wait('document.querySelector("[role=dialog] input[value=changes]")?.checked')
    await evaluate('new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))')
    assert.equal(await evaluate("new URLSearchParams(location.hash.slice(1)).get('snapshot')"), '2')
    assert.equal(await evaluate("new URLSearchParams(location.hash.slice(1)).get('snapshotTab')"), 'changes')
    await click('[role=dialog] button[aria-label=Close]')
    await wait('!document.querySelector("[role=dialog]")')
    assert.equal(await evaluate('location.hash'), '#view=ad&snapshotTab=changes')
    playerMode = true
    await visit('/games/901/challenges#9001-Ret2win&category=Pwn&view=list&sort=score', selected(tab('Pwn')))
    await wait('document.body.innerText.includes("Challenge files: Ret2win")')
    await audit(viewport + '-challenge-deep-link')
    await click('[role=dialog] button[aria-label=Close], [data-challenge-detail] button[aria-label=Close]')
    await wait('!document.body.innerText.includes("Challenge files: Ret2win")')
    assert.equal(await evaluate('location.hash'), '#category=Pwn&view=list&sort=score')
    await click('[data-challenge-row="9006"]')
    await wait('document.body.innerText.includes("Challenge files: Challenge 006")')
    assert.equal(await evaluate('location.hash'), '#9006-Challenge-006&category=Pwn&view=list&sort=score')
    playerMode = false
  }
  anonymous = true
  await visit('/admin/settings?keep=1#section=email', 'location.pathname === "/account/login"')
  assert.equal(await evaluate('new URLSearchParams(location.search).get("from")'), '/admin/settings?keep=1#section=email')
  await visit('/account/profile#tab=stats', 'document.querySelector("[data-profile-page] [role=alert]")')
  assert.equal(await evaluate('Array.from(document.querySelectorAll("a[href*=from]")).every(link => new URL(link.href).searchParams.get("from") === "/account/profile#tab=stats")'), true)
  assert.deepEqual(writes, [], 'navigation must never perform mutations')
  assert.deepEqual(unknown, [], 'fixtures must cover every read')
  assert.deepEqual(errors, [], 'navigation must not throw runtime errors')
  assert.ok(results.every((result) => !result.overflow && result.violations.length === 0), 'no overflow or Axe violations')
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ target, results, errors, unknown, writes, reads: Object.fromEntries(reads) }, null, 2))
  await close()
}
