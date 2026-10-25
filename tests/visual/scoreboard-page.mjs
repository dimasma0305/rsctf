// Real keyboard/focus acceptance against the disposable scoreboard fixture.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = new URL(process.env.RSCTF_VISUAL_TARGET || 'http://127.0.0.1:18189')
const game = Number(process.env.RSCTF_VISUAL_GAME_ID)
const team = process.env.RSCTF_VISUAL_TEAM_NAME || 'PLATFORM_BENCH_00500'
const token = readFileSync(process.env.RSCTF_VISUAL_PLAYER_JWT_FILE || '', 'utf8').trim()
if (!['localhost', '127.0.0.1', '::1'].includes(target.hostname) || !Number.isInteger(game) || game < 1
    || token.length < 32 || token.length > 4096 || Buffer.byteLength(team) > 512) {
  throw new Error('loopback target, GAME_ID, bounded team name and disposable PLAYER_JWT_FILE required')
}
const output = resolve(new URL('../../visual-audit-output/scoreboard-page/', import.meta.url).pathname)
rmSync(output, { recursive: true, force: true })
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const requests = []
let simulatedPage
const interceptionErrors = []
cdp.on('Fetch.requestPaused', event => {
  const response = simulatedPage ? cdp.send('Fetch.fulfillRequest', {
    requestId: event.requestId, responseCode: 200,
    responseHeaders: [{ name: 'Content-Type', value: 'application/json' },
      { name: 'ETag', value: '"scoreboard-dialog-regression"' },
      { name: 'X-Scoreboard-Version', value: 'scoreboard-dialog-regression' }],
    body: Buffer.from(JSON.stringify(simulatedPage)).toString('base64'),
  }) : cdp.send('Fetch.continueRequest', { requestId: event.requestId })
  response.catch(error => interceptionErrors.push(error.message))
})
cdp.on('Network.requestWillBeSent', ({ request }) => {
  const url = new URL(request.url)
  if (url.pathname.startsWith(`/api/game/${game}/scoreboard`)) requests.push(url.pathname + url.search)
})
const evaluate = async expression => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.text)
  return result.result.value
}
const wait = async expression => {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    if (await evaluate(expression)) return
    await new Promise(resolveWait => setTimeout(resolveWait, 100))
  }
  throw new Error(`timed out waiting for ${expression}`)
}
const rows = `document.querySelectorAll('[data-team-name]')`
const firstRank = `Number(document.querySelector('[data-team-name]')?.cells[0]?.textContent.trim())`
const search = `Array.from(document.querySelectorAll('input')).find(input =>
  /search.*team/i.test([input.getAttribute('aria-label'), input.placeholder,
    ...Array.from(input.labels || []).map(label => label.textContent)].join(' ')))`
const mine = `Array.from(document.querySelectorAll('button')).find(button =>
  /find my team/i.test(button.getAttribute('aria-label') || button.textContent))`
const enter = async () => {
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Enter', code: 'Enter',
    windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13, text: '\r', unmodifiedText: '\r' })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 })
}

const modalSurvivesPageChange = async (skip, count) => {
  const response = await fetch(`${target.origin}/api/game/${game}/scoreboard/page?skip=${skip}&count=${count + 1}`)
  assert.equal(response.status, 200)
  const next = await response.json()
  const selectedName = next.items[0].name
  await evaluate(`document.querySelector('[data-team-name] button').focus()`)
  await enter()
  await wait(`document.querySelector('[role="dialog"]')?.textContent.includes(${JSON.stringify(selectedName)})`)
  simulatedPage = { ...next, count, items: next.items.slice(1), snapshotVersion: 'dialog-regression' }
  await wait(`${firstRank} === ${skip + 2}`)
  await new Promise(resolve => setTimeout(resolve, 500)) // Let modal exit transitions finish.
  assert.equal(await evaluate(`Boolean(document.querySelector('[role="dialog"]'))`), true,
    'a live update moving the team off the page must keep its details open')
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 })
  await wait(`!document.querySelector('[role="dialog"]')`)
  simulatedPage = undefined
  await wait(`${firstRank} === ${skip + 1}`)
  assert.equal(await evaluate(`Boolean(document.querySelector('[role="dialog"]'))`), false,
    'the returning team must not reopen explicitly closed details')
}

