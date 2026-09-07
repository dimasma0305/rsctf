// Browser fixtures: ALL API requests are intercepted, including every mutation.
// May inspect the deployed frontend without touching a real account or sending mail.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_PROFILE_PREVIEW || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://intechfest.1pc.tf'].includes(target))
const before = process.argv.includes('--before')
const output = resolve(process.env.RSCTF_PROFILE_OUTPUT || '../visual-audit-output/profile-local')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const uid = '11111111-1111-4111-8111-111111111111'
const initial = { userId: uid, userName: 'CipherRunner', email: 'player@example.invalid', role: 'User', bio: null }
let user = { ...initial },
  scenario = 'normal',
  heldSave
const reports = [],
  errors = [],
  unknown = [],
  writes = []
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const wait = async (expression) => {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('Timed out: ' + expression + '; ' + (await evaluate('document.body.innerText.slice(-900)')))
}
const fulfill = (requestId, body, status = 200) =>
  cdp.send('Fetch.fulfillRequest', {
    requestId,
    responseCode: status,
    responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
    body: Buffer.from(JSON.stringify(body)).toString('base64'),
  })
const click = async (label) => {
  const selector = `Array.from(document.querySelectorAll('button')).find(b=>b.textContent.trim()===${JSON.stringify(label)})`
  assert.equal(await evaluate(`Boolean(${selector})`), true, label)
  await evaluate(`${selector}.focus();${selector}.click()`)
}
const key = async (key, code, extra = {}) => {
  for (const type of ['keyDown', 'keyUp'])
    await cdp.send('Input.dispatchKeyEvent', {
      type,
      key,
      windowsVirtualKeyCode: code,
      ...(key === 'Enter' && type === 'keyDown' ? { text: '\r' } : {}),
      ...extra,
    })
}
const fill = async (selector, text) => {
  await evaluate(
    `document.querySelector(${JSON.stringify(selector)}).focus();document.querySelector(${JSON.stringify(selector)}).select()`
  )
  await cdp.send('Input.insertText', { text })
}
const navigate = (name, query = '') =>
  cdp.send('Page.navigate', { url: `${target}/account/profile?profile-check=${name}${query}` })
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
    try {
      const path = new URL(request.url).pathname.toLowerCase()
      if (request.method === 'PUT' && path === '/api/account/update') {
        const body = JSON.parse(request.postData)
        writes.push({ path, body })
        if (scenario === 'save-error') return fulfill(requestId, { title: 'Username already taken', status: 409 }, 409)
        if (scenario === 'save-pending') {
          heldSave = { requestId, body }
          return
        }
        user = { ...user, ...body }
        return fulfill(requestId, { title: 'ok', status: 200 })
      }
      if (request.method === 'PUT' && path === '/api/account/changeemail') {
        const body = JSON.parse(request.postData)
        assert.ok(body.operationId && body.password)
        writes.push({ path })
        user = { ...user, email: body.newMail }
        return fulfill(requestId, { data: false, status: 200 })
      }
      if (request.method === 'PUT' && path === '/api/account/avatar') {
        const operationId = Object.entries(request.headers).find(
          ([name]) => name.toLowerCase() === 'x-rsctf-operation-id'
        )?.[1]
        assert.ok(operationId)
        writes.push({ path, operationId })
        if (scenario === 'avatar-error')
          return fulfill(requestId, { title: 'Fixture upload unavailable', status: 503 }, 503)
        return fulfill(requestId, '/avatar-fixture')
      }
      if (!['GET', 'HEAD'].includes(request.method)) {
        unknown.push(`${request.method} ${path}`)
        return fulfill(requestId, { title: 'Fixture write blocked' }, 405)
      }
      if (path === '/api/config')
        return fulfill(requestId, {
          title: 'RSCTF',
          portMapping: 'Default',
          allowRegister: true,
          allowTeamCreation: true,
          emailConfirmationRequired: false,
          enableBrowserFingerprint: false,
        })
      if (path === '/api/captcha') return fulfill(requestId, { type: 'None' })
      if (path === '/api/account/profile') {
        if (scenario === 'loading') return
        if (scenario === 'anonymous') return fulfill(requestId, { title: 'unauthorized', status: 401 }, 401)
        if (scenario === 'load-error') return fulfill(requestId, { title: 'Fixture unavailable', status: 503 }, 503)
        return fulfill(requestId, user)
      }
      if (path === '/api/account/stats')
        return fulfill(requestId, {
          totalSolves: 0,
          totalFirstBloods: 0,
          gamesParticipated: 0,
          solvesByCategory: {},
          games: [],
        })
      unknown.push(path)
      return fulfill(requestId, [])
    } catch (error) {
      errors.push(error.message)
      await fulfill(requestId, { title: 'Fixture error' }, 500)
    }
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `for(const identity of ['guest',${JSON.stringify(uid)}])localStorage.setItem('rsctf-player-guide:'+identity,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));`,
  })
  for (const [name, width, height, language, scheme] of [
    ['desktop', 1440, 1100, 'en-US', 'dark'],
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
    await navigate(name)
    await wait(before ? `document.querySelector('textarea')` : `document.querySelector('textarea[name=bio]')`)
    if (!before) {
      assert.equal(await evaluate(`document.querySelector('textarea[name=bio]').value`), '')
      assert.equal(await evaluate(`document.querySelector('[data-profile-form] button[type=submit]').disabled`), true)
    }
    await audit(name + '-profile')
    if (before) continue
    await evaluate(`document.querySelector('[data-profile-form] button[type=submit]').scrollIntoView({block:'center'})`)
    await audit(name + '-form-end')
    if (name === 'compact') {
      await click('Change avatar')
      await wait(`document.querySelector('[role=dialog]')`)
      await audit('compact-avatar')
      assert.equal(
        await evaluate(
          `Array.from(document.querySelectorAll('[role=dialog] button')).find(b=>b.textContent.trim()==='Save Avatar').disabled`
        ),
        true
      )
      await key('Escape', 27)
      await wait(`!document.querySelector('[role=dialog]')`)
      assert.equal(await evaluate('document.activeElement.textContent'), 'Change avatar')
      await click('Change Email')
      await wait(`document.querySelector('[role=dialog] input[type=email]')`)
      await audit('compact-email')
      await fill('[role=dialog] input[type=password]', 'fixture-password')
      await key('Escape', 27)
      await wait(`!document.querySelector('[role=dialog]')`)
      await click('Change Email')
      await wait(`document.querySelector('[role=dialog] input[type=password]')`)
      assert.equal(await evaluate(`document.querySelector('[role=dialog] input[type=password]').value`), '')
      await key('Escape', 27)
      await wait(`!document.querySelector('[role=dialog]')`)
      await click('Change Password')
      await wait(`document.querySelector('[role=dialog] input[type=password]')`)
      await audit('compact-password')
      for (let i = 0; i < 12; i++) {
        await key('Tab', 9)
        assert.equal(await evaluate(`!!document.activeElement.closest('[role=dialog]')`), true)
      }
      await key('Escape', 27)
      await wait(`!document.querySelector('[role=dialog]')`)
      assert.equal(await evaluate('document.activeElement.textContent'), 'Change Password')
    }
  }
  if (!before) {
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: 1440,
      height: 1100,
      deviceScaleFactor: 1,
      mobile: false,
    })
    await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');`,
    })
    await navigate('editing')
    await wait(`document.querySelector('[data-profile-form]')`)
    await fill('textarea[name=bio]', 'Keep this unfinished bio')
    await click('Change Email')
    await wait(`document.querySelector('[role=dialog] input[type=email]')`)
    await fill('[role=dialog] input[type=email]', 'updated@example.invalid')
    await fill('[role=dialog] input[type=password]', 'fixture-password')
    await key('Enter', 13)
    await wait(
      `!document.querySelector('[role=dialog]') && document.body.innerText.includes('updated@example.invalid')`
    )
    assert.equal(await evaluate(`document.querySelector('textarea[name=bio]').value`), 'Keep this unfinished bio')
    await click('Discard')
    assert.equal(await evaluate(`document.querySelector('textarea[name=bio]').value`), '')
    await fill('input[name=userName]', 'NewName')
    scenario = 'save-error'
    await key('Enter', 13)
    await wait(`document.querySelector('[data-profile-form] [role=alert]')`)
    assert.equal(await evaluate(`document.querySelector('input[name=userName]').value`), 'NewName')
    await audit('desktop-save-error')
    scenario = 'save-pending'
    await evaluate(
      `document.querySelector('[data-profile-form]').requestSubmit();document.querySelector('[data-profile-form]').requestSubmit()`
    )
    await wait(`document.querySelector('[data-profile-form]').getAttribute('aria-busy')==='true'`)
    assert.equal(
      writes.filter((w) => w.path === '/api/account/update').length,
      2,
      'one failed save, one retry; no duplicate submit'
    )
    await audit('desktop-saving')
    user = { ...user, ...heldSave.body }
    await fulfill(heldSave.requestId, { title: 'ok', status: 200 })
    await wait(`document.querySelector('[data-profile-form]').getAttribute('aria-busy')==='false'`)
    assert.equal(await evaluate(`document.querySelector('input[name=userName]').value`), 'NewName')
    assert.equal(await evaluate(`document.querySelector('[data-profile-form] button[type=submit]').disabled`), true)
    await fill('textarea[name=bio]', 'Retain across stats tab')
    await evaluate(`document.querySelector('[role=tab][data-active]').focus()`)
    await key('ArrowRight', 39)
    await wait(`location.search.includes('tab=stats')`)
    await key('ArrowLeft', 37)
    await wait(`document.querySelector('textarea[name=bio]')`)
    assert.equal(await evaluate(`document.querySelector('textarea[name=bio]').value`), 'Retain across stats tab')
    await key('ArrowRight', 39)
    await wait(`location.search.includes('tab=stats')`)
    await audit('desktop-stats')
    await evaluate(`document.querySelector('[role=tab][data-active]').focus()`)
    await key('ArrowLeft', 37)
    await wait(`document.querySelector('textarea[name=bio]')`)
    assert.equal(await evaluate(`document.querySelector('textarea[name=bio]').value`), 'Retain across stats tab')
    scenario = 'avatar-error'
    await click('Change avatar')
    await wait(`document.querySelector('[role=dialog] input[type=file]')`)
    await cdp.send('DOM.enable')
    const { root } = await cdp.send('DOM.getDocument')
    const { nodeId } = await cdp.send('DOM.querySelector', {
      nodeId: root.nodeId,
      selector: '[role=dialog] input[type=file]',
    })
    await cdp.send('DOM.setFileInputFiles', { nodeId, files: [output + '/desktop-profile.png'] })
    await wait(`document.querySelector('[role=dialog] img')`)
    await click('Save Avatar')
    await wait(`document.body.innerText.includes('Fixture upload unavailable')`)
    assert.equal(await evaluate(`!!document.querySelector('[role=dialog]')`), true)
    scenario = 'normal'
    await click('Save Avatar')
    await wait(`!document.querySelector('[role=dialog]')`)
    const avatarWrites = writes.filter((w) => w.path === '/api/account/avatar')
    assert.equal(avatarWrites.length, 2)
    assert.equal(avatarWrites[0].operationId, avatarWrites[1].operationId)
    assert.equal(await evaluate(`document.querySelector('textarea[name=bio]').value`), 'Retain across stats tab')
    for (const state of ['loading', 'load-error', 'anonymous']) {
      scenario = state
      await navigate(state)
      await wait(
        state === 'loading'
          ? `document.querySelector('[data-profile-loading]')`
          : `document.querySelector('[role=alert]') || location.pathname==='/account/login'`
      )
      assert.equal(await evaluate(`!!document.querySelector('[data-profile-form]')`), false)
      await audit(state)
      if (state === 'load-error') {
        scenario = 'normal'
        await click('Retry')
        await wait(`document.querySelector('[data-profile-form]')`)
      }
    }
    scenario = 'normal'
    user = {
      ...initial,
      userName: 'Very-long-player-name-without-breaks-for-mobile-layout-testing-'.repeat(2),
      email: 'very-long-address-without-breaks-for-mobile-layout@example.invalid',
    }
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: 320,
      height: 568,
      deviceScaleFactor: 1,
      mobile: false,
    })
    await navigate('long-identity')
    await wait(`document.querySelector('[data-profile-form]')`)
    await audit('compact-long-identity')
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
    JSON.stringify(
      { target, fixtures: true, reports, errors, unknown, mutationPaths: writes.map((w) => w.path) },
      null,
      2
    )
  )
  await close()
}
