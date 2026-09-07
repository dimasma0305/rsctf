// Browser-only fixtures: every API request is intercepted, including mutations.
import assert from 'node:assert/strict'
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_WORKSPACE_PREVIEW || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_WORKSPACE_OUTPUT || '../visual-audit-output/competition')
mkdirSync(output, { recursive: true })
const now = Date.now()
const profile = { userId: '11111111-1111-4111-8111-111111111111', role: 'User', userName: 'Dimas', email: 'player@example.invalid' }
const game = { id: 901, title: 'Intechfest 2026', start: now - 3600000, end: now + 8000000, serverTime: now, status: 'Accepted', joined: true, teamCount: 50, userCount: 150, practiceMode: false, divisions: [], writeupRequired: false }
const names = ['Ret2win', 'Cookie Jar', 'Cipher Garden', 'Minions in 32K', 'Signal Lost']
const challenges = Array.from({ length: 100 }, (_, i) => ({ id: 9001 + i, title: i < 5 ? names[i] : `Challenge ${String(i + 1).padStart(3, '0')}`, category: ['Pwn', 'Web', 'Crypto', 'Reverse', 'Misc'][i % 5], type: 'StaticAttachment', score: 500 - (i % 5) * 50, solved: i % 13, bloods: [], disableBloodBonus: true }))
const rank = { id: 7, name: 'TCP1P', rank: 7, score: 3250, solvedCount: 12, lastSubmissionTime: now - 30000, solvedChallenges: challenges.slice(1, 13).map((challenge) => ({ id: challenge.id, type: 'Normal', score: challenge.score, time: now - 30000 })) }
const config = { title: 'RSCTF', slogan: 'Capture the flag', portMapping: 'Default', allowRegister: true, allowPasswordRegistration: true, allowTeamCreation: true, emailConfirmationRequired: false, enableBrowserFingerprint: false }
const responses = {
  '/api/account/profile': profile, '/api/config': config, '/api/captcha': { type: 'None' },
  '/api/game/901': game,
  '/api/game/902': { ...game, id: 902 },
  '/api/game/902/notices': [],
  '/api/game/901/details': { challenges: Object.groupBy(challenges, (item) => item.category), challengeCount: 100, rank, teamToken: 'fixture-only-not-a-credential' },
  '/api/game/901/details/participant': { rank },
  '/api/game/901/notices': [{ id: 1, type: 'FirstBlood', time: now - 30000, publishTimeUtc: now - 30000, values: ['TCP1P', 'Cipher Garden'] }],
}
for (const challenge of challenges) {
  responses[`/api/game/901/challenges/${challenge.id}`] = { ...challenge, content: 'Download the challenge files and submit your flag.', context: { url: `/assets/${'a'.repeat(64)}/ret2win.zip`, fileSize: 2048 }, attempts: 0, hints: [] }
  responses[`/api/game/901/challenges/${challenge.id}/solvers/page`] = { data: [], total: challenge.solved }
}
const browser = await launchBrowser()
const { cdp } = browser
const reports = [], unknown = new Set(), requests = []
let errors = [], forbidden = false
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const waitFor = async (expression) => {
  for (let i = 0; i < 160; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-2500)')}`)
}
const press = async (key) => {
  const vk = { Enter: 13, ' ': 32, Escape: 27, Tab: 9 }[key]
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, windowsVirtualKeyCode: vk, ...(type === 'keyDown' && key === 'Enter' ? { text: '\r', unmodifiedText: '\r' } : {}) })
}
const screenshot = async (name) => {
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
}
const inspect = async (name) => {
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const issues = await evaluate(`(async () => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1, violations: (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa'] } })).violations.map(v => ({ id: v.id, nodes: v.nodes.map(n => ({ target: n.target, summary: n.failureSummary })) })) }))()`)
  await screenshot(name)
  reports.push({ name, ...issues, errors: [...errors] })
  console.log(name, JSON.stringify(issues))
}
const selectView = async (view) => {
  await evaluate(`document.querySelector('input[value="${view}"]').click()`)
  await waitFor(view === 'globe' ? `document.querySelector('[data-challenge-globe]')` : view === 'list' ? `document.querySelector('[data-challenge-list]')` : `document.querySelector('[data-guide="challenge-card"]')`)
}
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description ?? exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (location.origin === ${JSON.stringify(new URL(target).origin)}) { localStorage.setItem('language', JSON.stringify('en-US')); localStorage.setItem('mantine-color-scheme-value', 'dark'); localStorage.setItem('rsctf-player-guide:${profile.userId}', JSON.stringify({ interactiveEnabled:false, completedVersion:1, seenFeatures:[] })); }` })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: `${target}/api/*` }, { urlPattern: `${target}/hub*` }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname.toLowerCase()
    requests.push({ path, method: request.method })
    let value = responses[path], status = 200
    if (!['GET', 'HEAD'].includes(request.method)) { value = { title: 'Fixture mutation blocked', status: 405 }; status = 405 }
    else if (forbidden && path.startsWith('/api/game/902/details')) { value = { title: 'forbidden', status: 403 }; status = 403 }
    else if (value === undefined) { unknown.add(path); value = [] }
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: status, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(value)).toString('base64') })
  })
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1600, height: 1200, deviceScaleFactor: 1, mobile: false })
  await cdp.send('Page.navigate', { url: `${target}/games/901/challenges` })
  await waitFor(`document.querySelectorAll('[data-globe-node]').length === 5`)
  assert.equal(await evaluate(`getComputedStyle(document.querySelector('header[data-competition]')).display !== 'none'`), true)
  await inspect('desktop-globe-categories')
  const before = requests.length
  await evaluate(`document.querySelector('[aria-label="Rotate globe right"]').click()`)
  await evaluate(`document.querySelector('[aria-label="Reset globe view"]').click()`)
  assert.equal(requests.slice(before).filter((request) => request.path.includes('/challenges/')).length, 0)
  await evaluate(`document.querySelector('[data-globe-node="category-Pwn"]').focus()`)
  await press('Enter')
  await waitFor(`document.querySelectorAll('[data-globe-node]').length === 8`)
  await evaluate(`document.querySelector('input[placeholder="Name or ID"]').focus()`)
  await cdp.send('Input.insertText', { text: 'Ret2win' })
  await waitFor(`document.querySelectorAll('[data-globe-node]').length === 1`)
  await selectView('list')
  await evaluate(`document.querySelector('[data-challenge-row="9001"]').focus()`)
  await press('Enter')
  await waitFor(`document.querySelector('[data-challenge-detail] input')`)
  assert.equal(await evaluate(`document.activeElement.id`), 'competition-challenge-title')
  await inspect('desktop-list-selected')
  await selectView('globe')
  await waitFor(`document.querySelector('[data-challenge-detail]')`)
  assert.equal(await evaluate(`location.hash.startsWith('#9001-')`), true)
  await inspect('desktop-globe-selected')
  await evaluate(`document.querySelector('[data-challenge-detail] button[aria-label="Close"]').click()`)
  await waitFor(`!document.querySelector('[data-challenge-detail]')`)
  await selectView('list')
  await evaluate(`document.querySelector('input[placeholder="Name or ID"]').focus()`)
  await cdp.send('Input.insertText', { text: 'does-not-exist' })
  await waitFor(`document.body.innerText.includes('No matching challenges')`)
  await inspect('empty-search')
  await evaluate(`Array.from(document.querySelectorAll('button')).find(b => b.innerText === 'Reset filters').click()`)
  await waitFor(`document.querySelectorAll('[data-challenge-row]').length === 10`)
  for (const [name, width, height] of [['compact', 320, 568], ['mobile', 390, 844], ['tablet', 768, 1024], ['wide', 1920, 1080]]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await selectView('list')
    await inspect(`${name}-list`)
    if (width < 400) {
      await evaluate(`document.querySelector('[data-challenge-list]').scrollIntoView({ block: 'start', behavior: 'instant' })`)
      await inspect(`${name}-list-content`)
    }
    await evaluate(`(document.querySelector('[data-challenge-row="9001"]') ?? document.querySelector('[data-challenge-row]')).click()`)
    await waitFor(width >= 1100 ? `document.querySelector('[data-challenge-detail] input')` : `document.querySelector('[role="dialog"] input')`)
    await inspect(`${name}-detail`)
    if (width < 1100) { await press('Escape'); await waitFor(`!document.querySelector('[role="dialog"]')`) }
    else { await evaluate(`document.querySelector('[data-challenge-detail] button[aria-label="Close"]').click()`) }
  }
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false })
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `if (location.origin === ${JSON.stringify(new URL(target).origin)}) { localStorage.setItem('language', JSON.stringify('id-ID')); localStorage.setItem('mantine-color-scheme-value', 'light'); }` })
  await cdp.send('Page.navigate', { url: `${target}/games/901/challenges` })
  await waitFor(`document.querySelector('[data-challenge-list]')`)
  await inspect('light-indonesian-list')
  await selectView('globe')
  await inspect('light-indonesian-globe')
  await evaluate(`document.querySelector('[data-challenge-globe]').scrollIntoView({ block: 'center', behavior: 'instant' })`)
  await inspect('light-indonesian-globe-content')
  forbidden = true
  await cdp.send('Page.navigate', { url: `${target}/games/902/challenges#999999-hidden` })
  await waitFor(`document.querySelector('[role="alert"]')`)
  assert.equal(await evaluate(`!!document.querySelector('[data-challenge-detail]')`), false)
  assert.equal(requests.some((request) => request.path.includes('/challenges/999999')), false)
  await inspect('forbidden-no-detail')
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, unknown: [...unknown], requests }, null, 2))
  await browser.close()
}
assert.deepEqual([...unknown], [])
assert.deepEqual(reports.filter((report) => report.overflow || report.violations.length || report.errors.length), [])
console.log(`PASS: ${reports.length} browser/Axe views`)
