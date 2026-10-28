// Real Chromium cookies and cross-site top-level redirects. Called by the
// disposable Rust OAuth/PostgreSQL fixture; no API interception or token logs.
import assert from 'node:assert/strict'
import { setTimeout as pause } from 'node:timers/promises'
import { launchBrowser } from './cdp.mjs'

const origin = new URL(process.argv[2])
assert.equal(origin.hostname, '127.0.0.1')
assert.equal(origin.protocol, 'http:')
const browser = await launchBrowser()
try {
  const { cdp } = browser
  await cdp.send('Network.enable')
  await cdp.send('Page.enable')
  await cdp.send('Page.navigate', { url: new URL('/begin', origin).href })
  let done = false
  for (let attempt = 0; attempt < 150; attempt++) {
    const { result } = await cdp.send('Runtime.evaluate', { expression: 'location.pathname' })
    if (result.value === '/done') { done = true; break }
    await pause(100)
  }
  assert.ok(done, 'OAuth navigation must finish at the authorized return path')
  const { cookies } = await cdp.send('Network.getCookies', { urls: [origin.href] })
  const session = cookies.find(cookie => cookie.name === 'RSCTF_Token')
  assert.ok(session?.value, 'browser must retain a nonempty session cookie')
  assert.equal(session.httpOnly, true)
  assert.equal(session.sameSite, 'Lax')
  assert.ok(!cookies.some(cookie => cookie.name === 'RSCTF_OAuthState'),
    'callback must remove the temporary state cookie')
  const { result } = await cdp.send('Runtime.evaluate', {
    expression: "fetch('/api/account/profile').then(response => response.status)",
    awaitPromise: true, returnByValue: true,
  })
  assert.equal(result.value, 200, 'browser must attach the issued cookie to the profile request')
  await cdp.send('Network.clearBrowserCookies')
  await cdp.send('Page.navigate', {
    url: new URL('/callback?code=test-discord-code&state=unbound-state', origin).href,
  })
  let rejected = false
  for (let attempt = 0; attempt < 150; attempt++) {
    const { result } = await cdp.send('Runtime.evaluate', { expression: 'location.pathname' })
    if (result.value === '/account/login') { rejected = true; break }
    await pause(100)
  }
  assert.ok(rejected, 'unbound OAuth state must return to login')
  const failureCookies = await cdp.send('Network.getCookies', { urls: [origin.href] })
  assert.ok(!failureCookies.cookies.some(cookie => cookie.name === 'RSCTF_Token'),
    'failed callback must not authenticate the browser')
  const failureProfile = await cdp.send('Runtime.evaluate', {
    expression: "fetch('/api/account/profile').then(response => response.status)",
    awaitPromise: true, returnByValue: true,
  })
  assert.equal(failureProfile.result.value, 401)
  console.log('OAuth browser cookie regression passed')
} finally {
  await browser.close()
}
