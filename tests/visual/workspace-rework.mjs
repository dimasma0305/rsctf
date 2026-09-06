// Local visual fixtures, not authentication or backend integration tests.
// Every API request is intercepted inside Chromium; mutations never reach RSCTF.
import assert from 'node:assert/strict'
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_WORKSPACE_PREVIEW || 'http://127.0.0.1:63017'
const cardsOnly = process.argv.includes('--cards')
assert.equal(new URL(target).hostname, '127.0.0.1', 'fixtures are restricted to loopback')
const output = resolve(process.env.RSCTF_WORKSPACE_OUTPUT || '../visual-audit-output/rework-workspaces')
mkdirSync(output, { recursive: true })
const now = Date.now()
const profile = { userId: '11111111-1111-4111-8111-111111111111', role: 'Admin', userName: 'Morgan', email: 'morgan@example.invalid', hasManagedGames: true }
const game = { id: 901, title: 'Signal 2026 — Team Championship', summary: 'One team. A full day of challenges.', content: '## Welcome\nChoose a challenge and work with your team.', start: now - 3600000, end: now + 21600000, serverTime: now, limit: 3, joined: true, status: 'Accepted', participationStatus: 'Accepted', teamCount: 8, userCount: 24, practiceMode: false, allowUserSubmissions: true, divisions: [{ id: 1, name: 'Open' }], division: 1, teamName: 'Null Pointers', writeupRequired: true }
const challenges = ['A surprisingly long challenge title that must remain readable', 'Packet postcard', 'Lost in translation', 'Hidden in plain sight', 'Tiny machine', 'Last transmission'].map((title, index) => ({ id: 9001 + index, title, type: 'StaticAttachment', category: index % 2 ? 'Misc' : 'Web', score: 500 - index * 40, solved: index + 1, bloods: [], disableBloodBonus: true }))
if (cardsOnly) {
  challenges.forEach((challenge, index) => { challenge.category = ['Web', 'Misc', 'Reverse', 'Crypto', 'Pwn', 'Hardware'][index] })
  challenges[2].title = challenges[0].title
  challenges[0].title = 'Packet postcard'
  challenges[0].score = 500
  challenges[0].solved = 12
  challenges[1].title = 'Lost in translation'
  challenges[4].bloods = [{ id: 1, name: 'A first-blood team with a long readable name', submitTimeUtc: now - 60000 }]
  challenges[5].deadline = now - 60000
}
const categories = Object.groupBy(challenges, (challenge) => challenge.category)
const ranks = ['Null Pointers', 'Stacked Together', 'A very long team name that remains usable on small screens'].map((name, index) => ({ id: index + 1, name, divisionId: 1, divisionRank: index + 1, rank: index + 1, score: 1200 - index * 150, lastSubmissionTime: now - 60000, solvedCount: index === 0 ? 1 : 0, solvedChallenges: index === 0 ? [{ id: 9002, score: 460, type: 'Normal', time: now - 60000 }] : [] }))
const teams = ranks.map((rank) => ({ id: rank.id, name: rank.name, bio: 'Ready for the next challenge.', locked: false, members: [{ id: profile.userId, userName: 'Morgan', captain: true }, { id: '22222222-2222-4222-8222-222222222222', userName: 'Alex', captain: false }] }))
const config = { title: 'Signal', slogan: 'A place to play. A reason to learn.', portMapping: 'Default', allowRegister: true, allowPasswordRegistration: true, allowTeamCreation: true, emailConfirmationRequired: false, enableBrowserFingerprint: false, defaultLifetime: 120, extensionDuration: 120, renewalWindow: 10 }
const settings = { revision: 1, globalConfig: { ...config }, accountPolicy: { allowRegister: true, allowPasswordRegistration: true, allowTeamCreation: true }, containerPolicy: { portMapping: 'Default', defaultLifetime: 120, extensionDuration: 120, renewalWindow: 10 }, containerProvider: { type: 'Docker', name: 'Docker', available: true }, buildRegistry: {}, email: {}, captcha: { provider: 'None' }, oAuth: {}, registry: {}, donations: { enabled: false }, proxyTrust: { enabled: false, trustedNetworksCsv: '' } }

