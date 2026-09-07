// Render real submission/result flows against loopback-only browser fixtures.
// All API traffic, including POSTs, is intercepted; no live flags are submitted.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_WORKSPACE_PREVIEW || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_WORKSPACE_OUTPUT || '../visual-audit-output/flag-verdict')
mkdirSync(output, { recursive: true })
const now = Date.now()
const profile = { userId: '11111111-1111-4111-8111-111111111111', role: 'User', userName: 'Dimas' }
const game = { id: 901, title: 'Intechfest 2026', start: now - 3600000, end: now + 8000000, serverTime: now, status: 'Accepted', joined: true, divisions: [], practiceMode: false }
const challenge = { id: 9001, title: 'Tower of Babel — a challenge with a long readable title', category: 'Pwn', type: 'StaticAttachment', score: 500, solved: 2, bloods: [], disableBloodBonus: true, attempts: 0, content: 'Read the challenge files and submit your flag.', hints: [] }
const rank = { id: 7, name: 'TCP1P', rank: 7, score: 1000, solvedCount: 0, solvedChallenges: [] }
const responses = {
  '/api/account/profile': profile,
  '/api/config': { title: 'RSCTF', portMapping: 'Default', enableBrowserFingerprint: false },
  '/api/captcha': { type: 'None' },
  '/api/game/901': game,
  '/api/game/901/notices': [],
  '/api/game/901/details': { challenges: { Pwn: [challenge] }, challengeCount: 1, rank },
  '/api/game/901/details/participant': { rank },
  '/api/game/901/challenges/9001': challenge,
  '/api/game/901/challenges/9001/solvers/page': { data: [], total: 2 },
}
const browser = await launchBrowser()
const { cdp } = browser
const reports = [], requests = [], unknown = new Set(), errors = []
let answer = 'WrongAnswer', submissions = 0, statusReads = 0
const evaluate = async (expression) => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const waitFor = async (expression) => {
  for (let attempt = 0; attempt < 100; attempt++) {
    try { if (await evaluate(`Boolean(${expression})`)) return } catch (error) {
      if (!/context|navigated/i.test(error.message)) throw error
    }
    await new Promise((resolve) => setTimeout(resolve, 200))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-1800)')}`)
}
const press = async (key) => {
  const vk = { Enter: 13, Escape: 27, Tab: 9 }[key]
  for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, code: key, windowsVirtualKeyCode: vk, ...(type === 'keyDown' && key === 'Enter' ? { text: '\r', unmodifiedText: '\r' } : {}) })
}
const shot = async (name) => {
  const image = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false })
  writeFileSync(`${output}/${name}.png`, Buffer.from(image.data, 'base64'))
}

