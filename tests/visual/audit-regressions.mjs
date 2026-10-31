// Real SPA and Axe; every API response is isolated, invented fixture data.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture as common, player } from './full-layout-fixtures.mjs'
import { launchBrowser, isRetiredInterception } from './cdp.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const { cdp, close } = await launchBrowser()
const output = resolve(repositoryRoot, 'visual-audit-output/audit-regressions')
mkdirSync(output, { recursive: true })
const reports = [], errors = []
let theme = 'dark'
const component = { active: true, score: 50, projectedScore: 50, earnedPoints: 100, attainablePoints: 200 }
const combined = {
  generatedAt: Date.now(), freeze: null, isFrozenView: false, fullySettled: true,
  modes: Object.fromEntries(['jeopardy', 'attackDefense', 'koth'].map(key => [key, { active: true, challengeCount: 1, weight: 1 / 3 }])),
  divisions: [], items: [{ id: 7, name: 'Audit fixture team', avatar: null, divisionId: null, division: null,
    rank: 1, divisionRank: null, score: 50, projectedScore: 50,
    components: { jeopardy: component, attackDefense: component, koth: component } }],
}
const evaluate = async expression => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async expression => {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(`(()=>{try{return Boolean(${expression})}catch{return false}})()`)) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText')}`)
}
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `
    if (location.origin === ${JSON.stringify(server.origin)}) {
    localStorage.setItem('language', JSON.stringify('en-US'));
    localStorage.setItem('rsctf-player-guide:guest', JSON.stringify({interactiveEnabled:false, completedVersion:5, seenFeatures:[], activeTourStep:null,tourPaused:true}));
    }
  ` })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: server.origin + '/api/*' }, { urlPattern: server.origin + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    try {
      const path = new URL(request.url).pathname.toLowerCase()
      let result = common(request.url, request.method, 'Guest')
      if (path === '/api/config') result.body = { ...result.body, customTheme: '#e33f3d' }
      if (path === '/api/donations') result.body = { ...result.body,
        messages: Array.from({ length: 25 }, (_, i) => ({ supporterName: `Supporter ${i}`, message: 'Keep learning and solving puzzles! '.repeat(5), updatedAt: Date.now() })) }
      if (['/api/game/901/scoreboard', '/api/game/901/scoreboard/page', '/api/game/901/details'].includes(path)) {
        result.body = { ...result.body, challenges: { Web: [
          { ...player.challenges[0], type: 'StaticAttachment' },
          { ...player.challenges[0], id: 9901, type: 'AttackDefense' },
          { ...player.challenges[0], id: 9902, type: 'KingOfTheHill' },
        ] } }
      }
      if (path === '/api/game/901/scoreboard/combined') result = { body: combined }
      await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: result.status || 200,
        responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(result.body ?? {})).toString('base64') })
    } catch (error) { if (!isRetiredInterception(error)) errors.push(String(error)) }
  })
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
  for (theme of ['dark', 'light']) {
    await cdp.send('Page.navigate', { url: server.origin })
    await wait('document.body')
    await evaluate(`localStorage.setItem('mantine-color-scheme-value', ${JSON.stringify(theme)})`)
    for (const width of [1440, 390, 320]) {
      await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: 1000, deviceScaleFactor: 1, mobile: false })
      for (const [path, ready] of [
        ['/', 'document.querySelector("main a")'],
        ['/account/login', 'document.querySelector("input[type=password]")'],
        ['/account/register', 'document.querySelector("input[type=password]")'],
        ['/donations', 'document.body.innerText.includes("Supporter 24")'],
        ['/games/901/scoreboard#overall', 'document.body.innerText.includes("Audit fixture team")'],
      ]) {
        console.log('audit', theme, width, path)
        await cdp.send('Page.navigate', { url: 'about:blank' })
        await cdp.send('Page.navigate', { url: server.origin + path })
        await wait(ready)
        await evaluate('Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))')
        await evaluate(readFileSync(resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'), 'utf8'))
        const result = await evaluate(`(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`)
        const name = `${theme}-${width}-${path.replace(/[^a-z0-9]/gi,'_') || 'home'}`
        const shot = await cdp.send('Page.captureScreenshot', {format:'png',captureBeyondViewport:false})
        writeFileSync(resolve(output, name+'.png'),Buffer.from(shot.data,'base64'))
        if (path === '/donations') {
          const selector = '[role=region][aria-labelledby=donation-messages-title]'
          if (await evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)}))`)) {
            await evaluate(`document.querySelector(${JSON.stringify(selector)}).focus()`)
            await cdp.send('Input.dispatchKeyEvent', {type:'keyDown',key:'End',windowsVirtualKeyCode:35})
            await cdp.send('Input.dispatchKeyEvent', {type:'keyUp',key:'End',windowsVirtualKeyCode:35})
            await wait(`document.querySelector(${JSON.stringify(selector)}).scrollTop > 0`)
          } else result.keyboardFailure = true
        }
        reports.push({name,...result})
      }
    }
  }
  writeFileSync(resolve(output,'report.json'),JSON.stringify({reports,errors},null,2))
  assert.deepEqual(errors, [])
  assert.deepEqual(reports.filter(r=>r.overflow||r.violations.length||r.keyboardFailure), [])
  console.log(`Passed ${reports.length} responsive light/dark Axe views with keyboard scrolling and populated Overall scores`)
} finally { await close(); await server.close() }
