// All API calls are browser-intercepted fixtures, including on the deployed preview.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture, uid } from './admin-operations-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_ADMIN_OPS_TARGET || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://intechfest.1pc.tf'].includes(target))
const output = resolve(process.env.RSCTF_ADMIN_OPS_OUTPUT || '../visual-audit-output/admin-ops-local')
const before = process.argv.includes('--before')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
let scenario = 'normal',
  role = 'Admin'
const reports = [],
  errors = [],
  unknown = [],
  writes = [],
  reads = []
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const wait = async (expression) => {
  for (let n = 0; n < 100; n++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('Timed out: ' + expression + '; ' + (await evaluate('document.body.innerText.slice(-1200)')))
}
const click = async (label) => {
  const query = `Array.from(document.querySelectorAll('button')).find(b=>b.textContent.trim()===${JSON.stringify(label)} && b.getBoundingClientRect().height>0)`
  assert.equal(await evaluate(`Boolean(${query})`), true, label)
  await evaluate(`${query}.focus();${query}.click()`)
}
const key = async (key, code) => {
  for (const type of ['keyDown', 'keyUp'])
    await cdp.send('Input.dispatchKeyEvent', {
      type,
      key,
      windowsVirtualKeyCode: code,
      ...(type === 'keyDown' && key === 'Enter' ? { text: '\r' } : {}),
    })
}
const fill = async (selector, text) => {
  await evaluate(
    `document.querySelector(${JSON.stringify(selector)}).focus();document.querySelector(${JSON.stringify(selector)}).select()`
  )
  await cdp.send('Input.insertText', { text })
}
const navigate = (page, suffix) => cdp.send('Page.navigate', { url: `${target}/admin/${page}?ops-check=${suffix}` })
const audit = async (name) => {
  await evaluate(
    'Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))'
  )
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const result = await evaluate(
    `(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,headings:document.querySelectorAll('h1').length,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`
  )
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  reports.push({ name, ...result })
  console.log(name, JSON.stringify(result))
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) =>
    errors.push(exceptionDetails.exception?.description || exceptionDetails.text)
  )
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const url = new URL(request.url)
    if (!['GET', 'HEAD'].includes(request.method)) writes.push({ path: url.pathname, method: request.method })
    else reads.push(url.pathname + url.search)
    const result = fixture(url.pathname + url.search, request.method, scenario, role)
    if (result.unknown) unknown.push(result.unknown)
    if (result.hold) return
    await cdp.send('Fetch.fulfillRequest', {
      requestId,
      responseCode: result.status || 200,
      responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
      body: Buffer.from(JSON.stringify(result.body)).toString('base64'),
    })
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `for(const id of ['guest',${JSON.stringify(uid)}])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));`,
  })
  for (const [name, width, height, language, scheme] of [
    ['desktop', 1440, 1100, 'en-US', 'dark'],
    ['wide', 1920, 1080, 'en-US', 'dark'],
    ['tablet', 768, 1024, 'en-US', 'dark'],
    ['mobile', 390, 844, 'en-US', 'dark'],
    ['compact', 320, 568, 'en-US', 'dark'],
    ['light-id', 390, 844, 'id-ID', 'light'],
    ['light-desktop', 1440, 1100, 'en-US', 'light'],
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-reduced-motion', value: name === 'light-id' ? 'reduce' : 'no-preference' }],
    })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});`,
    })
    for (const page of ['repo-bindings', 'builds']) {
      await navigate(page, name)
      await wait(
        page === 'builds'
          ? `document.body.innerText.includes('Tower of Babel')`
          : `document.body.innerText.includes('example/intechfest-2026')`
      )
      await audit(name + '-' + page)
    }
  }
  if (!before) {
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');`,
    })
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: 390,
      height: 844,
      deviceScaleFactor: 1,
      mobile: false,
    })
    await navigate('repo-bindings', 'dialogs')
    await wait(`document.querySelector('[data-repo-binding]')`)
    assert.equal(await evaluate(`!!document.querySelector('input[type=password]')`), false)
    await evaluate(
      `Array.from(document.querySelectorAll('[data-repo-workspace] button')).find(b=>!b.textContent.includes('Refresh')).focus()`
    )
    // Discover the add action by its dialog-opening position, independent of existing locale wording.
    await evaluate(`document.querySelector('[data-repo-workspace]').querySelectorAll('button')[1].click()`)
    await wait(`document.querySelector('[role=dialog] input[type=password]')`)
    await audit('mobile-add-repository')
    await fill('[role=dialog] input[type=password]', 'fixture-only-token')
    await key('Escape', 27)
    await wait(`!document.querySelector('[role=dialog]')`)
    await evaluate(`document.querySelector('[data-repo-workspace]').querySelectorAll('button')[1].click()`)
    await wait(`document.querySelector('[role=dialog] input[type=password]')`)
    assert.equal(await evaluate(`document.querySelector('[role=dialog] input[type=password]').value`), '')
    await key('Escape', 27)
    await wait(`!document.querySelector('[role=dialog]')`)
    await evaluate(`document.querySelector('[data-repo-binding] button:nth-of-type(2)')?.click()`)
    await wait(`document.querySelector('[role=dialog]')`)
    await audit('mobile-repo-history')
    await key('Escape', 27)
    await wait(`!document.querySelector('[role=dialog]')`)
    await evaluate(`document.querySelector('nav button[aria-label="Next page"]').click()`)
    await wait(`document.querySelector('[data-repo-binding="21"]')`)
    assert.equal(await evaluate(`document.querySelectorAll('[data-repo-binding]').length`), 2)
    await navigate('builds', 'filter')
    await wait(`document.querySelector('[data-build-card]')`)
    await click('Clear failed')
    await wait(`document.querySelector('[role=dialog]')`)
    assert.equal(
      await evaluate(`document.querySelector('[role=dialog]').innerText.includes('older records outside this view')`),
      true
    )
    await audit('mobile-prune-confirmation')
    await key('Escape', 27)
    await wait(`!document.querySelector('[role=dialog]')`)
    const countBefore = reads.filter((path) => path.startsWith('/api/admin/builds?')).length
    await fill('[data-build-workspace] input', 'tower')
    await wait(`document.querySelectorAll('[data-build-card]').length===1`)
    assert.equal(reads.filter((path) => path.startsWith('/api/admin/builds?')).length, countBefore)
    await audit('mobile-build-search')
    await evaluate(`document.querySelector('[data-build-card] button').click()`)
    await wait(`document.querySelector('[role=dialog]')`)
    await audit('mobile-error-without-log')
    assert.equal(
      await evaluate(`document.querySelector('[role=dialog]').innerText.includes('dependency resolution unavailable')`),
      true
    )
    await key('Escape', 27)
    await wait(`!document.querySelector('[role=dialog]')`)
    await fill('[data-build-workspace] input', 'no-match-unique')
    await wait(`document.querySelectorAll('[data-build-card]').length===0`)
    await click('Clear filter')
    await wait(`document.querySelectorAll('[data-build-card]').length===25`)
    await evaluate(`document.querySelectorAll('[data-build-workspace] button[aria-pressed]')[2].focus()`)
    await key('Enter', 13)
    await wait(`document.querySelectorAll('[data-build-card]').length===12`)
    await audit('mobile-failed-group')
    await click('Images on disk')
    await wait(`document.body.innerText.includes('135')`)
    await audit('mobile-images')
    for (const state of ['loading', 'error', 'empty']) {
      scenario = state
      for (const page of ['repo-bindings', 'builds']) {
        await navigate(page, state)
        await wait(
          state === 'loading'
            ? `document.querySelector('[data-repo-loading], [data-build-loading]')`
            : state === 'error'
              ? `document.querySelector('[role=alert]')`
              : page === 'builds'
                ? `document.body.innerText.includes('No build history yet')`
                : `document.body.innerText.includes('No repository bindings yet')`
        )
        await audit(state + '-' + page)
        if (state === 'error') {
          scenario = 'normal'
          await evaluate(
            `Array.from(document.querySelectorAll('[role=alert] button')).forEach(button => button.click())`
          )
          await wait(
            page === 'repo-bindings'
              ? `document.querySelector('[data-repo-binding]')`
              : `document.querySelector('[data-build-card]')`
          )
          scenario = state
        }
      }
    }
    scenario = 'images-error'
    await navigate('builds', 'images-error')
    await wait(`document.querySelector('[data-build-card]')`)
    await click('Images on disk')
    await wait(`document.querySelectorAll('[role=alert]').length===2`)
    await audit('images-error')
    scenario = 'active'
    for (const page of ['repo-bindings', 'builds']) {
      await navigate(page, 'active')
      await wait(
        page === 'builds'
          ? `document.body.innerText.includes('fixture-active-build-')`
          : `document.body.innerText.includes('Importing challenge manifests')`
      )
      await audit('mobile-active-' + page)
    }
    role = 'User'
    scenario = 'normal'
    await navigate('builds', 'non-admin')
    await wait(`document.body.innerText.includes('Page does not exist')`)
    assert.equal(await evaluate(`!!document.querySelector('[data-build-workspace]')`), false)
    assert.deepEqual(
      writes,
      [],
      'No scans, builds, repository writes, or deletions are triggered by inspection/cancelled dialogs'
    )
    assert.deepEqual(unknown, [])
    assert.deepEqual(errors, [])
    assert.deepEqual(
      reports.filter((r) => r.overflow || r.headings !== 1 || r.violations.length),
      []
    )
  }
} finally {
  writeFileSync(
    output + '/report.json',
    JSON.stringify({ target, fixtures: true, reports, errors, unknown, writes }, null, 2)
  )
  await close()
}