try {
  await cdp.send('Page.enable')
  await cdp.send('Page.bringToFront')
  await cdp.send('Runtime.enable')
  await cdp.send('Network.enable')
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: `${target.origin}/api/game/${game}/scoreboard/page*` }] })
  // Exercise real completion polling quickly, without waiting thirty seconds
  // per simulated rank change. All other timer durations remain unchanged.
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `{
    const original = window.setTimeout;
    window.setTimeout = (callback, delay, ...args) => original(callback,
      delay >= 20000 && delay <= 40000 ? 1000 : delay, ...args);
  }` })
  await cdp.send('Network.setCookie', { name: 'RSCTF_Token', value: token, url: target.origin,
    secure: false, httpOnly: true, sameSite: 'Lax' })
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1100, deviceScaleFactor: 1, mobile: false })
  await cdp.send('Page.navigate', { url: `${target.origin}/games/${game}/scoreboard#jeopardy` })
  await wait(`${rows}.length === 30 && Boolean(${mine})`)
  // Hold API responses long enough to expose a transient loader/focus regression.
  await cdp.send('Network.emulateNetworkConditions', { offline: false, latency: 250,
    downloadThroughput: -1, uploadThroughput: -1 })
  await evaluate(`Array.from(document.querySelectorAll('button')).find(button =>
    /^next(?: page)?$/i.test(button.getAttribute('aria-label') || button.title)).focus()`)
  await enter()
  await wait(`${firstRank} === 31 && ${rows}.length === 30`)
  await modalSurvivesPageChange(30, 30)

  await evaluate(`window.scoreboardSearch = (${search}); window.scoreboardSearch.focus()`)
  await cdp.send('Input.insertText', { text: '00500' })
  await wait(`${rows}.length === 1 && document.querySelector('[data-team-name]')?.getAttribute('data-team-name') === ${JSON.stringify(team)}`)
  assert.equal(await evaluate('document.activeElement === window.scoreboardSearch'), true, 'search focus must survive the server request')
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', code: 'KeyA', modifiers: 2, windowsVirtualKeyCode: 65 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'a', code: 'KeyA', modifiers: 2, windowsVirtualKeyCode: 65 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 })
  await wait(`${rows}.length === 30 && ${firstRank} === 1`)
  await evaluate(`(${mine}).focus()`)
  await enter()
  await wait(`Array.from(${rows}).some(row => row.getAttribute('data-team-name') === ${JSON.stringify(team)})`)
  assert.equal(await evaluate(`${firstRank} > 30`), true, 'lookup must reach a team outside the loaded page')
  const desktop = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(resolve(output, 'desktop.png'), Buffer.from(desktop.data, 'base64'))

  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 320, height: 568, deviceScaleFactor: 1, mobile: true })
  await wait(`${rows}.length === 10 && ${firstRank} === 1`)
  await modalSurvivesPageChange(0, 10)
  await evaluate(`(${mine}).focus()`)
  await enter()
  await wait(`${rows}.length === 10 && Array.from(${rows}).some(row => row.getAttribute('data-team-name') === ${JSON.stringify(team)})`)
  assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth'), true)
  const mobile = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(resolve(output, 'compact.png'), Buffer.from(mobile.data, 'base64'))
  assert.equal(requests.some(path => path === `/api/game/${game}/scoreboard`), false, 'the table must not download the full board')
  assert.deepEqual(interceptionErrors, [])
  const result = { keyboardPagination: true, searchFocusRetained: true, lookupOutsidePage: true,
    desktopDialogSurvivesRowMove: true, compactDialogSurvivesRowMove: true,
    desktopPageSize: 30, compactPageSize: 10, overflow: false, fullBoardReads: 0, requests }
  writeFileSync(resolve(output, 'result.json'), JSON.stringify(result, null, 2))
  console.log(JSON.stringify(result))
} catch (error) {
  const diagnostic = await evaluate(`({ location: location.href, title: document.title,
    rows: ${rows}.length, firstRank: ${firstRank}, active: document.activeElement?.outerHTML,
    text: document.body.innerText.slice(0, 2500),
    buttons: Array.from(document.querySelectorAll('button')).map(button => ({ text: button.textContent, label: button.getAttribute('aria-label') })) })`)
  writeFileSync(resolve(output, 'failure.json'), JSON.stringify({ ...diagnostic, requests }, null, 2))
  const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png' })
  writeFileSync(resolve(output, 'failure.png'), Buffer.from(screenshot.data, 'base64'))
  throw error
} finally { await close() }
