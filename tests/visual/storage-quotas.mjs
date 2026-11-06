// Real SPA rendering; all API data is disposable, intercepted fixture state.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture } from './admin-navigation-fixtures.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const { cdp, close } = await launchBrowser().catch(async error => { await server.close(); throw error })
const output = resolve(repositoryRoot, 'visual-audit-output/storage-quotas')
mkdirSync(output, { recursive: true })
let enforced = false, required = false
const errors = [], reports = []
const evaluate = async expression => {
  const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const wait = async expression => {
  for (let i = 0; i < 120; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText')}`)
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: server.origin + '/api/*' }, { urlPattern: server.origin + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    try {
      const path = new URL(request.url).pathname
      const result = fixture(path, request.method)
      if (path === '/api/admin/config') {
        result.body = { ...result.body, containerProvider: { type: 'Docker', portMappingType: 'PlatformProxy', storageQuotaEnforced: enforced, storageQuotaRequired: required } }
      } else if (path === '/api/admin/workers') {
        result.status = 200
        result.body = [{ id: '11111111-1111-4111-8111-111111111112', name: 'Quota test worker', administrativeState: 'Enabled', platformOs: 'linux', architecture: 'amd64', runtimeKind: 'docker', runtimeVersion: 'test', online: true, capacity: { cpuMillis: 2000, memoryBytes: 2147483648, slots: 10 }, capabilities: { storageQuotaEnforced: false } }]
      }
      await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: result.status || 200,
        responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(result.body)).toString('base64') })
    } catch (error) { errors.push(String(error)) }
  })
  for (const [name, width, height, language, scheme] of [
    ['desktop', 1440, 1000, 'en-US', 'dark'], ['tablet', 768, 1024, 'en-US', 'dark'],
    ['mobile', 390, 844, 'en-US', 'dark'], ['compact', 320, 568, 'en-US', 'dark'],
    ['indonesian-light', 390, 844, 'id-ID', 'light'],
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if(location.origin===${JSON.stringify(server.origin)}){localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});for(const id of ['guest','11111111-1111-4111-8111-111111111111'])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));}` })
    const copy = JSON.parse(readFileSync(resolve(repositoryRoot, `web/src/locales/${language}/admin.json`))).content.games.challenges.storage_limit
    for (const [state, quota, strict, expected] of [
      ['fallback', false, false, copy.unbounded_warning], ['enforced', true, false, copy.enforced],
      ['unknown', null, false, copy.unknown_warning], ['strict', false, true, copy.strict_warning],
      ['worker', false, false, copy.unbounded_title],
    ]) {
      enforced = quota; required = strict
      await cdp.send('Page.navigate', { url: 'about:blank' })
      await cdp.send('Page.navigate', { url: server.origin + (state === 'worker' ? '/admin/workers' : '/admin/settings?section=container') })
      await wait(`document.body && document.body.innerText.toLowerCase().includes(${JSON.stringify(expected.toLowerCase())})`)
      if (state === 'enforced') assert.equal(await evaluate(`document.body.innerText.includes(${JSON.stringify(copy.unbounded_warning)})`), false)
      await evaluate('Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))')
      await evaluate(readFileSync(resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'), 'utf8'))
      const audit = await evaluate(`(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1, violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`)
      const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
      writeFileSync(`${output}/${name}-${state}.png`, Buffer.from(screenshot.data, 'base64'))
      reports.push({ name, state, ...audit })
      assert.equal(audit.overflow, false, `${name}/${state}`)
      assert.deepEqual(audit.violations, [], `${name}/${state}`)
    }
  }
  assert.deepEqual(errors, [])
  console.log(`Storage quota UI: ${reports.length} scenarios, no overflow, Axe violations or runtime errors.`)
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, errors }, null, 2))
  await close()
  await server.close()
}