const responses = {
  '/api/account/profile': profile,
  '/api/config': config,
  '/api/captcha': { type: 'None' },
  '/api/admin/config': settings,
  '/api/admin/users': teams.flatMap((team) => team.members).slice(0, 3).map((member, index) => ({ ...member, id: index === 0 ? profile.userId : `22222222-2222-4222-8222-22222222222${index}`, userName: ['Morgan', 'Alex', 'Riley'][index], email: `player${index}@example.invalid`, role: index === 0 ? 'Admin' : 'User', emailConfirmed: true, registrationTimeUtc: now - 86400000 })),
  '/api/admin/teams': teams,
  '/api/admin/dashboard': { systemStats: { userCount: 24, teamCount: 8, activeContainerCount: 6 }, topGames: [game] },
  '/api/admin/submissiontrend': [], '/api/admin/reviews': [], '/api/admin/writeups': [], '/api/admin/cheat-reports': [],
  '/api/edit/games': { data: [game], total: 1, length: 1 },
  '/api/edit/games/901': { ...game, revision: 1 },
  '/api/edit/games/901/challenges': challenges,
  '/api/game': { data: [game], total: 1, length: 1 },
  '/api/game/recent': [game],
  '/api/game/901': game,
  '/api/game/901/notices': [],
  '/api/game/901/details': { challenges: categories, challengeCount: challenges.length, rank: ranks[0], teamToken: 'visual-fixture-not-a-credential', writeupRequired: true, writeupDeadline: now + 86400000 },
  '/api/game/901/details/participant': { rank: ranks[0] },
  '/api/game/901/scoreboard': { updateTimeUtc: now, bloodBonus: 0, timelines: [], items: ranks, divisions: [{ id: 1, name: 'Open' }], challenges: categories, challengeCount: challenges.length },
  '/api/game/901/writeup': { submitted: false, note: 'Combine your solutions into one PDF.' },
  '/api/team': teams,
  '/api/team/selector': teams,
  '/api/admin/instances': { items: [], total: 0, page: 1, pageSize: 20 },
  '/api/admin/instances/filter-options': { games: [], challenges: [], teams: [] },
}
responses['/api/admin/users'] = { data: responses['/api/admin/users'], total: 3, length: 3 }

