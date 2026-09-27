// Browser check and documentation screenshots for AI chat links.
// Every API call is served from loopback fixtures; nothing reaches a backend.
//
//   RSCTF_AI_CHAT_TARGET=http://127.0.0.1:63017 node tests/visual/ai-chat-links.mjs
//   RSCTF_AI_CHAT_PUBLISH=1 ...   also refreshes docs/public/screenshots/ai-chat-links-*.png
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { aiChatFixture } from './ai-chat-links-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..')
const target = process.env.RSCTF_AI_CHAT_TARGET || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1', 'fixtures only run against a loopback preview')
const output = resolve(process.env.RSCTF_AI_CHAT_OUTPUT || resolve(root, 'visual-audit-output/ai-chat-links'))
const publish = process.env.RSCTF_AI_CHAT_PUBLISH === '1'
mkdirSync(output, { recursive: true })

const fixture = aiChatFixture()
const unknown = new Set()
const reports = []
let errors = []
const { cdp, close } = await launchBrowser()
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const waitFor = async (expression) => {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    } catch (error) {
      if (!/context|navigated/i.test(error.message)) throw error
    }
    await new Promise((r) => setTimeout(r, 200))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body?.innerText.slice(-1500)')}`)
}
const typeInto = (selector, value) =>
  evaluate(`(() => {
    const input = document.querySelector(${JSON.stringify(selector)});
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ${JSON.stringify(value)});
    input.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  })()`)
const settle = () =>
  evaluate(
    'Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))'
  ).then(() => new Promise((r) => setTimeout(r, 300)))
