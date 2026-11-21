import { expect, test } from '@playwright/test'
import { resolve } from 'node:path'
import JSZip from 'jszip'
import { serveAccountBuild } from '../../tests/visual/account-access-fixtures.mjs'
import { fixture } from '../../tests/visual/admin-navigation-fixtures.mjs'
import { fixture as operations } from '../../tests/visual/ad-operations-fixtures.mjs'
import { createCompetitionFixture } from '../../tests/visual/competition-fixtures.mjs'
import { repositoryRoot } from '../../tests/visual/route-catalog.mjs'

let server
test.beforeAll(async () => { server = await serveAccountBuild(resolve(repositoryRoot, 'web/build')) })
test.afterAll(async () => { await server?.close() })

test('team opens private BYOC source history and pages back to the legacy agent', async ({ page }) => {
  const game = createCompetitionFixture()
  game.challenges[0].type = 'AttackDefense'
  Object.assign(game.responses['/api/game/901/challenges/9001'], { type: 'AttackDefense', adSelfHosted: true })
  const reads = []
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    const path = url.pathname.toLowerCase()
    if (!path.startsWith('/api/') && !path.startsWith('/hub')) return route.continue()
    if (path === '/api/game/901/ad/state') return route.fulfill({ json: {
      flagsReady: true, currentRound: 1, services: [{ adTeamServiceId: 91, challengeId: 9001, selfHosted: true }],
    } })
    if (path === '/api/game/901/ad/byoc/source/9001') {
      const before = url.searchParams.get('before')
      reads.push(before)
      return route.fulfill({ json: { before: before ? null : 2, events: [{
        revision: before ? 1 : 2, firstSeen: Date.now(), lastSeen: Date.now(),
        status: before ? 'legacyAgent' : 'observed', changes: before ? [] : [{ path: 'src/app.py', kind: 'modified' }],
      }] } })
    }
    const result = game.fixture(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, json: result.body ?? null })
  })
  await page.goto(server.origin + '/games/901/challenges#9001')
  const open = page.getByRole('button', { name: 'Source history', exact: true })
  await expect(open).toBeVisible()
  expect(reads).toHaveLength(0)
  await open.click()
  const dialog = page.getByRole('dialog', { name: 'Source history' })
  await expect(dialog.getByText('Registered files observed')).toBeVisible()
  await dialog.locator('summary').click()
  await expect(dialog.getByText('src/app.py')).toBeVisible()
  await dialog.getByRole('button', { name: 'Older reports' }).click()
  await expect(dialog.getByText('Agent version does not report source')).toBeVisible()
  expect(reads).toContain('2')
})

test('admin opens the selected team’s BYOC reports from the operations table', async ({ page }) => {
  const reads = []
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    if (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/hub')) return route.continue()
    if (url.pathname.endsWith('/ByocSource')) {
      reads.push(url.pathname)
      return route.fulfill({ json: { events: [], before: null } })
    }
    const result = operations(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, contentType: 'application/json', body: JSON.stringify(result.body ?? null) })
  })
  await page.goto(server.origin + '/admin/games/19/adops')
  const open = page.locator('[data-ad-team="2"]').getByRole('button', { name: 'Source history' })
  await expect(open).toBeVisible()
  expect(reads).toEqual([])
  await open.click()
  const dialog = page.getByRole('dialog', { name: 'Source history' })
  await expect(dialog.getByText('No agent reports received yet.')).toBeVisible()
  expect(reads).toEqual(['/api/ad/admin/19/Services/11/ByocSource'])
})

