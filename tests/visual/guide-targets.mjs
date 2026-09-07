// Deliberately delayed, local-only API fixtures. Never joins events or starts services.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_GUIDE_PREVIEW || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_GUIDE_OUTPUT || '../visual-audit-output/guide-targets')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const errors = [],
  unknown = new Set(),
  reports = []
const userId = '11111111-1111-4111-8111-111111111111'
const key = `rsctf-player-guide:${userId}`
const now = Date.now()
const game = {
  id: 901,
  title: 'Delayed guide event',
  start: now - 60000,
  end: now + 3600000,
  summary: 'An event loaded after its search control.',
  teamCount: 2,
  userCount: 6,
  limit: 3,
}
const fixtures = {
  '/api/config': {
    title: 'RSCTF',
    portMapping: 'Default',
    allowRegister: true,
    allowPasswordRegistration: true,
    allowTeamCreation: true,
    emailConfirmationRequired: false,
    enableBrowserFingerprint: false,
  },
  '/api/captcha': { type: 'None' },
  '/api/account/profile': {
    userId,
    userName: 'Guide tester',
    role: 'User',
    email: 'guide@example.invalid',
  },
  '/api/game/recent': [],
}
let delayedRequest
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })
  if (result.exceptionDetails)
    throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 80; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-1200)')}`)
}
const fulfill = (requestId, value, status = 200) =>
  cdp.send('Fetch.fulfillRequest', {
    requestId,
    responseCode: status,
    responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
    body: Buffer.from(JSON.stringify(value)).toString('base64'),
  })
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) =>
    errors.push(exceptionDetails.exception?.description || exceptionDetails.text)
  )
  await cdp.send('Fetch.enable', {
    patterns: [{ urlPattern: `${target}/api/*` }, { urlPattern: `${target}/hub*` }],
  })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname.toLowerCase()
    if (!['GET', 'HEAD'].includes(request.method)) {
      errors.push(`Unexpected mutation: ${request.method} ${path}`)
      return fulfill(requestId, { title: 'Fixture mutation blocked' }, 405)
    }
    if (path === '/api/game') {
      delayedRequest = requestId
      return
    }
    if (!(path in fixtures)) unknown.add(path)
    await fulfill(requestId, fixtures[path] ?? [])
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `localStorage.setItem(${JSON.stringify(key)},JSON.stringify({interactiveEnabled:true,completedVersion:0,seenFeatures:[],activeTourStep:'events',tourPaused:false}));localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');`,
  })
  for (const [name, width, height] of [
    ['desktop', 1600, 1100],
    ['compact', 320, 568],
  ]) {
    delayedRequest = undefined
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    })
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-reduced-motion', value: 'reduce' }],
    })
    await cdp.send('Page.navigate', {
      url: `${target}/games?guide-target=${name}`,
    })
    await wait(`document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget==='games-search'`)
    assert.ok(delayedRequest, 'the catalog request is held until its fallback is highlighted')
    // Native :disabled includes inherited fieldset state, which happy-dom omits.
    await evaluate(
      `(()=>{const fieldset=document.createElement('fieldset');fieldset.id='disabled-guide-fixture';fieldset.disabled=true;fieldset.style.cssText='position:fixed;top:350px;left:20px';const button=document.createElement('button');button.dataset.guide='event-card';button.textContent='Unavailable event';fieldset.append(button);document.body.append(fieldset);})()`
    )
    await evaluate(`new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))`)
    assert.equal(
      await evaluate(`document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget`),
      'games-search'
    )
    await evaluate(`document.getElementById('disabled-guide-fixture').remove()`)
    await fulfill(delayedRequest, { data: [game], total: 1 })
    await wait(`document.querySelector('[data-guide="event-card"]')`)
    await wait(`document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget==='event-card'`)
    const alignment = `(()=>{const element=document.querySelector('[data-guide="event-card"]');const spotlight=document.querySelector('[data-guide-layer="spotlight"]');if(!element||!spotlight)return false;const card=element.getBoundingClientRect();const ring=spotlight.getBoundingClientRect();return Math.abs(ring.left-Math.max(4,card.left-8))<2 && Math.abs(ring.top-Math.max(4,card.top-8))<2 && Math.abs(ring.right-Math.min(innerWidth-4,card.right+8))<2;})()`
    await wait(alignment)
    // Cover refresh and collision checks after scroll settles, not only one frame.
    for (let sample = 0; sample < 3; sample++) {
      await new Promise((resolve) => setTimeout(resolve, 350))
      assert.equal(await evaluate(alignment), true, 'the outline stays aligned after scrolling settles')
    }
    const aligned = await evaluate(alignment)
    assert.equal(aligned, true, 'the outline follows the loaded card, not the previous search control')
    await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
    const result = await evaluate(
      `(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>n.target)}))}))()`
    )
    reports.push({ name, aligned, ...result })
    const shot = await cdp.send('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: false,
    })
    writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
    console.log(name, JSON.stringify(reports.at(-1)))
  }
  assert.deepEqual([...unknown], [])
  assert.deepEqual(errors, [])
  assert.deepEqual(
    reports.filter((report) => report.overflow || report.violations.length),
    []
  )
} catch (error) {
  console.error(
    await evaluate(
      `JSON.stringify({scrollY,targets:[...document.querySelectorAll('[data-guide="event-card"], [data-guide-layer="spotlight"], [data-guide-surface="coachmark"]')].map(e=>({guide:e.dataset,rect:e.getBoundingClientRect().toJSON(),hidden:e.closest('[aria-hidden="true"], [inert]')?.outerHTML.slice(0,250)}))})`
    )
  )
  const shot = await cdp.send('Page.captureScreenshot', {
    format: 'png',
    captureBeyondViewport: false,
  })
  writeFileSync(`${output}/failure.png`, Buffer.from(shot.data, 'base64'))
  throw error
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, unknown: [...unknown], errors }, null, 2))
  await close()
}
console.log('PASS: delayed guide targets on desktop and compact screens')
