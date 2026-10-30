// Actual SPA, isolated invented data. No API request reaches a deployment.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fixture as common, player } from './full-layout-fixtures.mjs'
import { launchBrowser, isRetiredInterception } from './cdp.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const target = server.origin
const output = resolve(repositoryRoot, 'visual-audit-output/easy-start')
const promo = process.env.RSCTF_CAPTURE_PROMO === '1' ? resolve(repositoryRoot, 'docs/public/screenshots') : null
mkdirSync(output, { recursive: true })
if (promo) mkdirSync(promo, { recursive: true })
const { cdp, close } = await launchBrowser()
const writes = [], errors = [], reports = []
const demoChallenges = player.challenges.slice(0, 8).map((challenge, i) => ({ ...challenge, title: ['Secret Door', 'Cookie Jar', 'Cipher Garden', 'Robot Riddle', 'Signal Lost', 'Treasure Map', 'Hidden Key', 'Rainbow Code'][i] }))
const demoTeams = ['Pixel Pirates','Cyber Cats','Puzzle Patrol','Code Comets'].map((name,i) => {
  const solvedChallenges = demoChallenges.slice(0,5-i).map((challenge,j)=>({id:challenge.id,type:'Normal',score:challenge.score,time:player.now-(5-j)*300_000+i*45_000}))
  return { ...player.rank, id:i+7, rank:i+1, name, score:solvedChallenges.reduce((sum,c)=>sum+c.score,0), solvedCount:solvedChallenges.length, solvedChallenges }
})
for (const challenge of demoChallenges) challenge.solved = demoTeams.filter(team=>team.solvedChallenges.some(solve=>solve.id===challenge.id)).length
let rejectFirstChallenge = true
let denied = false
let game = { ...common('/api/edit/games/19').body, id: 19, title: 'My first game', hidden: true, sourceRevision: 1 }
let challenge = { ...common('/api/edit/games/19/challenges/74').body, id: 74, title: 'Find your first flag', content: 'Welcome! Copy this answer: `flag{hello_world}`', flags: [{ id: 1, flag: 'flag{hello_world}' }], originalScore: 100, isEnabled: false }
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 120; i++) {
    if (await evaluate(`(() => { try { return Boolean(${expression}) } catch { return false } })()`)) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('location.href + ": " + document.body.innerText.slice(-1800)')}; errors: ${JSON.stringify(errors)}`)
}
const visit = async (path, ready) => {
  await cdp.send('Page.navigate', { url: 'about:blank' })
  await cdp.send('Page.navigate', { url: target + path })
  await wait(ready)
  await wait(`!document.querySelector('[data-motion="page"][style*="visibility: hidden"]')`)
}
const click = async (label, scope = 'document') => {
  await evaluate(`(() => { const button = [...${scope}.querySelectorAll('button')].find(b => b.textContent.trim() === ${JSON.stringify(label)}); if (!button) throw new Error('Missing button: '+${JSON.stringify(label)}); button.focus(); button.click(); })()`)
}
const fill = async (selector, text) => {
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).focus()`)
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', modifiers: 2, windowsVirtualKeyCode: 65 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'a', modifiers: 2, windowsVirtualKeyCode: 65 })
  await cdp.send('Input.insertText', { text })
}
const audit = async (name, scope = 'document', screenshotName = null) => {
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 5, y: 5 })
  await wait(`!document.querySelector('[data-event-tabs]') || [...document.querySelectorAll('[data-event-tabs] a')].every(a => a.innerText.trim().length > 0)`)
  await evaluate('Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))')
  await evaluate(readFileSync(resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'), 'utf8'))
  const result = await evaluate(`(async () => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1, violations: (await axe.run(${scope},{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,html:n.html,summary:n.failureSummary}))})) }))()`)
  reports.push({ name, ...result })
  const screenshot = await cdp.send('Page.captureScreenshot', { format: 'webp', quality: 85, captureBeyondViewport: false })
  writeFileSync(resolve(output, name + '.webp'), Buffer.from(screenshot.data, 'base64'))
  if (promo && screenshotName) writeFileSync(resolve(promo, screenshotName + '.webp'), Buffer.from(screenshot.data, 'base64'))
  assert.equal(result.overflow, false, name)
  assert.deepEqual(result.violations, [], name)
}
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (location.origin === ${JSON.stringify(target)}) { localStorage.setItem('language',JSON.stringify('en-US')); localStorage.setItem('mantine-color-scheme-value','dark'); for(const id of ['guest','11111111-1111-4111-8111-111111111111']) localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true})); }` })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    try {
      const path = new URL(request.url).pathname.toLowerCase()
      let result
      if (path.startsWith('/api/edit/') && denied) result = { status: 403, body: { title: 'Access denied', status: 403 } }
      else if (path === '/api/edit/games' && request.method === 'POST') {
        const body = JSON.parse(request.postData); writes.push({ path, body })
        game = { ...game, ...body }; result = { body: game }
      } else if (path === '/api/edit/games/19/challenges' && request.method === 'POST') {
        const body = JSON.parse(request.postData); writes.push({ path, body })
        challenge = { ...challenge, ...body, isEnabled: false, flags: [{ id: 1, flag: body.initialFlag }] }
        result = rejectFirstChallenge ? { status: 503, body: { title: 'Try again', status: 503 } } : { body: challenge }
        rejectFirstChallenge = false
      } else if (path === '/api/edit/games/19/challenges/74' && request.method === 'PUT') {
        const body = JSON.parse(request.postData); writes.push({ path, body })
        challenge = { ...challenge, ...body, revision: challenge.revision + 1 }; result = { body: challenge }
      } else if (path === '/api/edit/games/19/challenges/74') result = { body: challenge }
      else if (path === '/api/edit/games/19/challenges') result = { body: [challenge] }
      else if (path === '/api/edit/games/19') result = { body: game }
      else if (path === '/api/game/901') result = { body: { ...player.game, title: 'Flag Quest', teamCount: 4, userCount: 12 } }
      else if (path === '/api/game/901/details/participant') result = { body: { rank: demoTeams[0] } }
      else if (path === '/api/game/901/notices') result = { body: [{ id: 1, type: 'Normal', time: player.now-30_000, publishTimeUtc: player.now-30_000, values: ['Welcome to Flag Quest. Have fun!'] }] }
      else if (path === '/api/game/901/details') {
        result = { body: { ...player.responses[path], rank: demoTeams[0], challenges: Object.groupBy(demoChallenges, c=>c.category), challengeCount: demoChallenges.length } }
      } else if (path.startsWith('/api/game/901/scoreboard')) {
        const base = common('/api/game/901/scoreboard').body
        result = { body: { ...base, challenges: Object.groupBy(demoChallenges, c=>c.category), total: 4, items: demoTeams, timelines: [{ divisionId: 0, teams: demoTeams.map(team=>({id:team.id,name:team.name,items:[{time:player.game.start,score:0}, ...team.solvedChallenges.map((solve,i)=>({time:solve.time,score:team.solvedChallenges.slice(0,i+1).reduce((sum,c)=>sum+c.score,0)}))]})) }] } }
      } else result = common(request.url, request.method)
      await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: result.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(result.body ?? {})).toString('base64') })
    } catch (error) { if (!isRetiredInterception(error)) errors.push(String(error)) }
  })
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1050, deviceScaleFactor: 1, mobile: false })
  await visit('/admin/games', `document.body.innerText.includes('Create sample game')`)
  await click('Create sample game')
  await wait(`document.body.innerText.includes('Try again')`)
  await click('Create sample game')
  await wait(`location.pathname === '/admin/games/19/setup' && document.body.innerText.includes('Find your first flag')`)
  assert.equal(writes.filter(w=>w.path==='/api/edit/games').length, 1)
  const attempts = writes.filter(w=>w.path==='/api/edit/games/19/challenges')
  assert.equal(attempts.length, 2)
  assert.equal(attempts[0].body.operationId, attempts[1].body.operationId)
  // Close any transient error notification before capturing the actual setup.
  await evaluate(`document.querySelectorAll('.mantine-Notification-closeButton').forEach(b=>b.click())`)
  await audit('desktop-setup', 'document', 'getting-started')
  await click('Try answer')
  await wait(`document.querySelector('[role="dialog"] input')`)
  await fill('[role="dialog"] input', 'wrong')
  await click('Try answer', `document.querySelector('[role="dialog"]')`)
  await wait(`document.querySelector('[role="dialog"]').innerText.includes('does not match')`)
  assert.equal(await evaluate(`[...document.querySelectorAll('[role="dialog"] button')].find(b=>b.textContent==='Enable challenge').disabled`), true)
  await fill('[role="dialog"] input', 'flag{hello_world}')
  await click('Try answer', `document.querySelector('[role="dialog"]')`)
  await wait(`document.querySelector('[role="dialog"]').innerText.includes('Correct!')`)
  await audit('answer-test', `document.querySelector('[role="dialog"]')`)
  await click('Enable challenge')
  await wait(`!document.querySelector('[role="dialog"]')`)
  assert.equal(writes.at(-1).body.expectedRevision, 1)
  assert.equal(writes.at(-1).body.isEnabled, true)
  for (const width of [1440, 768, 390, 320]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height: width < 500 ? 844 : 1050, deviceScaleFactor: 1, mobile: false })
    await visit('/admin/games/19/setup', `document.body.innerText.includes('Find your first flag')`)
    await audit(`${width}-setup`)
    await evaluate(`document.querySelector('main button').focus()`)
    await click('New Challenge')
    await wait(`document.querySelector('[role="dialog"] textarea')`)
    assert.equal(await evaluate(`document.querySelectorAll('[role="dialog"] .mantine-Select-root').length`), 0)
    await fill('[role="dialog"] input[type="text"]', 'The secret word')
    await fill('[role="dialog"] textarea', 'I have keys but no locks. What am I?')
    await fill('[role="dialog"] input[autocomplete="off"]', 'flag{piano}')
    await audit(`${width}-simple-form`, `document.querySelector('[role="dialog"]')`, width===1440 ? 'create-challenge' : null)
    if (width === 320) {
      await click('New Challenge', `document.querySelector('[role="dialog"]')`)
      await wait(`document.body.innerText.includes('The secret word') && !document.querySelector('[role="dialog"]')`)
      assert.equal(writes.at(-1).body.initialFlag, 'flag{piano}')
      assert.equal(writes.at(-1).body.content, 'I have keys but no locks. What am I?')
      assert.equal(writes.at(-1).body.originalScore, 100)
    } else {
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', windowsVirtualKeyCode: 27 })
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', windowsVirtualKeyCode: 27 })
    }
    await wait(`!document.querySelector('[role="dialog"]')`)
  }
  denied = true
  await visit('/admin/games/19/setup', `document.querySelector('[data-request-error]')`)
  assert.equal(await evaluate(`document.body.innerText.includes('Find your first flag')`), false)
  assert.equal(await evaluate(`document.body.innerText.includes('The secret word')`), false)
  denied = false
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 960, deviceScaleFactor: 1, mobile: false })
  await visit('/games/901/challenges', `document.querySelectorAll('[data-guide="challenge-card"]').length === 8`)
  await wait(`getComputedStyle(document.querySelector('[data-guide="challenge-card"] dd')).visibility === 'visible'`)
  await audit('play-challenges', 'document', 'play')
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1250, deviceScaleFactor: 1, mobile: false })
  await visit('/games/901/scoreboard', `document.querySelector('table tbody tr')`)
  await wait(`getComputedStyle(document.querySelector('[data-team-name] p')).visibility === 'visible'`)
  await audit('scoreboard', 'document', 'scoreboard')
  assert.deepEqual(errors, [])
  writeFileSync(resolve(output, 'report.json'), JSON.stringify({ reports, writes: writes.map(w=>({path:w.path,operationId:w.body.operationId})), errors }, null, 2))
  console.log(JSON.stringify({ views: reports.length, writes: writes.length, errors }))
} finally { await close(); await server.close() }
