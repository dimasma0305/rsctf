import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { arenaBrowserSetup, createArenaFixture } from './attack-arena-fixtures.mjs'

const target = process.env.RSCTF_ARENA_PREVIEW || 'http://127.0.0.1:18080'
assert.ok(target === 'https://tcp.1pc.tf' || new URL(target).hostname === '127.0.0.1')
const output = resolve(process.env.RSCTF_ARENA_OUTPUT || 'visual-audit-output/attack-arena')
mkdirSync(output, { recursive: true })
const browser = await launchBrowser(), { cdp } = browser
const reports = [], errors = [], requests = []
let data = createArenaFixture(), unavailable = false
const root = `document.querySelector('[data-arena-theme]')?.shadowRoot`
const q = selector => `${root}?.querySelector(${JSON.stringify(selector)})`
const evaluate = async expression => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const waitFor = async expression => {
  for (let i = 0; i < 120; i++) { if (await evaluate(`Boolean(${expression})`)) return; await new Promise(r => setTimeout(r, 150)) }
  throw new Error(`Timed out: ${expression}`)
}
const press = async (key, code) => {
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, windowsVirtualKeyCode: code, ...(type === 'keyDown' && key === 'Enter' ? { text: '\r' } : {}) })
}
const inspect = async name => {
  await evaluate('document.fonts.ready')
  await evaluate(readFileSync('web/node_modules/axe-core/axe.min.js', 'utf8'))
  const result = await evaluate(`(async () => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1 || ${root}.host.scrollWidth > innerWidth + 1, violations: (await axe.run(document)).violations.map(v => ({ id:v.id, nodes:v.nodes.map(n => ({target:n.target,summary:n.failureSummary})) })) }))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  reports.push({ name, ...result }); console.log(name, JSON.stringify(result))
}
const navigate = async () => {
  await cdp.send('Page.navigate', { url: `${target}/games/901/attack` })
  await waitFor(`${q('#connectionStatus')}?.textContent === 'Connected'`)
  await waitFor(`${root}.querySelectorAll('#ranklist .rk').length === ${data.names.length}`)
}
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: arenaBrowserSetup })
  await cdp.send('Fetch.enable', { patterns: [{urlPattern:`${target}/api/*`},{urlPattern:`${target}/hub*`}] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    requests.push({ url: request.url, method: request.method })
    const result = unavailable && request.url.includes('/ad/scoreboard') ? { status: 503, body: {} } : data.fixture(request.url, request.method)
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: result.status, responseHeaders: [{name:'Content-Type',value:'application/json'}], body:Buffer.from(JSON.stringify(result.body)).toString('base64') })
  })
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1600, height: 1100, deviceScaleFactor: 1, mobile: false })
  await navigate()
  assert.equal(await evaluate(`${q('[data-rm="jeopardy"]')}.getAttribute('aria-pressed')`), 'true')
  assert.equal(await evaluate(`${q('#globeSurface')}.width`), 1200)
  assert.equal(await evaluate(`${q('#soundBtn')}.getAttribute('aria-pressed')`), 'false')
  assert.equal(await evaluate('arenaSockets.filter(s => s.readyState === 1).length'), 1)
  await inspect('desktop-49-teams')
  await evaluate(`${q('#fsBtn')}.focus()`); await press('Enter', 13)
  await waitFor(`${root}.fullscreenElement === ${q('.arena-wrap')}`)
  await waitFor(`${q('#fsBtn')}.getAttribute('aria-label') === 'Exit fullscreen globe'`)
  await inspect('fullscreen-globe')
  await evaluate(`${q('#fsBtn')}.focus()`); await press('Enter', 13)
  await waitFor(`!document.fullscreenElement`)
  await evaluate(`${q('#ranklist .rk')}.focus()`); await press('Enter', 13)
  assert.equal(await evaluate(`${q('#selectionName')}.textContent`), data.names[0])
  assert.equal(await evaluate(`${root}.querySelectorAll('.team-marker.selected').length`), 1)
  await inspect('desktop-selected')
  await evaluate(`arenaSockets.find(s => s.readyState === 1).close()`)
  await waitFor(`${q('#connectionStatus')}.textContent === 'Reconnecting'`)
  await waitFor(`${q('#connectionStatus')}.textContent === 'Connected'`)
  assert.equal(await evaluate('arenaSockets.filter(s => s.readyState === 1).length'), 1)
  await evaluate(`${q('#motionBtn')}.click()`)
  assert.equal(await evaluate(`${root}.host.dataset.motion`), 'off')
  await evaluate(`arenaSockets.find(s=>s.readyState===1).frame({kind:'attack',teamName:${JSON.stringify(data.names[0])},challengeTitle:'Web challenge 1',type:'FirstBlood',teamScore:8500})`)
  await waitFor(`${q('#log')}.textContent.includes('FIRST BLOOD')`)
  await waitFor(`${q('#selectionScore')}.textContent.includes('8500')`)
  assert.equal(await evaluate(`${root}.activeElement?.classList.contains('rk')`), true, 'score changes preserve focused ranking control')
  await evaluate(`${q('#jeop .chhit')}.focus()`); await press('Enter', 13)
  await waitFor(`${q('#territoryDetail')}.textContent.includes('Web challenge 1')`)
  assert.equal(await evaluate(`${q('#territoryDetail')}.textContent.includes(${JSON.stringify(data.names[0])})`), true)
  assert.equal(await evaluate(`${root}.querySelectorAll('.island').length`), 12)
  assert.equal(await evaluate(`document.querySelectorAll('h1').length`), 1)
  const beforeYaw = await evaluate(`${q('#arena')}.dataset.globeYaw`)
  await evaluate(`${q('#arena')}.focus()`); await press('ArrowRight', 39)
  assert.notEqual(await evaluate(`${q('#arena')}.dataset.globeYaw`), beforeYaw)
  const beforePitch = await evaluate(`${q('#arena')}.dataset.globePitch`)
  await press('ArrowUp', 38)
  assert.notEqual(await evaluate(`${q('#arena')}.dataset.globePitch`), beforePitch)
  await inspect('conquered-island-rotated')
  assert.equal(await evaluate('location.hash'), '#challenge=1000')
  const hiddenId = await evaluate(`${q('.island-pin[hidden]')}.dataset.challengeId`)
  await evaluate(`${q('#jeop')}?.querySelector('[data-challenge-id="${hiddenId}"]').click()`)
  assert.equal(await evaluate(`${q('#globePins')}?.querySelector('[data-challenge-id="${hiddenId}"]').hidden`), false, 'directory brings a far-side island to the front')
  await cdp.send('Page.reload')
  await waitFor(`${q('#connectionStatus')}?.textContent === 'Connected'`)
  await waitFor(`${q('#jeop [aria-pressed="true"]')}?.dataset.challengeId === '${hiddenId}'`)
  await inspect('bookmarked-island')
  await evaluate(`${q('#arena')}.scrollIntoView({block:'center'}); ${q('#globeReset')}.click()`)
  const dragStart = await evaluate(`(() => { const r=${q('#arena')}.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2,yaw:${q('#arena')}.dataset.globeYaw}; })()`)
  await cdp.send('Input.dispatchMouseEvent', {type:'mousePressed',x:dragStart.x,y:dragStart.y,button:'left',clickCount:1})
  await cdp.send('Input.dispatchMouseEvent', {type:'mouseMoved',x:dragStart.x+70,y:dragStart.y+45,button:'left',buttons:1})
  await cdp.send('Input.dispatchMouseEvent', {type:'mouseReleased',x:dragStart.x+70,y:dragStart.y+45,button:'left',clickCount:1})
  assert.notEqual(await evaluate(`${q('#arena')}.dataset.globeYaw`), dragStart.yaw, 'pointer drag rotates the 3D world')
  await evaluate(`${q('#jeop .chhit')}.blur()`)
  for (const [width, height] of [[320,568],[390,844],[768,1024],[1024,768],[1920,1080]]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', {width,height,deviceScaleFactor:1,mobile:false})
    await new Promise(r => setTimeout(r, 250))
    await evaluate(`window.scrollTo(0, 0)`)
    await inspect(`width-${width}`)
    if (width < 400) { await evaluate(`${q('#rankingTitle')}.scrollIntoView({block:'start'})`); await inspect(`rankings-${width}`) }
    if (width === 390) {
      await evaluate(`${q('#arena')}.scrollIntoView({block:'center'}); ${q('#globeReset')}.click()`)
      const touch = await evaluate(`(() => { const r=${q('#arena')}.getBoundingClientRect(); return {x:r.left+r.width/2,y:r.top+r.height/2,yaw:${q('#arena')}.dataset.globeYaw}; })()`)
      await cdp.send('Emulation.setTouchEmulationEnabled', {enabled:true})
      await cdp.send('Input.dispatchTouchEvent', {type:'touchStart',touchPoints:[{x:touch.x,y:touch.y}]})
      await cdp.send('Input.dispatchTouchEvent', {type:'touchMove',touchPoints:[{x:touch.x+55,y:touch.y+25}]})
      await cdp.send('Input.dispatchTouchEvent', {type:'touchEnd',touchPoints:[]})
      assert.notEqual(await evaluate(`${q('#arena')}.dataset.globeYaw`), touch.yaw, 'touch drag rotates without page scrolling')
      await cdp.send('Emulation.setTouchEmulationEnabled', {enabled:false})
      await inspect('touch-rotated')
      assert.equal(await evaluate(`(() => { const stage=${q('#arena')}.getBoundingClientRect(); return [...${root}.querySelectorAll('.island-pin:not([hidden])')].every(pin=>{const r=pin.getBoundingClientRect();return r.left>=stage.left-1&&r.right<=stage.right+1;}); })()`), true, 'island labels stay within the compact globe instead of clipping at its edges')
    }
  }
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {source:`localStorage.setItem('mantine-color-scheme-value','light')`})
  await cdp.send('Emulation.setDeviceMetricsOverride', {width:1366,height:1000,deviceScaleFactor:1,mobile:false})
  await navigate()
  await inspect('light-managed-accent')
  await cdp.send('Emulation.setEmulatedMedia', {features:[{name:'prefers-reduced-motion',value:'reduce'}]})
  data = createArenaFixture('mixed', 8); await navigate()
  assert.equal(await evaluate(`${root}.host.dataset.motion`), 'off')
  assert.equal(await evaluate(`${q('#motionBtn')}.disabled`), true)
  assert.equal(await evaluate(`${q('[data-rm="ad"]')}.getAttribute('aria-pressed')`), 'true')
  await inspect('light-mixed-reduced-motion')
  await evaluate(`${q('[data-rm="koth"]')}.click()`)
  await inspect('light-koth-ranking')
  data = createArenaFixture('koth', 8); await navigate()
  assert.equal(await evaluate(`${q('[data-rm="koth"]')}.getAttribute('aria-pressed')`), 'true')
  data = createArenaFixture('jeopardy', 0)
  await cdp.send('Page.navigate', {url:`${target}/games/901/attack`})
  await waitFor(`${q('#connectionStatus')}?.textContent === 'Waiting for teams'`)
  await inspect('waiting-for-teams')
  unavailable = true
  await cdp.send('Page.navigate', {url:`${target}/games/901/attack`})
  await waitFor(`${q('#connectionStatus')}?.textContent === 'Waiting for data'`)
  await inspect('waiting-for-data')
  unavailable = false; data = createArenaFixture()
  await waitFor(`${q('#connectionStatus')}.textContent === 'Connected'`)
  await waitFor(`${root}.querySelectorAll('#ranklist .rk').length === 49`)
  await inspect('recovered-without-reload')
  await cdp.send('Page.navigate', {url:`${target}/games/901/attack?preview`})
  await waitFor(`${q('#connectionStatus')}?.textContent === 'Preview'`)
  await waitFor(`${root}.querySelectorAll('#ranklist .rk').length === 8`)
  await evaluate(`${q('#freezeBtn')}.click()`)
  await waitFor(`${q('#fzOverlay')}.classList.contains('show')`)
  await inspect('preview-frozen')
  await evaluate(`${q('#freezeBtn')}.click(); ${q('#endBtn')}.click()`)
  await waitFor(`${q('#winOverlay')}.classList.contains('show')`)
  await waitFor(`getComputedStyle(${q('.pod.p1')}).opacity === '1'`)
  await inspect('preview-ended')
  assert.equal(requests.some(r => !['GET','HEAD'].includes(r.method) && !r.url.includes('/hub/user/negotiate')), false, 'spectator controls never write to event APIs')
  assert.deepEqual(errors, [])
  assert.deepEqual(reports.filter(r => r.overflow || r.violations.length), [])
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({reports,errors,requests}, null, 2))
  await browser.close()
}
