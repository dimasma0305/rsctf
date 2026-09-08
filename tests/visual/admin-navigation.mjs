import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture } from './admin-navigation-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_ADMIN_NAV_TARGET || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://intechfest.1pc.tf'].includes(target))
const output = resolve(process.env.RSCTF_ADMIN_NAV_OUTPUT || '../visual-audit-output/admin-navigation-local')
const screensOnly = process.argv.includes('--screens-only')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const reports = [], writes = [], unknown = [], errors = [], reads = []
let role = 'Admin'
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 120; i++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise((r) => setTimeout(r, 100))
  }
  throw new Error('Timed out: ' + expression + '; ' + await evaluate('document.body.innerText.slice(-1000)'))
}
const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).focus();document.querySelector(${JSON.stringify(selector)}).click()`)
const visit = async (path, expected) => {
  await evaluate('window.__rsctfOldNavigationDocument = true')
  await cdp.send('Page.navigate', { url: target + path })
  await wait(`!window.__rsctfOldNavigationDocument && document.querySelector('h1') && document.body.innerText.includes(${JSON.stringify(expected)})`)
}
const audit = async (name) => {
  await evaluate('Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))')
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const r = await evaluate(`(async()=>({path:location.pathname+location.search,overflow:document.documentElement.scrollWidth>innerWidth+1,headings:document.querySelectorAll('h1').length,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(output + '/' + name + '.png', Buffer.from(shot.data, 'base64'))
  reports.push({ name, ...r })
  console.log(name, JSON.stringify(r))
}
const keyboard = async (key, code) => {
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, windowsVirtualKeyCode: code, ...(type==='keyDown' && key==='Enter' ? {text:'\r'}:{}) })
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const u = new URL(request.url)
    if (!['GET', 'HEAD'].includes(request.method)) writes.push({ path: u.pathname, method: request.method })
    else reads.push(u.pathname)
    const r = fixture(u.pathname + u.search, request.method, role)
    if (r.unknown) unknown.push(r.unknown)
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: r.status || 200, responseHeaders: [{ name:'Content-Type',value:'application/json'}], body:Buffer.from(JSON.stringify(r.body)).toString('base64') })
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `for(const id of ['guest','11111111-1111-4111-8111-111111111111'])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));` })
  const pages = [
    ['/admin/games/19/adops', 'Stargazers'], ['/admin/games/19/challenges', 'Navigation challenge 74'],
    ['/admin/games/19/challenges/74', 'Navigation challenge 74'], ['/admin/games/19/challenges/74/flags', 'Navigation challenge 74'],
    ['/admin/settings?section=email', 'SMTP'], ['/admin/users', 'Import'],
    ['/admin/repo-bindings', 'example/intechfest-2026'], ['/admin/builds', 'Tower of Babel'],
  ]
  for (const [name, width, height, language, scheme] of (process.argv.includes('--interactions-only') ? [] : [
    ['desktop',1440,1100,'en-US','dark'], ['tablet',768,1024,'en-US','dark'],
    ['mobile',390,844,'en-US','dark'], ['compact',320,568,'en-US','dark'],
    ['light-id',390,844,'id-ID','light'],
  ])) {
    await cdp.send('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:false})
    await cdp.send('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:name==='light-id'?'reduce':'no-preference'}]})
    await cdp.send('Page.addScriptToEvaluateOnNewDocument',{source:`localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});`})
    for (const [path, expected] of pages) {
      await visit(path, expected)
      await audit(name + path.replaceAll('/','--').replace('?section=','-'))
    }
  }
  if (!screensOnly) {
    await cdp.send('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false})
    await cdp.send('Page.addScriptToEvaluateOnNewDocument',{source:`localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');`})
    await visit('/admin/games/19/challenges/74/flags','Jump to challenge')
    assert.equal(await evaluate(`document.querySelector('[aria-label="Breadcrumbs"] [aria-current=page]').textContent`),'Flags & files')
    assert.ok(await evaluate(`document.querySelector('a[href="/games/19"]').getBoundingClientRect().height>0`),'player preview available on mobile')
    assert.ok(await evaluate(`!!document.querySelector('nav[aria-label="Mobile navigation"] a[href="/admin/games"]')`),'admin dock retains admin destinations')
    await click('[data-admin-event-navigation] input')
    await wait(`document.querySelector('[role=option]')`)
    await evaluate(`Array.from(document.querySelectorAll('[role=option]')).find(e=>e.textContent.includes('KotH Ops')).click()`)
    await wait(`location.pathname==='/admin/games/19/adops' && document.querySelector('[data-ad-workspace]')`)
    await evaluate('history.back()')
    await wait(`location.pathname==='/admin/games/19/challenges/74/flags' && document.querySelector('[data-challenge-switcher]')`)
    await click('[data-challenge-switcher] input')
    await wait(`document.querySelector('[role=option]')`)
    await evaluate(`Array.from(document.querySelectorAll('[role=option]')).find(e=>e.textContent.includes('#75')).click()`)
    await wait(`location.pathname==='/admin/games/19/challenges/75/flags' && document.body.innerText.includes('Navigation challenge 75')`)
    await audit('challenge-switch-preserves-flags')
    await click('[data-workspace-bar] button')
    await wait(`document.activeElement.matches('[role=dialog] input')`)
    await cdp.send('Input.insertText',{text:'adops'})
    await wait(`document.querySelectorAll('[role=dialog] a').length===1 && document.querySelector('[role=dialog] a[href="/admin/games/19/adops"]')`)
    await keyboard('ArrowDown',40)
    assert.equal(await evaluate('document.activeElement.getAttribute("href")'),'/admin/games/19/adops')
    await keyboard('Enter',13)
    await wait(`location.pathname==='/admin/games/19/adops' && document.querySelector('[data-ad-workspace]') && !document.querySelector('[role=dialog]')`)
    await click('[data-workspace-bar] button')
    await wait(`document.activeElement.matches('[role=dialog] input')`)
    await cdp.send('Input.insertText',{text:'smtp'})
    await wait(`document.querySelectorAll('[role=dialog] a').length===1 && document.querySelector('[role=dialog] a[href="/admin/settings?section=email"]')`)
    await audit('contextual-settings-search')
    await evaluate(`document.querySelector('[role=dialog] input').focus()`)
    await keyboard('ArrowDown',40)
    assert.equal(await evaluate('document.activeElement.getAttribute("href")'),'/admin/settings?section=email')
    await keyboard('Enter',13)
    await wait(`location.search==='?section=email' && document.querySelector('#settings-tab-email[aria-selected=true]')`)
    await evaluate('window.__rsctfOldNavigationDocument = true')
    await cdp.send('Page.reload')
    await wait(`!window.__rsctfOldNavigationDocument && document.querySelector('#settings-tab-email[aria-selected=true]')`)
    await click('#settings-tab-platform')
    await wait(`document.querySelector('#settings-panel input[placeholder=RS]')?.value==='RSCTF'`)
    await evaluate(`document.querySelector('#settings-panel input[placeholder=RS]').focus();document.querySelector('#settings-panel input[placeholder=RS]').select()`)
    await cdp.send('Input.insertText',{text:'Unsaved fixture title'})
    await wait(`document.querySelector('#settings-panel input[placeholder=RS]')?.value==='Unsaved fixture title'`)
    const readCount = reads.filter((p)=>p==='/api/admin/config').length
    await click('#settings-tab-email')
    await wait(`location.search==='?section=email'`)
    await evaluate('history.back()')
    await wait(`document.querySelector('#settings-panel input[placeholder=RS]')?.value==='Unsaved fixture title'`)
    assert.equal(reads.filter((p)=>p==='/api/admin/config').length,readCount,'switching settings sections does not fetch again')
    await audit('settings-history-preserves-draft')
    await click('[data-workspace-bar] button')
    await wait(`document.activeElement.matches('[role=dialog] input')`)
    await keyboard('Escape',27)
    await wait(`!document.querySelector('[role=dialog]')`)
    await wait(`document.activeElement.closest('[data-workspace-bar]')`)
    role = 'Manager'
    await visit('/admin/games/19/adops','Stargazers')
    await click('[data-workspace-bar] button')
    assert.equal(await evaluate(`document.querySelectorAll('[role=dialog] a[href^="/admin/settings"]').length`),0)
    assert.equal(await evaluate(`document.querySelectorAll('[role=dialog] a[href$="/managers"]').length`),0)
    await audit('event-manager-navigation')
  }
  assert.deepEqual(unknown,[],'all API routes must be explicitly fixture-backed')
  assert.deepEqual(writes,[],'no real or mocked mutation should be attempted')
  assert.deepEqual(errors,[])
  assert.deepEqual(reports.filter((r)=>r.overflow || r.headings!==1 || r.violations.length),[])
} finally {
  writeFileSync(output+'/report.json',JSON.stringify({target,fixtures:true,reports,writes,unknown,errors},null,2))
  await close()
}