try {
  await cdp.send('Page.enable')
  await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('challenge-explorer-view', JSON.stringify('list')); localStorage.setItem('rsctf-player-guide:${profile.userId}', JSON.stringify({ interactiveEnabled: false, completedVersion: 1, seenFeatures: [] }));` })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: `${target}/api/*` }, { urlPattern: `${target}/hub*` }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname.toLowerCase()
    requests.push({ path, method: request.method })
    let value = responses[path], status = 200
    if (request.method === 'POST' && path === '/api/game/901/challenges/9001') {
      assert.ok(JSON.parse(request.postData).attemptId)
      value = ++submissions
    } else if (!['GET', 'HEAD'].includes(request.method)) {
      status = 405
      value = { status, title: 'Fixture mutation blocked' }
    } else if (path.startsWith('/api/game/901/challenges/9001/status/')) {
      statusReads++
      value = answer
    } else if (value === undefined) {
      unknown.add(path)
      value = []
    }
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: status, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(value)).toString('base64') })
  })

  for (const [name, width, height, scheme, language, reduced, view = 'list'] of [
    ['desktop', 1600, 1000, 'dark', 'en-US', false],
    ['desktop-cards', 1600, 1000, 'dark', 'en-US', false, 'cards'],
    ['compact', 320, 568, 'dark', 'en-US', false],
    ['mobile-light-id', 390, 844, 'light', 'id-ID', false],
    ['tablet-reduced', 768, 1024, 'dark', 'en-US', true],
    ['landscape', 844, 390, 'dark', 'en-US', false],
  ]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false })
    await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: reduced ? 'reduce' : 'no-preference' }] })
    const { identifier } = await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `localStorage.setItem('language', JSON.stringify('${language}')); localStorage.setItem('mantine-color-scheme-value', '${scheme}'); localStorage.setItem('challenge-explorer-view', JSON.stringify('${view}'));` })
    for (const [kind, result] of [['wrong', 'WrongAnswer'], ['success', 'Accepted']]) {
      answer = name === 'desktop' && kind === 'wrong' ? 'FlagSubmitted' : result
      const previousSubmissions = submissions
      const previousReads = statusReads
      await cdp.send('Page.navigate', { url: `${target}/games/901/challenges` })
      await waitFor(`document.querySelector('[data-challenge-row="9001"], [data-guide="challenge-card"] button')`)
      await evaluate(`document.querySelector('[data-challenge-row="9001"], [data-guide="challenge-card"] button').click()`)
      await waitFor(`document.querySelector('form[data-guide="flag-submit"] input')`)
      await evaluate(`document.querySelector('form[data-guide="flag-submit"] input').focus()`)
      await cdp.send('Input.insertText', { text: 'fixture-only-not-a-real-flag' })
      await evaluate(`Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))`)
      await evaluate(`window.retainedChallenge = {
        form: document.querySelector('form[data-guide="flag-submit"]'),
        material: document.querySelector('[data-guide="challenge-material"]'),
        panel: document.querySelector('[data-challenge-detail]') ?? document.querySelector('[role="dialog"]'),
        row: document.querySelector('[data-challenge-row="9001"], [data-guide="challenge-card"]'),
      }; window.retainedBounds = retainedChallenge.panel.getBoundingClientRect().toJSON();
      window.transitionFrames = []; window.trackVerdictFrames = true;
      requestAnimationFrame(function sample() {
        if (!window.trackVerdictFrames) return;
        const panel = retainedChallenge.panel, r = panel.getBoundingClientRect();
        const result = document.querySelector('[data-flag-verdict]')?.closest('[role="dialog"]');
        transitionFrames.push({ connected: panel.isConnected && retainedChallenge.form.isConnected && retainedChallenge.material.isConnected && retainedChallenge.row.isConnected,
          x: r.x, width: r.width, visible: getComputedStyle(panel).display !== 'none' && getComputedStyle(panel).visibility !== 'hidden',
          result: result ? { height: result.getBoundingClientRect().height, opacity: Number(getComputedStyle(result).opacity), exiting: window.verdictDismissing === true } : null });
        requestAnimationFrame(sample);
      });`)
      await evaluate(`document.querySelector('form[data-guide="flag-submit"]').requestSubmit()`)
      if (answer === 'FlagSubmitted') {
        for (let attempt = 0; attempt < 100 && statusReads === previousReads; attempt++) await new Promise((resolve) => setTimeout(resolve, 100))
        assert.ok(statusReads > previousReads)
        assert.equal(await evaluate(`!!document.querySelector('[data-flag-verdict]')`), false, 'pending is not a success or denial')
        answer = result
      }
      await waitFor(`document.querySelector('[data-flag-verdict][data-kind="${kind}"]')`)
      assert.equal(await evaluate(`retainedChallenge.form.isConnected && retainedChallenge.material.isConnected && retainedChallenge.panel.isConnected && retainedChallenge.row.isConnected`), true, 'verdict must not remove the challenge or selected card')
      assert.equal(submissions, previousSubmissions + 1)
      assert.equal(await evaluate(`document.activeElement === document.querySelector('[data-flag-verdict] [data-autofocus]')`), true)
      assert.equal(await evaluate(`getComputedStyle(document.activeElement).opacity === '1' && !document.activeElement.disabled`), true, 'actions are usable during the animation')
      const animation = await evaluate(`(() => {
        const root = document.querySelector('[data-flag-verdict]');
        return root.getAnimations({ subtree: true }).map(a => ({ state: a.playState, end: a.effect.getComputedTiming().endTime }));
      })()`)
      if (reduced) assert.deepEqual(animation, [])
      else assert.ok(animation.length > 0 && animation.every(a => a.end <= 800), 'motion has a finite sub-second budget')
      if (name === 'desktop') {
        await evaluate(`document.querySelector('[data-flag-verdict]').getAnimations({ subtree: true }).forEach(a => { a.pause(); a.currentTime = 220; })`)
        await shot(`${name}-${kind}-motion`)
        await evaluate(`document.querySelector('[data-flag-verdict]').getAnimations({ subtree: true }).forEach(a => a.finish())`)
      }
      await evaluate(`Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))`)
      await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
      const issues = await evaluate(`(async () => ({
        overflow: document.documentElement.scrollWidth > innerWidth + 1,
        dialogFits: (() => { const r = document.querySelector('[data-flag-verdict]').closest('[role="dialog"]').getBoundingClientRect(); return r.x >= 0 && r.y >= 0 && r.right <= innerWidth && r.bottom <= innerHeight; })(),
        violations: (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa'] } })).violations.map(v => ({ id: v.id, nodes: v.nodes.map(n => ({ target: n.target, summary: n.failureSummary })) }))
      }))()`)
      await shot(`${name}-${kind}`)
      reports.push({ name: `${name}-${kind}`, ...issues, animation })
      console.log(`${name}-${kind}: ${JSON.stringify(issues)}`)
      assert.equal(await evaluate(`document.querySelectorAll('[data-flag-verdict] i').length`), kind === 'success' ? 12 : 0)
      await press('Tab')
      assert.equal(await evaluate(`document.activeElement === document.querySelector('[data-flag-verdict] button')`), true, 'focus stays trapped in the result')
      await press('Tab')
      assert.equal(await evaluate(`document.activeElement.matches('[data-autofocus]')`), true)
      await evaluate('window.verdictDismissing = true')
      if (kind === 'wrong') await press('Escape')
      else await press('Enter')
      await waitFor(`!document.querySelector('[data-flag-verdict]')`)
      const continuity = await evaluate(`(() => {
        window.trackVerdictFrames = false;
        const resultFrames = transitionFrames.map(f => f.result).filter(Boolean);
        return { frames: transitionFrames.length,
          before: retainedBounds,
          deviations: transitionFrames.filter(f => !f.connected || !f.visible || Math.abs(f.x - retainedBounds.x) > 1 || Math.abs(f.width - retainedBounds.width) > 1).slice(0, 4),
          stable: transitionFrames.every(f => f.connected && f.visible && Math.abs(f.x - retainedBounds.x) <= 1 && Math.abs(f.width - retainedBounds.width) <= 1),
          resultStable: resultFrames.length > 0 && Math.max(...resultFrames.map(f => f.height)) - Math.min(...resultFrames.map(f => f.height)) <= 1,
          enters: resultFrames.some(f => !f.exiting && f.opacity > 0 && f.opacity < 1),
          exits: resultFrames.some(f => f.exiting && f.opacity > 0 && f.opacity < 1),
          retained: retainedChallenge.form === document.querySelector('form[data-guide="flag-submit"]') && retainedChallenge.material === document.querySelector('[data-guide="challenge-material"]') };
      })()`)
      reports.at(-1).continuity = continuity
      assert.ok(continuity.frames > 0 && continuity.stable && continuity.retained, `${name}-${kind}: challenge must stay visible and retain its geometry throughout both transitions`)
      assert.ok(continuity.resultStable, 'result content must not collapse during dismissal')
      if (!reduced) assert.ok(continuity.enters && continuity.exits, 'result must animate both its entrance and exit')
      if (kind === 'wrong') await waitFor(`document.activeElement === document.querySelector('form[data-guide="flag-submit"] input')`)
      else await waitFor(`document.activeElement !== document.body && (document.querySelector('[data-challenge-detail]') ?? document.querySelector('[role="dialog"]'))?.contains(document.activeElement)`)
      assert.equal(submissions, previousSubmissions + 1, 'dismissal must not resubmit the flag')
    }
    await cdp.send('Page.removeScriptToEvaluateOnNewDocument', { identifier })
  }
} finally {
  writeFileSync(`${output}/report.json`, JSON.stringify({ reports, unknown: [...unknown], errors, submissions, requests }, null, 2))
  await browser.close()
}
assert.equal(reports.length, 12)
assert.deepEqual([...unknown], [])
assert.deepEqual(errors, [])
assert.deepEqual(reports.filter(r => r.overflow || !r.dialogFits || r.violations.length), [])
console.log(`PASS: ${reports.length} verdict flows, keyboard return, reduced motion, and pending-result boundary`)
