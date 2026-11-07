// Exercise real Toolkit controls against a strict, disposable SSH wire contract.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { chromium, expect } from '../../web/node_modules/@playwright/test/index.mjs'
import { createCompetitionFixture } from './competition-fixtures.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { resolveChromium } from './cdp.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const output = resolve(repositoryRoot, 'visual-audit-output/toolkit-ssh')
mkdirSync(output, { recursive: true })
const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const browser = await chromium.launch({ executablePath: resolveChromium(), args: [
  '--no-sandbox', '--no-proxy-server', '--host-resolver-rules=MAP rsctf.test 127.0.0.1',
] }).catch(async error => { await server.close(); throw error })
const publicKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA fixture@example.invalid'
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
try {
  for (const insecure of [false, true]) {
    const page = await browser.newPage({ viewport: { width: insecure ? 390 : 1280, height: 900 } })
    const errors = [], writes = []
    page.on('pageerror', error => errors.push(error.message))
    const data = createCompetitionFixture()
    data.responses['/api/game/901/details'].challenges.Web[0].type = 'AttackDefense'
    data.responses['/api/game/901/ad/token'] = { exists: false, canManage: false, revision: 1 }
    data.responses['/api/game/901/ad/state'] = {
      currentRound: 7, startRound: 1, epochTicks: 8, roundEndsAt: Date.now() + 60000,
      flagsReady: true, flagDeliveryFailures: 0, scoringPaused: false, services: [],
    }
    let key = { exists: false, revision: 7, algorithm: 'ssh-ed25519', fingerprint: '', platformGenerated: false }
    await page.addInitScript(userId => {
      localStorage.setItem('language', JSON.stringify('en-US'))
      localStorage.setItem('rsctf-player-guide:' + userId, JSON.stringify({ interactiveEnabled: false, completedVersion: 5, seenFeatures: [] }))
    }, data.profile.userId)
    await page.route('**/hub**', route => route.fulfill({ json: {} }))
    await page.route('**/api/**', async route => {
      const request = route.request(), path = new URL(request.url()).pathname.toLowerCase()
      if (path.startsWith('/api/game/901/ad/ssh/key')) {
        if (request.method() === 'GET') return route.fulfill({ json: key })
        const body = request.postDataJSON()
        const upload = request.method() === 'POST' && path.endsWith('/key')
        const fields = upload ? ['expectedRevision', 'operationId', 'publicKey'] : ['expectedRevision', 'operationId']
        const valid = JSON.stringify(Object.keys(body).sort()) === JSON.stringify(fields)
        writes.push({ method: request.method(), path, body, valid })
        if (!valid) return route.fulfill({ status: 422, json: { title: 'Unknown credential fields' } })
        assert.match(body.operationId, uuid)
        assert.equal(body.expectedRevision, key.revision)
        // An ambiguous failure must retain the same ID/revision across reload.
        if (writes.length === 1) return route.fulfill({ status: 503, json: { title: 'Fixture retry' } })
        key = { ...key, exists: request.method() !== 'DELETE', revision: key.revision + 1,
          fingerprint: 'SHA256:toolkit-fixture', platformGenerated: path.endsWith('/generate') }
        return route.fulfill({ json: { ...key, operationId: body.operationId,
          ...(key.platformGenerated ? { publicKey, privateKey: 'FIXTURE PRIVATE KEY — NOT A CREDENTIAL' } : {}),
        } })
      }
      const result = data.fixture(request.url(), request.method())
      await route.fulfill({ status: result.status || 200, json: result.body })
    })
    const origin = insecure ? server.origin.replace('127.0.0.1', 'rsctf.test') : server.origin
    const openSsh = async () => {
      await page.getByRole('button', { name: 'A&D Toolkit', exact: true }).click()
      await page.getByRole('button', { name: 'Shell access (SSH)' }).click()
      await expect(page.getByRole('textbox', { name: 'SSH public key' })).toBeVisible()
    }
    await page.goto(origin + '/games/901/challenges')
    assert.deepEqual(await page.evaluate(() => ({ secure: isSecureContext, subtle: !!crypto.subtle, randomUUID: !!crypto.randomUUID })),
      { secure: !insecure, subtle: !insecure, randomUUID: !insecure })
    await openSsh()
    await page.getByRole('textbox', { name: 'SSH public key' }).fill(publicKey)
    await page.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect.poll(() => writes.length).toBe(1)
    assert.equal(writes[0].valid, true, `Toolkit leaked local fields: ${Object.keys(writes[0].body)}`)
    await expect(page.getByRole('button', { name: 'Upload', exact: true })).toBeEnabled()
    await page.reload()
    await openSsh()
    await page.getByRole('textbox', { name: 'SSH public key' }).fill(publicKey)
    await page.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(page.getByText('SHA256:toolkit-fixture', { exact: true })).toBeVisible()
    assert.deepEqual(writes[1].body, writes[0].body)
    assert.equal(key.revision, 8)
    await page.getByRole('button', { name: 'Revoke', exact: true }).click()
    await expect(page.getByText('No key yet', { exact: true })).toBeVisible()
    assert.equal(key.revision, 9)
    await page.getByRole('tab', { name: 'Generate keypair', exact: true }).click()
    await page.getByRole('button', { name: 'Generate ed25519 keypair', exact: true }).click()
    await expect(page.getByText('FIXTURE PRIVATE KEY — NOT A CREDENTIAL', { exact: true })).toBeVisible()
    assert.equal(key.revision, 10)
    assert.equal(writes.length, 4)
    assert.ok(writes.every(write => write.valid))
    assert.equal(new Set(writes.map(write => write.body.operationId)).size, 3)
    await page.getByRole('button', { name: 'Confirm', exact: true }).click()
    await expect(page.getByText('FIXTURE PRIVATE KEY — NOT A CREDENTIAL', { exact: true })).toHaveCount(0)
    await page.addScriptTag({ content: readFileSync(resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js'), 'utf8') })
    const violations = await page.evaluate(async () => (await axe.run(document.querySelector('[role="dialog"]'), {
      runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa'] },
    })).violations.map(item => ({ id: item.id, nodes: item.nodes.map(node => node.html) })))
    assert.deepEqual(violations, [])
    for (const width of insecure ? [320, 390, 768] : [1280, 1920]) {
      await page.setViewportSize({ width, height: 900 })
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)).toBe(true)
      await page.screenshot({ path: resolve(output, `${insecure ? 'http' : 'secure'}-${width}.png`) })
    }
    assert.deepEqual(errors, [])
    await page.close()
  }
  console.log('Toolkit SSH: upload/reload retry/revoke/generate, strict payloads, revision ownership, HTTP crypto fallback, Axe and responsive checks passed.')
} finally {
  await browser.close()
  await server.close()
}
