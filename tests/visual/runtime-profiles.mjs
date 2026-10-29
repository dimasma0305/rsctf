// Real SPA rendering with intercepted local API fixtures; no deployment writes.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture } from './admin-navigation-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const target = server.origin
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_RUNTIME_UI_OUTPUT || resolve(repositoryRoot, 'visual-audit-output/runtime-profiles'))
mkdirSync(output, { recursive: true })
const profiles = [
  { id: 'docker', name: 'Docker host', backend: 'docker', isolation: 'container', enabled: true },
  { id: 'kube', name: 'Cluster', backend: 'kubernetes', isolation: 'container', enabled: true },
  { id: 'vm', name: 'Isolated VM', backend: 'kubernetes', isolation: 'microvm', runtimeClass: 'kata-qemu-runtime-rs', enabled: true },
  { id: 'retired', name: 'Retired host', backend: 'docker', isolation: 'container', enabled: false },
]
let detail = { ...fixture('/api/edit/games/19/challenges/74').body, type: 'DynamicContainer', isEnabled: true, runtimeProfile: 'docker', runtimeProfiles: profiles, containerImage: 'registry.example/challenge@sha256:' + 'a'.repeat(64), memoryLimit: 256, storageLimit: 512, cpuCount: 1, exposePort: 8080, buildStatus: 'Success', networkMode: 'Open', flagTemplate: 'flag{[GUID]}', scoreCurve: 'Standard', originalScore: 500 }
let locked = null
const now = Date.now()
const writes = [], errors = [], reports = []
const { cdp, close } = await launchBrowser().catch(async error => { await server.close(); throw error })
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 120; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-2500)')}`)
}
const visit = async (path, selector) => {
  await cdp.send('Page.navigate', { url: 'about:blank' })
  await cdp.send('Page.navigate', { url: target + path + '?runtime-check=' + Math.random() })
  await wait(`document.querySelector(${JSON.stringify(selector)})`)
}
const key = async (key, code) => {
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, windowsVirtualKeyCode: code })
}
const click = async (text, scope = 'document') => {
  await evaluate(`(() => { const button=[...${scope}.querySelectorAll('button')].find(button => button.textContent.trim() === ${JSON.stringify(text)}); button.focus(); button.click(); })()`)
}
const audit = async (name, selector) => {
  await evaluate('Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))')
  await evaluate(readFileSync(resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'), 'utf8'))
  const report = await evaluate(`(async () => ({overflow: document.documentElement.scrollWidth > innerWidth + 1, violations: (await axe.run(document.querySelector(${JSON.stringify(selector)}), {runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id:v.id, nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`)
  reports.push({ name, ...report })
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  assert.equal(report.overflow, false, name)
  assert.deepEqual(report.violations, [], name)
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    try {
      const path = new URL(request.url).pathname
      let result
      if (path === '/api/edit/games/19/challenges/74') {
        if (!['GET', 'HEAD'].includes(request.method)) {
          const body = JSON.parse(request.postData)
          writes.push(body)
          detail = { ...detail, ...body, revision: detail.revision + 1 }
          delete detail.replaceRuntimeInstances
        }
        result = { body: detail }
      } else if (path === '/api/edit/games/19/challenges') result = { body: [detail] }
      else if (path === '/api/edit/games/19') result = { body: { ...fixture(path).body, adScoringStartRound: locked === 'ad' ? 1 : null, kothScoringStartRound: locked === 'koth' ? 1 : null } }
      else if (path === '/api/game/19') result = { body: { id: 19, title: 'INTECHFEST 2026 Warmup', start: now - 3600000, end: now + 86400000, serverTime: now, practiceMode: true, limit: 0, teamCount: 10, divisions: [] } }
      else result = fixture(path, request.method)
      await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: result.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(result.body ?? {})).toString('base64') })
    } catch (error) { errors.push(String(error)) }
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (location.origin === ${JSON.stringify(target)}) { localStorage.setItem('language',JSON.stringify('en-US')); for(const id of ['guest','11111111-1111-4111-8111-111111111111']) localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true})); }` })
  for (const theme of ['dark', 'light']) for (const width of [1440, 390, 320]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 1000, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (location.origin === ${JSON.stringify(target)}) localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(theme)});` })
    await visit('/admin/games/19/challenges/74', '[data-runtime-profile-controls] input')
    await evaluate(`document.querySelector('[data-runtime-profile-controls]').scrollIntoView({block:'center'})`)
    await audit(`${theme}-${width}-editor`, '[data-runtime-profile-controls]')
    await visit('/games/19', '[data-game-status] .mantine-Badge-label')
    const spacing = await evaluate(`(() => { const badge=document.querySelector('[data-game-status]'); const dot=badge.querySelector('[aria-hidden]'); const label=badge.querySelector('.mantine-Badge-label'); return {gap:label.getBoundingClientRect().left-dot.getBoundingClientRect().right, width:dot.getBoundingClientRect().width, text:label.textContent, shadow:getComputedStyle(dot).boxShadow}; })()`)
    assert.ok(spacing.gap >= 7 && spacing.width >= 6, JSON.stringify(spacing))
    assert.equal(spacing.shadow, 'none')
    assert.match(spacing.text, /Live now/i)
    await audit(`${theme}-${width}-live-dot`, '[data-game-status]')
  }
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false })
  await visit('/admin/games/19/challenges/74', '[data-runtime-profile-controls] input')
  await evaluate(`document.querySelector('[data-runtime-profile-controls] input').focus()`)
  await key('ArrowDown', 40)
  await wait(`document.querySelector('[role="option"]')`)
  await evaluate(`[...document.querySelectorAll('[role="option"]')].find(option=>option.textContent.includes('Isolated VM')).click()`)
  assert.equal(writes.length, 0, 'selection alone must not mutate or destroy instances')
  await click('Save and replace instances')
  await wait(`document.querySelector('[role="dialog"]')`)
  await audit('replacement-confirmation', '[role="dialog"]')
  await click('Cancel', `document.querySelector('[role="dialog"]')`)
  await wait(`!document.querySelector('[role="dialog"]')`)
  await wait(`document.activeElement === document.querySelector('[data-runtime-profile-controls] button')`)
  assert.equal(writes.length, 0)
  await click('Save and replace instances')
  await wait(`document.querySelector('[role="dialog"]')`)
  await click('Save and replace instances', `document.querySelector('[role="dialog"]')`)
  await wait(`!document.querySelector('[role="dialog"]') && !document.querySelector('[data-runtime-profile-controls] input').disabled`)
  assert.equal(writes.length, 1)
  assert.equal(writes[0].runtimeProfile, 'vm')
  assert.equal(writes[0].replaceRuntimeInstances, true)
  assert.equal(writes[0].expectedRevision, 1)
  assert.equal(writes[0].runtimeProfiles, undefined, 'admin-owned catalog is never sent as an edit')
  // Ordinary save changes placement without authorizing teardown.
  await evaluate(`document.querySelector('[data-runtime-profile-controls] input').focus()`)
  await key('ArrowDown', 40)
  await wait(`document.querySelector('[role="option"]')`)
  await evaluate(`[...document.querySelectorAll('[role="option"]')].find(option=>option.textContent === 'Deployment default').click()`)
  await click('Save Changes')
  await wait(`!document.querySelector('[data-runtime-profile-controls] input').disabled`)
  assert.equal(writes.length, 2)
  assert.equal(writes[1].runtimeProfile, null)
  assert.notEqual(writes[1].replaceRuntimeInstances, true)
  for (const [type, scoring] of [['AttackDefense', 'ad'], ['KingOfTheHill', 'koth']]) {
    detail = { ...detail, type }; locked = scoring
    await visit('/admin/games/19/challenges/74', '[data-runtime-profile-controls] input')
    assert.equal(await evaluate(`document.querySelector('[data-runtime-profile-controls] input').disabled`), true)
    assert.equal(await evaluate(`document.querySelector('[data-runtime-profile-controls] button').disabled`), true)
  }
  locked = null
  detail = { ...detail, type: 'DynamicContainer', isEnabled: false, runtimeTransitionPending: true }
  await visit('/admin/games/19/challenges/74', '[data-runtime-profile-controls] input')
  assert.equal(await evaluate(`document.querySelector('[data-runtime-profile-controls] button').disabled`), false)
  assert.match(await evaluate(`document.querySelector('[data-runtime-profile-controls]').textContent`), /interrupted runtime update/)
  await evaluate(`document.querySelector('[data-runtime-profile-controls]').scrollIntoView({block:'center'})`)
  await audit('interrupted-replacement', '[data-runtime-profile-controls]')
  assert.deepEqual(errors, [])
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, writes, errors }, null, 2))
  console.log(JSON.stringify({ views: reports.length, replacementWrites: writes.length, errors }))
} finally { await close(); await server.close() }