test('organizer registers a starter and sees the original after the event starts', async ({ page }) => {
  const challenge = '/api/edit/games/19/challenges/74'
  const zip = new JSZip()
  zip.file('Dockerfile', 'FROM scratch\n')
  const bytes = await zip.generateAsync({ type: 'nodebuffer' })
  const file = { name: 'starter.zip', mimeType: 'application/zip', buffer: bytes }
  let starter = null
  let started = false
  let byoc = false
  const writes = []
  const errors = []
  page.on('pageerror', error => errors.push(error.message))
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    if (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/hub')) return route.continue()
    let { body = null, status = 200 } = fixture(url.pathname, req.method()) ?? {}
    if (url.pathname === challenge) body = { ...body, type: 'AttackDefense', adSelfHosted: byoc }
    if (url.pathname === '/api/edit/games/19') {
      body = { ...body, start: Date.now() + (started ? -1 : 1) * 3600_000, end: Date.now() + 7200_000 }
    }
    if (url.pathname === challenge + '/patch-starter') {
      if (req.method() === 'PUT') {
        writes.push({ expected: req.headers()['x-rsctf-expected-tree'], type: req.headers()['content-type'], body: req.postDataBuffer() })
        starter = { treeId: 'a'.repeat(64), archiveBytes: bytes.length }
      }
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(starter) })
    }
    return route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
  })
  await page.goto(server.origin + '/admin/games/19/challenges/74')
  const panel = page.locator('[data-ad-patch-starter]')
  const input = panel.locator('input[type=file]')
  await expect(panel.getByRole('button', { name: 'Starter ZIP for team patches' })).toBeEnabled()
  await input.setInputFiles(file)
  await expect(panel.getByRole('button', { name: 'Clear selected starter ZIP' })).toBeVisible()
  expect(writes).toHaveLength(0)
  await panel.getByRole('button', { name: 'Save starter ZIP' }).click()
  await expect(panel.getByRole('status')).toContainText('aaaaaaaaaaaa')
  await input.setInputFiles(file)
  await panel.getByRole('button', { name: 'Replace starter ZIP' }).click()
  await expect.poll(() => writes.length).toBe(2)
  expect(writes.map(r => r.expected)).toEqual([undefined, starter.treeId])
  for (const write of writes) {
    expect(write.type).toBe('application/zip')
    expect(write.body.equals(bytes)).toBe(true)
  }
  await page.addScriptTag({ path: resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js') })
  const violations = await panel.evaluate(async element => {
    const report = await window.axe.run(element, { runOnly: { type: 'tag', values: ['wcag2a','wcag2aa','wcag21aa'] } })
    return report.violations.map(item => item.id)
  })
  expect(violations).toEqual([])
  expect(await panel.evaluate(el => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
  started = true
  await page.reload()
  await expect(panel.getByRole('status')).toContainText('Game started')
  await expect(panel.getByRole('button', { name: 'Starter ZIP for team patches' })).toBeDisabled()
  byoc = true
  await page.reload()
  await expect(page.getByText('Self-hosted / Bring Your Own Container (BYOC)', { exact: true })).toBeVisible()
  await expect(panel).toHaveCount(0)
  expect(errors).toEqual([])
})

test('player opens a hosted challenge from the Toolkit and compares a private patch', async ({ page }) => {
  const game = createCompetitionFixture()
  const challenge = '/api/game/901/challenges/9001'
  game.challenges[0].type = 'AttackDefense'
  game.challenges[1].type = 'AttackDefense'
  Object.assign(game.responses[challenge], { type: 'AttackDefense', adSelfHosted: false })
  const base = new JSZip().file('Dockerfile', 'FROM scratch\n')
  const starter = await base.generateAsync({ type: 'nodebuffer' })
  const candidate = await base.file('src/version.txt', 'candidate').generateAsync({ type: 'nodebuffer' })
  const root = '/api/game/901/ad/challenges/9001'
  const writes = []
  const revisions = []
  let buildAttempt = null
  let builderReconnected = false
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    const path = url.pathname.toLowerCase()
    if (!path.startsWith('/api/') && !path.startsWith('/hub')) return route.continue()
    if (path === root + '/patch-starter') return route.fulfill({
      headers: { 'Content-Type': 'application/zip', 'Content-Disposition': 'attachment; filename="starter.zip"' },
      body: starter,
    })
    if (path === root + '/patch-preview') {
      if (req.method() === 'GET') return route.fulfill({ json: { treeId: 'a'.repeat(64), archiveBytes: starter.length } })
      writes.push(req.postDataBuffer())
      const base = url.searchParams.get('base')
      expect(base).toBe(writes.length > 2 ? 'starter' : 'latest')
      const same = base === 'latest' && revisions.length > 0
      return route.fulfill({ json: { starterTree: 'a'.repeat(64), candidateTree: 'b'.repeat(64), baseRevision: revisions.length,
        comparisonRevision: base === 'latest' ? revisions.length : undefined, changes: same ? [] : [{ path: 'src/version.txt', kind: 'Added' }] } })
    }
    if (path === root + '/patch-revisions') {
      if (req.method() === 'GET') return route.fulfill({ json: revisions })
      expect(req.headers()['x-rsctf-expected-revision']).toBe('0')
      expect(req.headers()['x-rsctf-expected-service']).toBe('91')
      expect(req.postDataBuffer().equals(candidate)).toBe(true)
      revisions.push({ revision: 1, commitId: '1'.repeat(64), parentId: '2'.repeat(64), treeId: 'b'.repeat(64), createdAt: 1700000000000 })
      return route.fulfill({ json: revisions[0] })
    }
    if (path === root + '/patch-revisions/1/changes') return route.fulfill({
      json: [{ path: 'src/version.txt', kind: 'Added' }],
    })
    if (path === root + '/patch-revisions/1/build') {
      expect(url.searchParams.get('service')).toBe('91')
      expect(url.searchParams.get('commit')).toBe(revisions[0].commitId)
      if (req.method() === 'POST') {
        expect(buildAttempt).toBeNull()
        buildAttempt = 'bb071404-0670-4655-94dd-726e8ea59254'
        return route.fulfill({ json: { available: true, status: 'Queued', attempt: buildAttempt } })
      }
      if (buildAttempt && !builderReconnected) {
        builderReconnected = true
        return route.fulfill({ json: { available: false, status: 'Building', attempt: buildAttempt } })
      }
      return route.fulfill({ json: { available: true, status: buildAttempt ? 'Ready' : null, attempt: buildAttempt } })
    }
    if (path === root + '/patch-revisions/1/source.zip') {
      expect(url.searchParams.get('commit')).toBe(revisions[0].commitId)
      return route.fulfill({ contentType: 'application/zip', body: candidate })
    }
    if (path === '/api/game/901/ad/state') return route.fulfill({ json: {
      currentRound: 1, epochTicks: 12, flagsReady: true, flagDeliveryFailures: 0, scoringPaused: false,
      services: [
        { adTeamServiceId: 91, challengeId: 9001, deliveryState: 'Managed', canReset: false, snapshotAvailable: false },
        { adTeamServiceId: 93, challengeId: game.challenges[1].id, selfHosted: true, deliveryState: 'ByocHealthy' },
        { adTeamServiceId: 92, challengeId: 98765, challengeTitle: 'Hidden service', deliveryState: 'Managed' },
      ],
    } })
    if (path === '/api/game/901/ad/ssh/key') return route.fulfill({ json: { exists: false } })
    const result = game.fixture(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, json: result.body ?? null })
  })
  await page.goto(server.origin + '/games/901/challenges')
  await page.getByRole('button', { name: 'A&D Toolkit' }).click()
  const toolkit = page.getByRole('dialog')
  await toolkit.getByRole('button', { name: 'Patch your service' }).click()
  await expect(toolkit.getByRole('link', { name: 'Hidden service' })).toHaveCount(0)
  await expect(toolkit.getByRole('link', { name: game.challenges[1].title, exact: true })).toHaveCount(0)
  await toolkit.getByRole('link', { name: game.challenges[0].title, exact: true }).click()
  await expect(page).toHaveURL(/#9001-/)
  const panel = page.locator('[data-ad-patch-preview]')
  await expect(panel).toBeVisible()
  const downloaded = page.waitForEvent('download')
  await panel.getByRole('button', { name: 'Download starter ZIP' }).click()
  expect((await downloaded).suggestedFilename()).toBe('starter.zip')
  const input = panel.locator('input[type=file]')
  await input.setInputFiles({ name: 'candidate.zip', mimeType: 'application/zip', buffer: candidate })
  await expect(panel.getByRole('button', { name: 'Clear selected patch ZIP' })).toBeVisible()
  expect(writes).toHaveLength(0)
  await panel.getByRole('button', { name: 'Compare files' }).click()
  await expect(panel.getByRole('status')).toContainText('Files changed: 1')
  await expect(panel.getByRole('listitem')).toHaveText('Added · src/version.txt')
  expect(writes).toHaveLength(1)
  expect(writes[0].equals(candidate)).toBe(true)
  expect(revisions).toHaveLength(0)
  await panel.getByRole('button', { name: 'Save private revision' }).click()
  await expect(panel.getByText('Revision 1 saved. Your service keeps running.')).toBeVisible()
  await panel.getByText('Team revision history').click()
  await expect(panel.getByText('111111111111')).toBeVisible()
  await panel.getByRole('button', { name: 'Changes in revision 1' }).click()
  await panel.getByRole('button', { name: 'Build revision 1' }).click()
  await expect(panel.getByText('Build queued')).toBeVisible()
  await expect(panel.getByText('Waiting for the challenge builder to reconnect.')).toBeVisible({ timeout: 25000 })
  await expect(panel.getByText(/^Candidate ready\./)).toBeVisible({ timeout: 25000 })
  await expect(panel.locator('details').getByRole('listitem')).toHaveText('Added · src/version.txt')
  const revisionDownload = page.waitForEvent('download')
  await panel.getByRole('button', { name: 'Download revision 1 ZIP' }).click()
  expect((await revisionDownload).suggestedFilename()).toBe('patch-revision-1.zip')
  await expect(panel.getByRole('button', { name: 'Save private revision' })).toHaveCount(0)
  await panel.getByRole('button', { name: 'Compare files' }).click()
  await expect(panel.getByText(/Compared with revision 1\.\s*No files changed\./)).toBeVisible()
  await panel.getByRole('combobox', { name: 'Compare with' }).selectOption('starter')
  await expect(panel.getByText(/Compared with revision 1\./)).toHaveCount(0)
  await panel.getByRole('button', { name: 'Compare files' }).click()
  await expect(panel.getByText(/Compared with the starter ZIP\.\s*Files changed: 1/)).toBeVisible()
  await page.addScriptTag({ path: resolve(repositoryRoot, 'web/node_modules/axe-core/axe.min.js') })
  expect(await panel.evaluate(async element => (await window.axe.run(element, {
    runOnly: { type: 'tag', values: ['wcag2a','wcag2aa','wcag21aa'] },
  })).violations.map(item => item.id))).toEqual([])
  expect(await panel.evaluate(el => el.scrollWidth <= el.clientWidth + 1)).toBe(true)
})
