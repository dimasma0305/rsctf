// Built-client hash navigation, native links, history and access-gate regression.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { createChallengeLinksFixture } from './challenge-links-fixtures.mjs'

const target = new URL(process.env.RSCTF_CHALLENGE_LINK_TARGET || 'http://127.0.0.1:18080').origin
const output = resolve('visual-audit-output', process.env.RSCTF_CHALLENGE_LINK_OUTPUT || 'challenge-links')
mkdirSync(output, { recursive: true })
const player = createChallengeLinksFixture()
const { cdp, close } = await launchBrowser()
const errors = [], unknown = [], writes = [], reads = [], results = []
let anonymous = false, failedId = null, heldId = null, releaseHeld = null
const evaluate = async expression => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async expression => {
  for (let attempt = 0; attempt < 150; attempt++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise(done => setTimeout(done, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('location.href')}; ${await evaluate('document.body.innerText.slice(-900)')}`)
}
const visit = async (path, ready) => {
  // Explicit reload also covers copied URLs that happen to match the current URL.
  await cdp.send('Page.navigate', { url: 'about:blank' })
  await cdp.send('Page.navigate', { url: target + path })
  await wait(ready)
}
const press = async (key, windowsVirtualKeyCode) => {
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key, windowsVirtualKeyCode, ...(key === 'Enter' ? { text: '\r' } : {}) })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key, windowsVirtualKeyCode })
}
const audit = async name => {
  await evaluate('document.fonts.ready')
  await evaluate('Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))')
  await new Promise(done => setTimeout(done, 500))
  await evaluate(readFileSync('web/node_modules/axe-core/axe.min.js', 'utf8'))
  const result = await evaluate(`(async () => ({url: location.pathname + location.search + location.hash, overflow: document.documentElement.scrollWidth > innerWidth + 1, violations: (await axe.run(document, {runOnly: {type: 'tag', values: ['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id: v.id, targets: v.nodes.map(n => n.target)}))}))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  results.push({ name, ...result })
  console.log(name, JSON.stringify(result))
}
const cardsReady = 'document.querySelectorAll("#challenge-catalog-results article a").length === 24'
const detailReady = title => `document.body.innerText.includes(${JSON.stringify('Challenge files: ' + title)})`
const noModal = '!document.querySelector("[role=dialog]")'

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const url = new URL(request.url)
    let response
    if (url.pathname.startsWith('/hub')) response = { body: { error: 'Read-only fixture' } }
    else if (!['GET', 'HEAD'].includes(request.method)) {
      writes.push(url.pathname)
      response = { status: 405, body: {} }
    } else {
      reads.push(url.pathname + url.search)
      if (anonymous && url.pathname === '/api/account/profile') response = { status: 401, body: { status: 401 } }
      else if (failedId && url.searchParams.get('challengeId') === failedId) response = { status: 503, body: { status: 503 } }
      else response = player.fixture(url.pathname + url.search, request.method)
    }
    if (response.status === 404) unknown.push(url.pathname)
    if (heldId && url.searchParams.get('challengeId') === heldId) await new Promise(done => { releaseHeld = done })
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: response.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(response.body)).toString('base64') })
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (window === window.top && location.origin === ${JSON.stringify(target)}) { localStorage.setItem('language', JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');for (const id of ['guest','${player.profile.userId}']) localStorage.setItem('rsctf-player-guide:' + id, JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true})); }` })
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
  for (const [viewport, width, height] of [['desktop',1440,1100], ['compact',320,568], ['mobile',390,844], ['tablet',768,1024], ['wide',1920,1080]]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await visit('/challenges?keep=1#section=notes', cardsReady)
    await audit(viewport + '-catalog')
    const href = await evaluate('document.querySelector("#challenge-catalog-results article a").href')
    assert.equal(href, target + '/challenges?keep=1#section=notes&challenge=9001')
    const catalogReads = reads.filter(path => path.startsWith('/api/game/challenges')).length
    await evaluate('document.querySelector("#challenge-catalog-results article a").focus()')
    await press('Enter', 13)
    await wait(detailReady('Ret2win'))
    assert.equal(await evaluate('location.href'), href)
    assert.equal(reads.filter(path => path.startsWith('/api/game/challenges')).length, catalogReads, 'opening a visible card needs no extra catalog read')
    await audit(viewport + '-catalog-detail')
    await evaluate('history.back()')
    await wait(noModal)
    await wait('document.activeElement === document.querySelector("#challenge-catalog-results article a")')
    await evaluate('history.forward()')
    await wait(detailReady('Ret2win'))
    await press('Escape', 27)
    await wait(noModal)
    assert.equal(await evaluate('location.hash'), '#section=notes')
    await visit(new URL(href).pathname + new URL(href).search + new URL(href).hash, detailReady('Ret2win'))
    await press('Escape', 27)
    await wait(noModal)

    // A card outside the first page still opens after a fresh load.
    await visit('/challenges#challenge=9099', detailReady('Challenge 099'))
    assert.ok(reads.some(path => path.includes('challengeId=9099')))
    await press('Escape', 27)
    await wait(noModal)
    await evaluate('document.querySelector("input[aria-controls=challenge-catalog-results]").focus()')
    await cdp.send('Input.insertText', { text: 'Challenge 099' })
    await wait('document.querySelectorAll("#challenge-catalog-results article a").length === 1')
    await evaluate('document.querySelector("#challenge-catalog-results article a").focus()')
    await press('Enter', 13)
    await wait(detailReady('Challenge 099'))
    await evaluate('history.back()')
    await wait(noModal)
    assert.equal(await evaluate('document.querySelector("input[aria-controls=challenge-catalog-results]").value'), 'Challenge 099')

    for (const view of ['cards', 'list']) {
      await visit('/games/901/challenges?view=' + view, 'document.querySelector("article[data-guide=challenge-card] a, [data-challenge-row]")')
      const selector = view === 'cards' ? 'article[data-guide=challenge-card] a' : '[data-challenge-row]'
      const link = await evaluate(`document.querySelector(${JSON.stringify(selector)}).href`)
      assert.match(link, /\/games\/901\/challenges\?view=(?:cards|list)#\d+-/)
      await evaluate(`document.querySelector(${JSON.stringify(selector)}).focus()`)
      await press('Enter', 13)
      await wait('document.body.innerText.includes("Challenge files:")')
      assert.equal(await evaluate('location.href'), link)
      await audit(viewport + '-event-' + view)
      await evaluate('history.back()')
      await wait('!document.body.innerText.includes("Challenge files:")')
      await wait(`document.activeElement === document.querySelector(${JSON.stringify(selector)})`)
    }
  }
  const beforeDenied = reads.length
  await visit('/challenges#challenge=999999', 'document.body.innerText.includes("unavailable or you do not have access")')
  await audit('unavailable-link')
  assert.ok(!reads.slice(beforeDenied).some(path => /\/api\/game\/\d+\/challenges\//.test(path)), 'an unauthorized ID never enables challenge detail reads')
  const beforeInvalid = reads.length
  await visit('/challenges#challenge=1&challenge=2', cardsReady)
  assert.ok(!reads.slice(beforeInvalid).some(path => path.includes('challengeId=')), 'ambiguous hashes do not perform lookups')
  assert.equal(await evaluate(noModal), true)

  failedId = '9099'
  await visit('/challenges#challenge=9099', 'document.body.innerText.includes("could not be loaded")')
  await audit('failed-link')
  failedId = null
  await evaluate('Array.from(document.querySelectorAll("[role=dialog] button")).find(button => button.textContent === "Retry").click()')
  await wait(detailReady('Challenge 099'))

  heldId = '9099'
  await visit('/challenges#challenge=9099', 'document.body.innerText.includes("Loading challenge…")')
  await press('Escape', 27)
  await wait(noModal)
  assert.ok(releaseHeld)
  heldId = null
  releaseHeld()
  await new Promise(done => setTimeout(done, 750))
  assert.equal(await evaluate(noModal), true, 'late lookup responses cannot reopen a closed card')

  anonymous = true
  const beforeAnonymous = reads.length
  await visit('/challenges?keep=1#challenge=9099', 'location.pathname === "/account/login"')
  assert.equal(await evaluate('new URLSearchParams(location.search).get("from")'), '/challenges?keep=1#challenge=9099')
  assert.ok(!reads.slice(beforeAnonymous).some(path => path.startsWith('/api/game/challenges')), 'anonymous users never fetch the catalog')
  assert.deepEqual(writes, [])
  assert.deepEqual(unknown, [])
  assert.deepEqual(errors, [])
  assert.ok(results.every(result => !result.overflow && result.violations.length === 0))
} finally {
  if (releaseHeld) releaseHeld()
  writeFileSync(`${output}/report.json`, JSON.stringify({ target, results, errors, unknown, writes, reads }, null, 2))
  await close()
}