const axe = readFileSync(resolve(root, 'web/node_modules/axe-core/axe.min.js'), 'utf8')
const audit = async (name) => {
  await settle()
  await evaluate(axe)
  const result = await evaluate(`(async () => ({
    overflow: document.documentElement.scrollWidth > innerWidth + 1,
    violations: (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa'] } }))
      .violations.map(v => ({ id: v.id, nodes: v.nodes.slice(0, 3).map(n => ({ target: n.target, summary: n.failureSummary })) })),
  }))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(shot.data, 'base64'))
  reports.push({ name, ...result, errors })
  console.log(name, JSON.stringify({ overflow: result.overflow, violations: result.violations.length, errors: errors.length }))
  errors = []
}
const capture = async (file, selector) => {
  await settle()
  let clip
  if (selector) {
    const rect = await evaluate(`(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height } })()`)
    const pad = 16
    clip = { x: Math.max(0, rect.x - pad), y: Math.max(0, rect.y - pad), width: rect.width + pad * 2, height: rect.height + pad * 2, scale: 1 }
  }
  const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false, ...(clip ? { clip } : {}) })
  const path = resolve(root, 'docs/public/screenshots', file)
  writeFileSync(path, Buffer.from(shot.data, 'base64'))
  console.log('published', path)
}
const click = (selector, text) =>
  evaluate(`[...document.querySelectorAll(${JSON.stringify(selector)})].find(el => el.textContent.trim() === ${JSON.stringify(text)}).click()`)

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: `${target}/api/*` }, { urlPattern: `${target}/hub*` }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const response = fixture.handle(request.url, request.method, request.postData || '')
    if (response.unknown) unknown.add(response.unknown)
    await cdp.send('Fetch.fulfillRequest', {
      requestId,
      responseCode: response.status || 200,
      responseHeaders: [{ name: 'Content-Type', value: 'application/json' }],
      body: Buffer.from(JSON.stringify(response.body)).toString('base64'),
    })
  })

  const gemini = fixture.builtins.find((provider) => provider.key === 'gemini').examples[0]
  const section = '[data-ai-chat-links]'
  for (const [name, width, height, scheme] of [
    ['desktop', 1440, 1000, 'dark'],
    ['compact', 320, 568, 'dark'],
    ['mobile-light', 390, 844, 'light'],
  ]) {
    // Published documentation images are captured at 2x so they stay sharp on
    // high-density displays; audits are unaffected by the pixel ratio.
    const deviceScaleFactor = publish && name === 'desktop' ? 2 : 1
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor, mobile: width < 500 })
    const { identifier } = await cdp.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `localStorage.setItem('language', JSON.stringify('en-US'));
        localStorage.setItem('mantine-color-scheme-value', '${scheme}');
        localStorage.setItem('challenge-explorer-view', JSON.stringify('list'));
        localStorage.setItem('rsctf-player-guide:${fixture.profile.userId}', JSON.stringify({ interactiveEnabled: false, completedVersion: 5, seenFeatures: [] }));`,
    })

    // Player card: saved links, a valid new link, then a rejected lookalike.
    await cdp.send('Page.navigate', { url: `${target}/games/901/challenges#9002` })
    await waitFor(`document.querySelector('${section}')`)
    await evaluate(`[...document.querySelectorAll('${section} button')].find(b => b.getAttribute('aria-expanded') === 'false')?.click()`)
    await waitFor(`document.querySelector('${section} input')`)
    await typeInto(`${section} input`, gemini)
    await waitFor(`document.querySelector('[data-ai-chat-status]')?.textContent.includes('Gemini')`)
    await evaluate(`document.querySelector('${section}').scrollIntoView({ block: 'center' })`)
    await audit(`${name}-player-valid`)
    if (publish && name === 'desktop') await capture('ai-chat-links-player.png', '[data-challenge-detail], [role="dialog"]')
    await typeInto(`${section} input`, 'https://chatgpt.com.evil.test/share/abcdefgh1234')
    await waitFor(`document.querySelector('${section} input').getAttribute('aria-invalid') === 'true'`)
    await audit(`${name}-player-rejected`)

    // A King of the Hill challenge never offers the section.
    await cdp.send('Page.navigate', { url: `${target}/games/901/challenges#9006` })
    await waitFor(`document.body.innerText.includes('Crown Hill')`)
    await new Promise((r) => setTimeout(r, 1200))
    assert.equal(await evaluate(`Boolean(document.querySelector('${section}'))`), false, 'KotH has no AI chat links')

    // Admin provider registry with a disabled built-in and a custom regex.
    await cdp.send('Page.navigate', { url: `${target}/admin/settings?section=ai_links` })
    await waitFor(`document.body.innerText.includes('Built-in providers') && document.body.innerText.includes('Microsoft Copilot')`)
    await typeInto('input[placeholder^="https://"]', 'https://copilot.microsoft.com/shares/AbCdEf123456')
    await waitFor(`document.body.innerText.includes('Matches Microsoft Copilot')`)
    await audit(`${name}-providers`)
    if (publish && name === 'desktop') {
      await evaluate('window.scrollTo(0, 0)')
      await capture('ai-chat-links-providers.png')
    }

    // Monitor review list.
    await cdp.send('Page.navigate', { url: `${target}/games/901/monitor/ai-chats` })
    await waitFor(`document.body.innerText.includes('Provider now blocked') && document.body.innerText.includes('Byte Bandits')`)
    await audit(`${name}-monitor`)
    if (publish && name === 'desktop') await capture('ai-chat-links-monitor.png')
    await cdp.send('Page.removeScriptToEvaluateOnNewDocument', { identifier })
  }

  // The section saves through the contract and never blocks closing.
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false })
  await cdp.send('Page.navigate', { url: `${target}/games/901/challenges#9002` })
  await waitFor(`document.querySelector('${section}')`)
  await evaluate(`[...document.querySelectorAll('${section} button')].find(b => b.getAttribute('aria-expanded') === 'false')?.click()`)
  await waitFor(`document.querySelector('${section} input')`)
  await typeInto(`${section} input`, gemini)
  await waitFor(`document.querySelector('[data-ai-chat-status]')?.textContent.includes('Gemini')`)
  await click(`${section} button`, 'Add')
  await click(`${section} button`, 'Save links')
  await waitFor(`${JSON.stringify(fixture.writes)}.length >= 0 && document.body.innerText.includes('Gemini')`)
  const save = fixture.writes.find((write) => write.method === 'PUT' && write.path.endsWith('/ai-chats'))
  assert.ok(save, 'Save issued a PUT')
  const body = JSON.parse(save.body)
  assert.equal(body.expectedRevision, 2)
  assert.deepEqual(body.links, [...fixture.state.links.map((link) => link.url), gemini])

  // The KotH page's own engine reads are irrelevant to this feature.
  const ignored = (path) =>
    path.startsWith('/api/game/901/challenges/') || path.startsWith('/api/game/901/ad/') || path === '/api/game/901/writeup'
  assert.deepEqual([...unknown].filter((path) => !ignored(path)), [])
  assert.deepEqual(reports.filter((report) => report.overflow || report.violations.length || report.errors.length), [])
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ target, publish, reports, unknown: [...unknown], writes: fixture.writes }, null, 2))
  await close()
}
