// Local fixtures only: no published posts, event changes, or live mutations.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_COMMUNITY_PREVIEW || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_COMMUNITY_OUTPUT || '../visual-audit-output/community-local')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const errors = [],
  reports = [],
  mutations = [],
  pageRequests = [],
  unknown = new Set()
const uid = '11111111-1111-4111-8111-111111111111'
let scenario = 'populated',
  role = 'User',
  heldRequest
const rows = Array.from({ length: 21 }, (_, i) => ({
  id: String(i + 1).padStart(8, '0'),
  title:
    i === 0
      ? 'Before the competition: schedules, access, and where to find help'
      : i === 1
        ? 'A long title with a link: https://example.invalid/competition/announcements/this-is-a-very-long-title-for-testing-small-screens'
        : 'Competition update ' + (i + 1),
  summary:
    i === 0
      ? 'Read the **player guide** before your first challenge.\n\nCheck your team and event access, then you’re ready to play.'
      : i === 1
        ? ''
        : 'A short update from the organizers, with [more information](/guide) in the player guide.',
  authorName: i === 2 ? 'a-very-long-organizer-name-that-must-wrap-on-a-small-screen' : 'Challenge organizers',
  time: Date.UTC(2026, 8, 7 - i, 8, 30),
  tags: i === 0 ? ['announcement', 'warmup'] : [],
  isPinned: i === 0,
  authorAvatar: null,
}))
const config = {
  title: 'RSCTF',
  portMapping: 'Default',
  allowRegister: true,
  allowPasswordRegistration: true,
  allowTeamCreation: true,
  emailConfirmationRequired: false,
  enableBrowserFingerprint: false,
}
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails)
    throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 100; i++) {
    if (await evaluate('Boolean(document.body && (' + expression + '))')) return
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('Timed out: ' + expression + '; ' + (await evaluate('document.body.innerText.slice(-1200)')))
}
const fulfill = (requestId, value, status = 200) =>
  cdp.send('Fetch.fulfillRequest', {
    requestId,
    responseCode: status,
    responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
    body: Buffer.from(JSON.stringify(value)).toString('base64'),
  })
const audit = async (name) => {
  await evaluate(
    'Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))'
  )
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const result = await evaluate(
    `(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,headings:document.querySelectorAll('h1').length,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`
  )
  const screenshot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(output + '/' + name + '.png', Buffer.from(screenshot.data, 'base64'))
  reports.push({ name, ...result })
  console.log(name, JSON.stringify(result))
}
const navigate = async (path, suffix = '') =>
  cdp.send('Page.navigate', { url: target + path + '?community-check=' + suffix })
