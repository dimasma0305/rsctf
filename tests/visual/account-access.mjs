// Real Chromium regressions against the built UI and invented API responses.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { repositoryRoot } from './route-catalog.mjs'
import {
  accountAccessFixture,
  serveAccountBuild
} from './account-access-fixtures.mjs'

const output = resolve(
  process.env.RSCTF_ACCOUNT_ACCESS_OUTPUT ||
    resolve(repositoryRoot, 'visual-audit-output/account-access')
)
mkdirSync(output, { recursive: true })
const server = await serveAccountBuild(
  process.env.RSCTF_ACCOUNT_ACCESS_BUILD_ROOT ||
    resolve(repositoryRoot, 'web/build')
)
const fixture = accountAccessFixture()
const browser = await launchBrowser().catch(async (error) => {
  await server.close()
  throw error
})
const { cdp } = browser
const results = [],
  exceptions = [],
  interceptionErrors = [],
  pending = new Set()
const destination = '/games/901/challenges?division=2#9001-Ret2win'
const encoded = encodeURIComponent(destination)
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true
  })
  if (r.exceptionDetails)
    throw new Error(
      r.exceptionDetails.exception?.description || r.exceptionDetails.text
    )
  return r.result?.value
}
const until = async (read, message) => {
  for (let i = 0; i < 80; i++) {
    if (await read()) return
    await pause(100)
  }
  throw new Error('Timed out: ' + message)
}
const wait = (expression) =>
  until(() => evaluate(`Boolean(document.body && (${expression}))`), expression)