const browser = await launchBrowser()
const { cdp } = browser
const reports = []
const unknownPaths = new Set()
const mutations = []
let runtimeErrors = []
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const waitFor = async (expression) => {
  for (let index = 0; index < 120; index++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise((r) => setTimeout(r, 250))
  }
  throw new Error(`Timed out: ${expression}`)
}
const screenshot = async (name) => {
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png' })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
}
const inspect = async (name) => {
  await cdp.send('Runtime.evaluate', { expression: readFileSync('node_modules/axe-core/axe.min.js', 'utf8') })
  const issues = await evaluate(`(async () => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1, fallback: !!document.querySelector('[data-error-fallback]'), violations: (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa'] } })).violations.map(v => ({ id: v.id, nodes: v.nodes.map(n => n.target) })) }))()`)
  await screenshot(name)
  reports.push({ name, ...issues, runtimeErrors: [...runtimeErrors] })
  console.log(`${name}: ${JSON.stringify(issues)}`)
}
const press = async (key, code = key) => {
  const windowsVirtualKeyCode = { Enter: 13, ' ': 32, Tab: 9, Escape: 27 }[key]
  const text = key === 'Enter' ? '\r' : key === ' ' ? ' ' : undefined
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key, code, windowsVirtualKeyCode, text, unmodifiedText: text })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode })
}
const visit = async (path, name) => {
  runtimeErrors = []
  await cdp.send('Page.navigate', { url: `${target}${path}` })
  try {
    await waitFor(`document.querySelector('h1') && !document.querySelector('.mantine-LoadingOverlay-root')`)
    if (path.endsWith('/challenges')) await waitFor(`document.querySelectorAll('[data-guide="challenge-card"]').length === 6`)
  } catch (error) { runtimeErrors.push(error.message) }
  await new Promise((r) => setTimeout(r, 600))
  if (cardsOnly) await evaluate(`document.querySelector('[data-guide="challenge-card"]')?.scrollIntoView({ block: 'center', behavior: 'instant' })`)
  await inspect(name)
  if (cardsOnly) {
    // Native activation and the CSS hit area; mounted tests check the real
    // callback separately, so this fixture never fetches a challenge or mutates it.
    await evaluate(`(() => {
      window.cardClicks = 0;
      window.cardClickController = new AbortController();
      const button = document.querySelector('[data-guide="challenge-card"] button');
      button.addEventListener('click', event => { event.preventDefault(); event.stopPropagation(); window.cardClicks++; }, { signal: window.cardClickController.signal });
      button.focus();
    })()`)
    assert.equal(await evaluate(`document.activeElement === document.querySelector('[data-guide="challenge-card"] button')`), true, 'card button must receive focus')
    await press('Enter')
    assert.equal(await evaluate('window.cardClicks'), 1, 'Enter must activate the native card button')
    await press(' ', 'Space')
    assert.equal(await evaluate('window.cardClicks'), 2, 'Space must activate the native card button')
    assert.equal(await evaluate(`document.activeElement.matches(':focus-visible') && parseFloat(getComputedStyle(document.activeElement.closest('article')).outlineWidth) >= 2`), true, 'card focus must remain visible')
    await screenshot(`${name}-keyboard`)
    const point = await evaluate(`(() => { const rect = document.querySelector('[data-guide="challenge-card"] dl').getBoundingClientRect(); return { x: rect.x + 12, y: rect.y + 12 }; })()`)
    await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button: 'left', clickCount: 1 })
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'left', clickCount: 1 })
    assert.equal(await evaluate('window.cardClicks'), 3, 'the score area must open the same card action')
    await evaluate('window.cardClickController.abort(); document.activeElement.blur()')
  }
}

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => runtimeErrors.push(exceptionDetails.exception?.description ?? exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('rsctf-player-guide:${profile.userId}', JSON.stringify({ interactiveEnabled:false, completedVersion:1, seenFeatures:[] })); localStorage.setItem('rsctf-player-guide:guest', JSON.stringify({ interactiveEnabled:false, completedVersion:1, seenFeatures:[] }));` })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: `${target}/api/*` }, { urlPattern: `${target}/hub*` }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname.toLowerCase()
    let value = responses[path]
    if (!['GET', 'HEAD'].includes(request.method)) {
      mutations.push({ path, method: request.method })
      value = { title: 'Visual fixture: mutations disabled', status: 405 }
    } else if (value === undefined) { unknownPaths.add(path); value = [] }
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: !['GET', 'HEAD'].includes(request.method) ? 405 : 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(value)).toString('base64') })
  })
  const pages = cardsOnly ? ['/games/901/challenges'] : ['/admin/dashboard', '/admin/games', '/admin/users', '/admin/settings', '/teams', '/account/profile', '/games/901/challenges', '/games/901/scoreboard']
  for (const [viewport, width, height] of [['desktop', 1440, 1100], ['compact', 320, 568]]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    for (const path of pages) {
      await visit(path, `${viewport}${path.replaceAll('/', '--')}`)
    }
  }
  if (!cardsOnly) {
    await cdp.send('Page.navigate', { url: `${target}/admin/settings` })
    await waitFor(`document.querySelector('#settings-tab-account')`)
    await evaluate(`document.querySelector('#settings-tab-account').click()`)
    await inspect('compact-settings-account')
    await evaluate(`document.querySelector('[data-workspace-bar] button').focus(); document.querySelector('[data-workspace-bar] button').click()`)
    await waitFor(`document.querySelector('[role="dialog"] input')`)
    await inspect('compact-quick-navigation')
    assert.equal(await evaluate(`document.activeElement === document.querySelector('[role="dialog"] input')`), true)
    await cdp.send('Input.insertText', { text: 'settings' })
    await waitFor(`document.querySelectorAll('[role="dialog"] a').length === 1`)
    await press('ArrowDown')
    assert.equal(await evaluate(`document.activeElement.getAttribute('href')`), '/admin/settings')
    await press('Escape')
    await waitFor(`!document.querySelector('[role="dialog"]')`)
    await waitFor(`document.activeElement === document.querySelector('[data-workspace-bar] button')`)
    await evaluate(`document.querySelector('#settings-tab-account').focus()`)
    await press('ArrowDown')
    await waitFor(`document.activeElement.id === 'settings-tab-container'`)
    await press('Home')
    await waitFor(`document.activeElement.id === 'settings-tab-platform'`)

    await visit('/games/901/challenges', 'compact-challenge-interactions')
    await evaluate(`document.querySelector('input[placeholder="Name or ID"]').focus()`)
    await cdp.send('Input.insertText', { text: '9002' })
    await waitFor(`document.querySelectorAll('[data-guide="challenge-card"]').length === 1`)
    await cdp.send('Input.insertText', { text: '-no-match' })
    await waitFor(`document.body.innerText.includes('No matching challenges')`)
    await inspect('compact-empty-challenge-search')
    await evaluate(`Array.from(document.querySelectorAll('button')).find(b => b.innerText === 'Reset filters').click()`)
    await waitFor(`document.querySelectorAll('[data-guide="challenge-card"]').length === 6`)
    await evaluate(`Array.from(document.querySelectorAll('button')).find(b => b.innerText === 'Submit Writeup').focus(); Array.from(document.querySelectorAll('button')).find(b => b.innerText === 'Submit Writeup').click()`)
    await waitFor(`document.querySelector('[role="dialog"]')`)
    await inspect('compact-writeup-dialog')
    await press('Escape')
    await waitFor(`!document.querySelector('[role="dialog"]')`)

  }
  for (const [viewport, width, height] of [['mobile', 390, 844], ['tablet', 768, 1024], ['wide', 1920, 1080]]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    for (const path of cardsOnly ? ['/games/901/challenges'] : ['/admin/settings', '/games/901/challenges']) await visit(path, `${viewport}${path.replaceAll('/', '--')}`)
  }
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('language', JSON.stringify('id-ID')); localStorage.setItem('mantine-color-scheme-value', 'light');` })
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false })
  for (const path of cardsOnly ? ['/games/901/challenges'] : ['/admin/settings', '/games/901/challenges', '/teams']) await visit(path, `light-id${path.replaceAll('/', '--')}`)
  assert.equal(await evaluate(`document.documentElement.getAttribute('data-mantine-color-scheme')`), 'light')
  assert.equal(await evaluate(`document.documentElement.lang`), 'id-id')
  if (cardsOnly) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width: 320, height: 568, deviceScaleFactor: 1, mobile: false })
    await visit('/games/901/challenges', 'light-id-compact-cards')
  }
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, unknownPaths: [...unknownPaths], blockedMutations: mutations }, null, 2))
  await browser.close()
}
assert.ok(reports.length >= (cardsOnly ? 7 : 30))
assert.deepEqual([...unknownPaths], [])
assert.deepEqual(reports.filter((r) => r.overflow || r.fallback || r.violations.length || r.runtimeErrors.length), [])
