// Browser-only fixture: exercise organizer confirmation without sending API writes.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture } from './admin-navigation-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const target = new URL(process.env.RSCTF_SCHEDULE_TARGET || 'http://127.0.0.1:18080').origin
const output = resolve('visual-audit-output/schedule-reopen')
mkdirSync(output, { recursive: true })
const now = Date.now()
let saved = { ...fixture('/api/edit/games/19').body, start: now - 172800000, end: now - 3600000, freeze: null, practiceMode: true }
const original = { ...saved }
const writes = [], errors = [], results = []
const { cdp, close } = await launchBrowser()
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise((done) => setTimeout(done, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-1200)')}`)
}
const button = (text) => `Array.from(document.querySelectorAll('button')).find(e => e.textContent.trim() === ${JSON.stringify(text)})`
const dialog = `Array.from(document.querySelectorAll('[role=dialog]')).find(e => e.textContent.includes('Reopen this competition?') && e.getBoundingClientRect().height > 0 && getComputedStyle(e).visibility !== 'hidden')`
const key = async (key, code) => {
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, code: key, windowsVirtualKeyCode: code, ...(key === 'Enter' && type === 'keyDown' ? { text: '\r' } : {}) })
}
const save = async () => {
  await wait(`${button('Save Changes')} && !${button('Save Changes')}.disabled`)
  await new Promise(done => setTimeout(done, 300))
  await evaluate(`${button('Save Changes')}.focus()`)
  await key('Enter', 13)
  await wait(dialog)
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname
    let r
    if (path === '/api/edit/games/19' && request.method === 'PUT') {
      const payload = JSON.parse(request.postData)
      writes.push(payload)
      saved = { ...saved, ...payload, revision: (saved.revision || 0) + 1 }
      r = { body: saved }
    } else if (path === '/api/edit/games/19') r = { body: saved }
    else if (path.startsWith('/hub')) r = { status: 503, body: {} }
    else r = fixture(path, request.method)
    try {
      await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: r.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(r.body ?? {})).toString('base64') })
    } catch (e) { if (!e.message.includes('Invalid InterceptionId')) throw e }
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (window === window.top && location.origin === ${JSON.stringify(target)}) { localStorage.setItem('language', JSON.stringify('en-US')); localStorage.setItem('mantine-color-scheme-value','dark') }` })
  for (const [name, width, height] of [['desktop', 1440, 1100], ['tablet', 768, 1024], ['mobile', 390, 844], ['compact', 320, 568]]) {
    saved = { ...original }
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
    await cdp.send('Page.navigate', { url: 'about:blank' })
    await cdp.send('Page.navigate', { url: target + '/admin/games/19/info#section=general' })
    await wait(`document.querySelector('#game-info-tab-general')?.getAttribute('aria-selected') === 'true'`)
    await evaluate(`(() => { const label = Array.from(document.querySelectorAll('label')).find(e => /End Time/i.test(e.textContent)); document.getElementById(label.htmlFor).click() })()`)
    const tomorrow = new Date(now + 86400000)
    const dayLabel = `${tomorrow.getDate()} ${tomorrow.toLocaleString('en-US', { month: 'long' })} ${tomorrow.getFullYear()}`
    await wait(`document.querySelector('button[aria-label="${dayLabel}"]')`)
    await evaluate(`document.querySelector('button[aria-label="${dayLabel}"]').click()`)
    await key('Escape', 27)
    const before = writes.length
    await save()
    await evaluate('document.fonts.ready')
    await evaluate(readFileSync('web/node_modules/axe-core/axe.min.js', 'utf8'))
    const audit = await evaluate(`(async () => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1, violations: (await axe.run(document, {runOnly: {type:'tag', values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id:v.id, targets:v.nodes.map(n => n.target), html:v.nodes.map(n => n.html)})) }))()`)
    assert.equal(await evaluate(`${dialog}.contains(document.activeElement)`), true)
    for (let i = 0; i < 7; i++) await key('Tab', 9)
    assert.equal(await evaluate(`${dialog}.contains(document.activeElement)`), true)
    const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
    writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
    await key('Escape', 27)
    await wait(`!(${dialog})`)
    await wait(`document.activeElement?.textContent.trim() === 'Save Changes'`)
    assert.equal(writes.length, before, 'cancel must not save')
    await save()
    await evaluate(`${button('Save schedule')}.focus()`)
    await key('Enter', 13)
    await wait(`!(${dialog}) && document.body.innerText.includes('All changes saved')`)
    assert.equal(writes.length, before + 1)
    assert.ok(writes.at(-1).end > now)
    assert.ok(writes.at(-1).operationId)
    assert.deepEqual(audit.violations, [])
    assert.equal(audit.overflow, false)
    results.push({ name, ...audit, cancelNoWrite: true, confirmedWrites: 1, keyboard: true })
    console.log(JSON.stringify(results.at(-1)))
  }
  assert.deepEqual(errors, [])
  writeFileSync(`${output}/results.json`, JSON.stringify({ results, errors }, null, 2))
} finally { await close() }