try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) =>
    errors.push(exceptionDetails.exception?.description || exceptionDetails.text)
  )
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const url = new URL(request.url),
      path = url.pathname.toLowerCase()
    if (!['GET', 'HEAD'].includes(request.method)) {
      mutations.push({ path, method: request.method })
      return fulfill(requestId, { title: 'Fixture mutation blocked' }, 405)
    }
    if (path === '/api/posts/page') {
      const skip = Number(url.searchParams.get('skip') || 0),
        count = Number(url.searchParams.get('count') || 10)
      pageRequests.push({ skip, count })
      if (scenario === 'loading' || (scenario === 'pagination' && skip === 10)) {
        heldRequest = requestId
        return
      }
      if (scenario === 'error') return fulfill(requestId, { title: 'Fixture unavailable' }, 503)
      return fulfill(
        requestId,
        scenario === 'empty' ? { data: [], total: 0 } : { data: rows.slice(skip, skip + count), total: rows.length }
      )
    }
    if (path === '/api/config') return fulfill(requestId, config)
    if (path === '/api/captcha') return fulfill(requestId, { type: 'None' })
    if (path === '/api/account/profile')
      return fulfill(requestId, { userId: uid, userName: 'Reader', role, email: 'reader@example.invalid' })
    if (path.startsWith('/api/posts/'))
      return fulfill(requestId, { ...rows.find((row) => path.endsWith('/' + row.id)), content: 'Article fixture.' })
    unknown.add(path)
    return fulfill(requestId, [])
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `for(const identity of ['guest',${JSON.stringify(uid)}])localStorage.setItem('rsctf-player-guide:'+identity,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));`,
  })
  for (const [name, width, height, language, scheme] of [
    ['desktop', 1600, 1100, 'en-US', 'dark'],
    ['wide', 1920, 1080, 'en-US', 'dark'],
    ['tablet', 768, 1024, 'en-US', 'dark'],
    ['mobile', 390, 844, 'en-US', 'dark'],
    ['compact', 320, 568, 'en-US', 'dark'],
    ['light-id', 390, 844, 'id-ID', 'light'],
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', {
      features: [{ name: 'prefers-reduced-motion', value: name === 'light-id' ? 'reduce' : 'no-preference' }],
    })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});`,
    })
    scenario = 'populated'
    await navigate('/posts', name)
    await wait(`document.querySelectorAll('[data-post-card]').length===10`)
    assert.equal(await evaluate(`document.querySelectorAll('[data-post-card] h3 a[href]').length`), 10)
    assert.equal(await evaluate(`document.querySelectorAll('[data-post-card][data-pinned]').length`), 1)
    assert.equal(await evaluate(`!!document.querySelector('a[href="/posts/new/edit"]')`), false)
    assert.equal(await evaluate(`document.querySelectorAll('[data-post-card] time[datetime]').length`), 10)
    await audit(name + '-posts')
    await navigate('/about', name)
    await wait(`document.querySelector('[data-about-page]')`)
    assert.equal(
      await evaluate(
        `(()=>{const links=[...document.querySelectorAll('[data-about-page] li a')].map(a=>a.href);return links.length>0&&new Set(links).size===links.length})()`
      ),
      true
    )
    assert.equal(
      await evaluate(`document.querySelectorAll('[data-about-page] a[href="/legal/LICENSING.md"]').length`),
      1
    )
    assert.equal(
      await evaluate(`document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime===Infinity).length`),
      0
    )
    await audit(name + '-about')
  }
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 320, height: 568, deviceScaleFactor: 1, mobile: false })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');`,
  })
  for (const state of ['loading', 'empty', 'error']) {
    scenario = state
    await navigate('/posts', state)
    await wait(
      state === 'loading'
        ? `document.querySelector('[data-posts-loading]')`
        : state === 'empty'
          ? `document.body.innerText.includes('The notice board is quiet')`
          : `document.querySelector('[role="alert"]')`
    )
    await audit('compact-posts-' + state)
    assert.equal(await evaluate(`document.querySelectorAll('[data-post-card]').length`), 0)
    if (state === 'error') {
      scenario = 'populated'
      await evaluate(`document.querySelector('[role="alert"] button').focus()`)
      for (const type of ['keyDown', 'keyUp'])
        await cdp.send('Input.dispatchKeyEvent', {
          type,
          key: 'Enter',
          windowsVirtualKeyCode: 13,
          ...(type === 'keyDown' ? { text: '\r' } : {}),
        })
      await wait(`document.querySelectorAll('[data-post-card]').length===10`)
      assert.equal(await evaluate(`!!document.querySelector('[role="alert"]')`), false)
    }
  }
  scenario = 'pagination'
  heldRequest = undefined
  await navigate('/posts', 'pagination')
  await wait(`document.querySelectorAll('[data-post-card]').length===10`)
  const requestsBefore = pageRequests.length
  await evaluate(`document.querySelector('button[aria-label="Next page"]').click()`)
  await wait(`document.querySelector('[data-posts-loading]')`)
  await new Promise((resolve) => setTimeout(resolve, 500))
  assert.equal(
    await evaluate(`!!document.querySelector('[data-posts-loading]')`),
    true,
    'loading page two does not reset to the page-one cache'
  )
  assert.deepEqual(pageRequests.slice(requestsBefore), [{ skip: 10, count: 10 }])
  assert.equal(await evaluate('document.activeElement.id'), 'post-feed-title')
  await fulfill(heldRequest, { data: rows.slice(10, 20), total: 21 })
  await wait(`document.querySelector('[data-post-card="00000011"]')`)
  await audit('compact-posts-page-two')
  assert.equal(
    await evaluate(`document.querySelector('nav[aria-label="News result pages"] [aria-current="page"]').textContent`),
    '2'
  )
  role = 'Admin'
  scenario = 'populated'
  await navigate('/posts', 'admin')
  await wait(
    `document.querySelector('a[href="/posts/new/edit"]') && document.querySelectorAll('[data-post-card]').length===10`
  )
  assert.equal(await evaluate(`document.querySelectorAll('[data-post-card] a[href$="/edit"]').length`), 10)
  await audit('compact-posts-admin')
  assert.deepEqual([...unknown], [])
  assert.deepEqual(mutations, [])
  assert.deepEqual(errors, [])
  assert.deepEqual(
    reports.filter((report) => report.overflow || report.headings !== 1 || report.violations.length),
    []
  )
} finally {
  writeFileSync(
    output + '/report.json',
    JSON.stringify({ reports, errors, mutations, unknown: [...unknown], pageRequests }, null, 2)
  )
  await close()
}
console.log('PASS: ' + reports.length + ' community page browser/Axe views')