const click = async (selector) => {
  await wait(`document.querySelector(${JSON.stringify(selector)})`)
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`)
}
const fill = async (selector, text, index = 0) => {
  await evaluate(
    `document.querySelectorAll(${JSON.stringify(selector)})[${index}].focus()`
  )
  await cdp.send('Input.insertText', { text })
}
const visit = async (path, options = {}) => {
  fixture.reset(options)
  await cdp.send('Page.navigate', { url: 'about:blank' })
  await cdp.send('Page.navigate', { url: server.origin + path })
  await wait('document.querySelector("#main-content h1")')
  await evaluate('document.fonts.ready')
}
const settled = (path) =>
  until(
    () =>
      fixture.state.calls.some((call) => call.path === path && call.settled),
    path + ' response'
  )
const openAuthenticatedProfile = async () => {
  await wait(
    'document.querySelector("[data-guide=account-menu], [data-guide=more-navigation]")'
  )
  if (
    await evaluate(
      'Boolean(document.querySelector("[data-guide=account-menu]"))'
    )
  )
    await click('[data-guide="account-menu"]')
  else await click('[data-guide="more-navigation"]')
  await wait('document.querySelector("[data-guide=account-profile]")')
  await click('[data-guide="account-profile"]')
  await wait('document.querySelector("[data-profile-form]")')
}
const signInFields = async () => {
  await wait('document.querySelector("#main-content input[type=password]")')
  await fill('#main-content input[type=text]', 'audit-user')
  await fill('#main-content input[type=password]', 'FixturePassword123!')
}
const registerFields = async () => {
  await wait('document.querySelector("#main-content input[type=email]")')
  await fill('#main-content input[type=email]', 'audit@example.invalid')
  await fill('#main-content input[type=text]', 'audit-user')
  await fill('#main-content input[type=password]', 'FixturePassword123!', 0)
  await fill('#main-content input[type=password]', 'FixturePassword123!', 1)
}
const controls = async () =>
  evaluate(`({username:document.querySelector('#main-content input[type=text]')?.value,
  passwordLength:document.querySelector('#main-content input[type=password]')?.value.length})`)
const capture = async (name) => {
  const shot = await cdp.send('Page.captureScreenshot', {
    format: 'png',
    captureBeyondViewport: false
  })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
}
const audit = async (name) => {
  await evaluate(
    readFileSync(
      resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'),
      'utf8'
    )
  )
  const violations = await evaluate(
    `(async () => (await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,targets:v.nodes.map(n=>n.target)})))()`
  )
  assert.deepEqual(violations, [], 'WCAG violations')
  assert.equal(
    await evaluate('document.documentElement.scrollWidth > innerWidth + 1'),
    false,
    'Horizontal overflow'
  )
  await capture(name)
}
const check = async (name, fn) => {
  try {
    await fn()
    assert.deepEqual(
      fixture.state.blockedWrites,
      [],
      'Unexpected API mutations'
    )
    assert.deepEqual(fixture.state.unknown, [], 'Missing API fixtures')
    results.push({ name, status: 'passed' })
    console.log('PASS', name)
  } catch (error) {
    await capture(name + '-failure').catch(() => {})
    results.push({ name, status: 'failed', error: error.message })
    console.error('FAIL', name, error.message)
  } finally {
    fixture.releaseAll()
  }
}

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) =>
    exceptions.push(
      exceptionDetails.exception?.description || exceptionDetails.text
    )
  )
  await cdp.send('Fetch.enable', {
    patterns: [
      { urlPattern: server.origin + '/api/*' },
      { urlPattern: server.origin + '/hub*' }
    ]
  })
  cdp.on('Fetch.requestPaused', ({ requestId, request }) => {
    const task = (async () => {
      const r = await fixture.response(new URL(request.url), request.method)
      await cdp.send('Fetch.fulfillRequest', {
        requestId,
        responseCode: r.status || 200,
        responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
        body: Buffer.from(JSON.stringify(r.body)).toString('base64')
      })
    })()
      .catch((error) => {
        // Canceled XHRs and document replacements retire their interception IDs.
        if (!/Invalid InterceptionId|Invalid request id/i.test(error.message))
          interceptionErrors.push(error.message)
      })
      .finally(() => pending.delete(task))
    pending.add(task)
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
    source: `if(window===window.top && location.origin===${JSON.stringify(server.origin)}){
    localStorage.setItem('language',JSON.stringify(new URLSearchParams(location.search).get('_auditLang')||'en-US'));
    localStorage.setItem('mantine-color-scheme-value','dark');
    for(const id of ['guest','${fixture.profile.userId}'])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));}`
  })
  await cdp.send('Emulation.setEmulatedMedia', {
    features: [{ name: 'prefers-reduced-motion', value: 'reduce' }]
  })

  for (const [viewport, width, height] of [
    ['desktop', 1440, 1100],
    ['tablet', 768, 1024],
    ['mobile', 390, 844],
    ['compact', 320, 568]
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false
    })
    await check(viewport + '-initial-profile-keeps-draft', async () => {
      await visit('/account/login', { holdInitialProfile: true })
      await signInFields()
      const before = await controls()
      fixture.release('initialProfile')
      await settled('/api/account/profile')
      await pause(150)
      assert.deepEqual(await controls(), before)
    })
    await check(viewport + '-registration-publishes-session', async () => {
      await visit('/account/register')
      await settled('/api/account/profile')
      await registerFields()
      await click('button[type=submit]')
      await wait('location.pathname === "/"')
      await openAuthenticatedProfile()
      assert.equal(fixture.state.authenticated, true)
      await capture(viewport + '-registered-session')
    })
    for (const [name, options, boundary, release] of [
      ['pending-login', { holdLogin: true }, '/api/account/login', 'login'],
      [
        'pending-profile',
        { holdFreshProfile: true },
        '/api/account/profile',
        'freshProfile'
      ]
    ])
      await check(viewport + '-leave-' + name, async () => {
        await visit('/account/login?from=' + encoded, options)
        await settled('/api/account/profile')
        await signInFields()
        await click('button[type=submit]')
        await until(
          () =>
            fixture.state.calls.filter((call) => call.path === boundary)
              .length >= (release === 'freshProfile' ? 2 : 1),
          'held ' + boundary
        )
        await click('#main-content a[href="/"]')
        await wait('location.pathname === "/"')
        fixture.release(release)
        await settled('/api/account/login')
        await pause(300)
        assert.equal(await evaluate('location.pathname'), '/')
      })
    await check(viewport + '-registration-retains-destination', async () => {
      await visit('/account/login?from=' + encoded, {
        config: { allowPasswordRegistration: false }
      })
      await click('a[href^="/account/register"]')
      await wait(
        'document.querySelector("#main-content h1")?.textContent === "Register" && location.pathname === "/account/register"'
      )
      assert.equal(
        await evaluate('new URLSearchParams(location.search).get("from")'),
        destination
      )
      await click('[data-guide=account-oauth]')
      await wait('location.pathname === "/api/oauth/discord"')
      assert.equal(
        await evaluate('new URLSearchParams(location.search).get("returnUrl")'),
        destination
      )
    })
    await check(
      viewport + '-keyboard-recovery-retains-destination',
      async () => {
        await visit('/account/login?from=' + encoded)
        await evaluate(
          'document.querySelector("a[href^=\\"/account/recovery\\"]").focus()'
        )
        for (const type of ['keyDown', 'keyUp'])
          await cdp.send('Input.dispatchKeyEvent', {
            type,
            key: 'Enter',
            code: 'Enter',
            windowsVirtualKeyCode: 13
          })
        await wait('location.pathname === "/account/recovery"')
        assert.equal(
          await evaluate('new URLSearchParams(location.search).get("from")'),
          destination
        )
        await audit(viewport + '-recovery')
        await click('a[href^="/account/login"]')
        await wait('location.pathname === "/account/login"')
        assert.equal(
          await evaluate('new URLSearchParams(location.search).get("from")'),
          destination
        )
      }
    )
    await check(viewport + '-closed-registration-is-explicit', async () => {
      await visit('/account/register', { config: { allowRegister: false } })
      assert.equal(
        await evaluate(
          'document.querySelectorAll("#main-content input").length'
        ),
        0
      )
      assert.equal(
        await evaluate('document.querySelector("button[type=submit]")'),
        null
      )
      assert.equal(
        await evaluate(
          'document.querySelector("#main-content h1").textContent'
        ),
        'Registration is closed'
      )
      await audit(viewport + '-closed-registration')
    })
    await check(
      viewport + '-oauth-errors-are-persistent-and-translated',
      async () => {
        await visit(
          '/account/login?error=oauth_state_expired&_auditLang=id-ID',
          { config: { allowPasswordRegistration: false } }
        )
        const error = await evaluate(
          'document.querySelector("#main-content [role=alert]")?.innerText'
        )
        assert.ok(
          error?.includes('kedaluwarsa'),
          'Explain the expired request in Indonesian'
        )
        assert.ok(!error.includes('use your password'))
        await audit(viewport + '-expired-oauth')
      }
    )
    await check(viewport + '-pending-page-does-not-invent-email', async () => {
      await visit('/account/pending?from=' + encoded)
      assert.equal(
        await evaluate('document.body.innerText.includes("ctf@example.com")'),
        false
      )
      assert.equal(
        await evaluate(
          'document.body.innerText.includes("We sent an email to:")'
        ),
        false
      )
      assert.equal(
        await evaluate(
          'new URL(document.querySelector("a[href^=\\"/account/register\\"]").href).searchParams.get("from")'
        ),
        destination
      )
      await audit(viewport + '-pending-without-context')
    })
  }
  await cdp.send('Emulation.setDeviceMetricsOverride', {
    width: 1440,
    height: 1100,
    deviceScaleFactor: 1,
    mobile: false
  })
  await check(
    'late-anonymous-profile-cannot-replace-successful-login',
    async () => {
      await visit('/account/login?from=' + encoded, {
        holdInitialProfile: true
      })
      await signInFields()
      await click('button[type=submit]')
      await wait('location.pathname === "/games/901/challenges"')
      await wait(
        `document.body.innerText.includes(${JSON.stringify(fixture.profile.userName)})`
      )
      fixture.release('initialProfile')
      await settled('/api/account/profile')
      await pause(150)
      assert.equal(
        await evaluate(
          'document.body.innerText.includes("Account Regression Player")'
        ),
        true
      )
    }
  )
  await check(
    'confirmation-retains-real-email-without-putting-it-in-url',
    async () => {
      await visit('/account/register?from=' + encoded, {
        registerStatus: 'EmailConfirmationRequired'
      })
      await registerFields()
      await click('button[type=submit]')
      await wait('location.pathname === "/account/pending"')
      assert.equal(
        await evaluate(
          'document.body.innerText.includes("audit@example.invalid")'
        ),
        true
      )
      assert.equal(
        await evaluate(
          'location.href.includes("audit%40") || location.href.includes("audit@example")'
        ),
        false
      )
      assert.equal(
        await evaluate('new URLSearchParams(location.search).get("from")'),
        destination
      )
      await audit('pending-with-email')
    }
  )
  await check(
    'unknown-registration-policy-offers-retry-without-a-form',
    async () => {
      await visit('/account/register', { configError: true })
      assert.equal(
        await evaluate(
          'document.querySelectorAll("#main-content input").length'
        ),
        0
      )
      await wait(
        'document.querySelector("#main-content [role=alert]")?.textContent.includes("settings could not be loaded")'
      )
      fixture.state.options.configError = false
      await click('#main-content button')
      await wait('document.querySelector("#main-content input[type=email]")')
    }
  )
  await check(
    'bootstrap-stays-available-when-public-registration-is-closed',
    async () => {
      await visit('/account/register?bootstrap=1', {
        config: { allowRegister: false, allowPasswordRegistration: false }
      })
      await wait('document.querySelector("input[autocomplete=off]")')
      assert.equal(
        await evaluate('document.body.innerText.includes("Setup token")'),
        true
      )
    }
  )
  await check(
    'fingerprint-policy-still-disables-external-sign-in',
    async () => {
      await visit('/account/login', {
        config: { enableBrowserFingerprint: true }
      })
      assert.equal(
        await evaluate('document.querySelector("[data-guide=account-oauth]")'),
        null
      )
      assert.equal(
        await evaluate(
          'document.body.innerText.includes("External sign-in unavailable")'
        ),
        true
      )
    }
  )
  await check('oauth-guidance-remains-after-normal-toast-timeout', async () => {
    await visit('/account/login?error=oauth_state_expired')
    await pause(5200)
    assert.equal(
      await evaluate(
        'document.querySelector("#main-content [role=alert]")?.textContent.includes("expired")'
      ),
      true
    )
  })
} finally {
  fixture.releaseAll()
  await Promise.allSettled([...pending])
  await browser.close()
  await server.close()
  const summary = {
    results,
    exceptions,
    interceptionErrors,
    unhandledApiRequests: server.unhandled
  }
  writeFileSync(
    resolve(output, 'report.json'),
    JSON.stringify(summary, null, 2)
  )
  const failures = results.filter((result) => result.status === 'failed')
  console.log(
    JSON.stringify({
      passed: results.length - failures.length,
      failed: failures.length,
      exceptions,
      interceptionErrors,
      unhandledApiRequests: server.unhandled
    })
  )
  if (
    failures.length ||
    exceptions.length ||
    interceptionErrors.length ||
    server.unhandled.length
  )
    process.exitCode = 1
}
