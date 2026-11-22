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

test('closed hosted challenge shows paged file statuses on a narrow screen', async ({ page }) => {
  const game = createCompetitionFixture()
  game.responses['/api/game/901'].end = Date.now() - 60000
  Object.assign(game.responses['/api/game/901/challenges/9001'], { type: 'AttackDefense', adSelfHosted: false })
  await page.setViewportSize({ width: 320, height: 800 })
  await page.route('**/api/**', async (route) => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.pathname.endsWith('/patch-archive')) {
      const next = url.searchParams.get('afterTeam') === '2' && url.searchParams.get('afterRevision') === '3'
      return route.fulfill({ json: { hasMore: !next, revisions: [{
        teamId: next ? 7 : 2, teamName: next ? 'Finches' : 'Owls', revision: next ? 1 : 3,
        changes: [{ path: 'src/example.py', kind: next ? 'Deleted' : 'Modified' }],
      }] } })
    }
    const result = game.fixture(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, json: result.body ?? null })
  })
  await page.goto(server.origin + '/games/901/challenges#9001')
  await expect(page.getByText('Owls · Revision 3')).toBeVisible()
  await page.getByRole('button', { name: 'Next page' }).click()
  await expect(page.getByText('Finches · Revision 1')).toBeVisible()
  await expect(page.getByRole('listitem').filter({ hasText: 'src/example.py · Deleted' })).toBeVisible()
  await page.getByRole('button', { name: 'First page' }).click()
  await expect(page.getByText('Owls · Revision 3')).toBeVisible()
})

test('team resets its hosted service and sees the completed result without crypto.randomUUID', async ({ page }) => {
  const game = createCompetitionFixture()
  game.challenges[0].type = 'AttackDefense'
  Object.assign(game.responses['/api/game/901/challenges/9001'], { type: 'AttackDefense', adSelfHosted: false })
  const sent = []
  let polls = 0
  await page.addInitScript(() => Object.defineProperty(crypto, 'randomUUID', { value: undefined }))
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    const path = url.pathname.toLowerCase()
    if (!path.startsWith('/api/') && !path.startsWith('/hub')) return route.continue()
    if (path === '/api/game/901/ad/state') return route.fulfill({ json: {
      flagsReady: true, currentRound: 1, services: [{ adTeamServiceId: 91, challengeId: 9001,
        canReset: true, selfHosted: false, containerIp: '127.0.0.1', containerPort: 8080 }],
    } })
    if (path === '/api/game/901/ad/services/91/reset' && req.method() === 'POST') {
      sent.push(req.headers()['idempotency-key'])
      return route.fulfill({ status: 202, json: { id: 'reset-job', status: 'Queued' } })
    }
    if (path === '/api/game/901/ad/resetjobs/reset-job') {
      polls++
      return route.fulfill({ json: { id: 'reset-job', status: 'Succeeded', result: { reset: polls === 1 } } })
    }
    const result = game.fixture(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, json: result.body ?? null })
  })
  await page.goto(server.origin + '/games/901/challenges#9001')
  const reset = page.getByRole('button', { name: 'Reset', exact: true })
  await reset.click()
  await expect(page.getByText('Your service has been rebuilt.')).toBeVisible()
  // The second request belongs to a fresh visit after a different operation.
  await page.reload()
  await reset.click()
  await expect(page.getByText('Another operation already replaced your service.')).toBeVisible()
  expect(polls).toBe(2)
  expect(new Set(sent).size).toBe(2)
  for (const id of sent) expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
})

test('team opens private BYOC source history and pages back to the legacy agent', async ({ page }) => {
  const game = createCompetitionFixture()
  await page.clock.install({ time: game.now + 600_000 })
  game.challenges[0].type = 'AttackDefense'
  Object.assign(game.responses['/api/game/901/challenges/9001'], { type: 'AttackDefense', adSelfHosted: true })
  const reads = []
  let lastSeen = game.now - 150_000
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    const path = url.pathname.toLowerCase()
    if (!path.startsWith('/api/') && !path.startsWith('/hub')) return route.continue()
    if (path === '/api/game/901/ad/state') return route.fulfill({ json: {
      flagsReady: true, currentRound: 1, services: [{ adTeamServiceId: 91, challengeId: 9001, selfHosted: true,
        deliveryState: 'ByocConnecting', containerIp: '', containerPort: 0 }],
    } })
    if (path === '/api/game/901/ad/byoc/setup/9001' || path === '/api/game/901/ad/byoc/compose/9001') {
      return route.fulfill({ contentType: 'application/octet-stream', body: 'fixture download',
        headers: { 'Content-Disposition': 'attachment; filename="challenge-named-attachment"' } })
    }
    if (path === '/api/game/901/ad/byoc/source/9001') {
      const before = url.searchParams.get('before')
      reads.push(before)
      return route.fulfill({ json: { before: before ? null : 2, events: [{
        revision: before ? 1 : 2, firstSeen: lastSeen, lastSeen,
        status: before ? 'legacyAgent' : 'observed', changes: before ? [] : [{ path: 'src/app.py', kind: 'modified' }],
      }] } })
    }
    const result = game.fixture(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, json: result.body ?? null })
  })
  await page.goto(server.origin + '/games/901/challenges#9001')
  await expect(page.getByText(/run sh setup-901-9001\.sh/)).toBeVisible()
  for (const [button, filename] of [['Download setup script', 'setup-901-9001.sh'], ['Bring your own service', 'compose.yaml']]) {
    const downloaded = page.waitForEvent('download')
    await page.getByRole('button', { name: button, exact: true }).click()
    expect((await downloaded).suggestedFilename()).toBe(filename)
  }
  const open = page.getByRole('button', { name: 'Source history', exact: true })
  await expect(open).toBeVisible()
  expect(reads).toHaveLength(0)
  await open.click()
  const dialog = page.getByRole('dialog', { name: 'Source history' })
  await expect(dialog.getByText('Watching challenge source')).toBeVisible()
  const stale = dialog.getByText('Reports are out of date.', { exact: false })
  await expect(stale).toHaveCount(0)
  await page.clock.fastForward(40_000)
  await expect(stale).toBeVisible()
  expect(reads).toEqual([null])
  await dialog.locator('summary').click()
  await expect(dialog.getByText('src/app.py')).toBeVisible()
  await dialog.getByRole('button', { name: 'Older reports' }).click()
  await expect(dialog.getByText('Agent version does not report source')).toBeVisible()
  await expect(stale).toHaveCount(0)
  expect(reads).toContain('2')
  lastSeen = game.now + 40_000
  await dialog.getByRole('button', { name: 'Latest reports' }).click()
  await expect(dialog.getByText('Watching challenge source')).toBeVisible()
  await expect(stale).toHaveCount(0)
})

test('team can see sampled SSH edits to the same registered file', async ({ page }) => {
  const game = createCompetitionFixture()
  game.challenges[0].type = 'AttackDefense'
  Object.assign(game.responses['/api/game/901/challenges/9001'], { type: 'AttackDefense', adSelfHosted: false })
  const reads = []
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    const path = url.pathname.toLowerCase()
    if (!path.startsWith('/api/') && !path.startsWith('/hub')) return route.continue()
    if (path === '/api/game/901/ad/state') return route.fulfill({ json: {
      flagsReady: true, currentRound: 1, services: [{ adTeamServiceId: 91, challengeId: 9001,
        selfHosted: false, containerIp: '127.0.0.1', containerPort: 8080 }],
    } })
    if (path === '/api/game/901/ad/challenges/9001/ssh-source') {
      reads.push(path)
      const at = Date.now() - 3600000
      return route.fulfill({ json: { before: null, events: [2, 1].map(revision => ({
        revision, firstSeen: at + revision, lastSeen: at + revision, status: 'observed',
        recoveryId: revision === 2 ? '4e13e9c5-7825-485d-bc88-227666c89302' : undefined,
        changes: [{ path: 'exercise.py', kind: revision === 1 ? 'present' : 'modified' }],
      })) } })
    }
    if (path === '/api/game/901/ad/challenges/9001/ssh-source/2/4e13e9c5-7825-485d-bc88-227666c89302/starter.zip') {
      return route.fulfill({ contentType: 'application/zip', body: 'private recovery fixture' })
    }
    const result = game.fixture(url.pathname + url.search, req.method())
    return route.fulfill({ status: result.status ?? 200, json: result.body ?? null })
  })
  await page.goto(server.origin + '/games/901/challenges#9001')
  const open = page.getByRole('button', { name: 'SSH edit history', exact: true })
  await expect(open).toBeVisible()
  expect(reads).toHaveLength(0)
  await open.click()
  const dialog = page.getByRole('dialog', { name: 'SSH edit history' })
  await expect(dialog.getByText('SSH source observed')).toHaveCount(2)
  await expect(dialog.getByRole('button', { name: 'Download starter with SSH edits (revision 2)' })).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Download starter with SSH edits (revision 1)' })).toHaveCount(0)
  const downloaded = page.waitForEvent('download')
  await dialog.getByRole('button', { name: 'Download starter with SSH edits (revision 2)' }).click()
  expect((await downloaded).suggestedFilename()).toBe('ssh-revision-2.zip')
  await dialog.locator('summary').first().click()
  await expect(dialog.getByText('Modified', { exact: false }).first()).toBeVisible()
  await expect(dialog.getByText('Reports are out of date.', { exact: false })).toHaveCount(0)
  expect(reads).toHaveLength(1)
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

test('organizer opts into delayed deployment counts before the game', async ({ page }) => {
  let policy = { enabled: false, revision: 0, editable: true }
  const writes = []
  await page.route('**/*', async route => {
    const request = route.request()
    const url = new URL(request.url())
    if (url.origin !== server.origin) return route.abort()
    if (!url.pathname.startsWith('/api/') && !url.pathname.startsWith('/hub')) return route.continue()
    if (url.pathname.toLowerCase() === '/api/edit/games/19/ad/patch-activity') {
      if (request.method() === 'PUT') {
        writes.push(request.postDataJSON())
        policy = { enabled: true, revision: 1, editable: true }
      }
      return route.fulfill({ json: policy })
    }
    const { body = null, status = 200 } = fixture(url.pathname, request.method()) ?? {}
    return route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })
  })
  await page.goto(server.origin + '/admin/games/19/info#section=ad')
  const setting = page.getByRole('switch', { name: 'Share patch activity' })
  await expect(setting).toBeEnabled()
  await expect(setting).not.toBeChecked()
  await setting.click()
  await expect(setting).toBeChecked()
  expect(writes).toEqual([{ enabled: true, expectedRevision: 0 }])
  policy.editable = false
  await page.reload()
  await expect(setting).toBeChecked()
  await expect(setting).toBeDisabled()
})

test('organizer registers a starter and sees the original after the event starts', async ({ page }) => {
  const challenge = '/api/edit/games/19/challenges/74'
  const zip = new JSZip()
  zip.file('Dockerfile', 'FROM scratch\n')
  const sshSources = [{ sourcePath: 'src/game.py', containerPath: '/app/game.py' }]
  zip.file('src/game.py', 'print(1)\n')
  zip.file('.rsctf-ssh.json', JSON.stringify(sshSources))
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
        starter = { treeId: 'a'.repeat(64), archiveBytes: bytes.length, runtimeImageRef: null, awaitingImageBuild: true, sshSources }
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
  await panel.getByText('Registered SSH source files (1)').click()
  await expect(panel.getByText('/app/game.py', { exact: true })).toBeVisible()
  await input.setInputFiles(file)
  await panel.getByRole('button', { name: 'Replace starter ZIP' }).click()
  await expect.poll(() => writes.length).toBe(2)
  await expect(panel.getByText('Build the challenge image before the game starts')).toBeVisible()
  const image = 'private.example/event/service@sha256:' + 'b'.repeat(64)
  starter = { ...starter, runtimeImageRef: image, awaitingImageBuild: false }
  await expect(panel.getByText('Original image · ' + image)).toBeVisible({ timeout: 30000 })
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
  await expect(panel.getByText('Original image · ' + image)).toBeVisible()
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
  let publishedRounds = []
  let staged = false
  let deployed = false
  await page.route('**/*', async route => {
    const req = route.request()
    const url = new URL(req.url())
    if (url.origin !== server.origin) return route.abort()
    const path = url.pathname.toLowerCase()
    if (!path.startsWith('/api/') && !path.startsWith('/hub')) return route.continue()
    if (path === root + '/patch-activity') return route.fulfill({ json: publishedRounds })
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
    if (path === root + '/patch-revisions/1/ssh-comparison') {
      expect(url.searchParams.get('service')).toBe('91')
      expect(url.searchParams.get('commit')).toBe(revisions[0].commitId)
      return route.fulfill({ json: { report: { revision: 8, lastSeen: 1700000000000, trackedFiles: 2,
        changes: [{ path: 'src/ssh-only.py', kind: 'Deleted' }] } } })
    }
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
      return route.fulfill({ json: { available: true, status: buildAttempt ? 'Ready' : null, attempt: buildAttempt, staged,
        canActivate: staged && !deployed, active: deployed } })
    }
    if (path === root + '/patch-revisions/1/activate') {
      expect(req.method()).toBe('POST')
      expect(req.postDataJSON()).toEqual({ service: 91, attempt: buildAttempt })
      expect(req.headers()['idempotency-key']).toMatch(/^[0-9a-f-]{36}$/)
      return route.fulfill({ status: 202, json: { id: 'deployment', status: 'Queued' } })
    }
    if (path === '/api/game/901/ad/resetjobs/deployment') {
      deployed = true
      return route.fulfill({ json: { id: 'deployment', status: 'Succeeded' } })
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
  const activity = panel.getByText('Patch activity across teams', { exact: true })
  await activity.click()
  await expect(panel.getByText('No rounds are available yet.')).toBeVisible()
  publishedRounds = [{ round: 3, deployments: 2 }]
  await panel.getByRole('button', { name: 'Refresh activity' }).click()
  await expect(panel.getByText('Round 3 · 2 deployed')).toBeVisible()
  await activity.click()
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
  await panel.getByRole('button', { name: 'Compare ZIP with last SSH report' }).click()
  await expect(panel.getByRole('status').filter({ hasText: 'SSH report 8' })).toContainText('Differences in this ZIP: 1')
  await expect(panel.getByText('src/ssh-only.py')).toBeVisible()
  await panel.getByRole('button', { name: 'Build revision 1' }).click()
  await expect(panel.getByText('Build queued')).toBeVisible()
  await expect(panel.getByText('Waiting for the challenge builder to reconnect.')).toBeVisible({ timeout: 25000 })
  await expect(panel.getByText(/^Candidate ready\./)).toBeVisible({ timeout: 25000 })
  staged = true
  await panel.getByRole('button', { name: 'Reload build status' }).click()
  await expect(panel.getByText(/^Image verified in the registry\./)).toBeVisible()
  await panel.getByRole('button', { name: 'Deploy revision 1' }).click()
  await expect(panel.getByText('Your service is running this image.')).toBeVisible()
  await expect(panel.getByRole('button', { name: 'Deploy revision 1' })).toHaveCount(0)
  await expect(panel.locator('details').getByRole('listitem')).toHaveText([
    'Added · src/version.txt', 'Deleted · src/ssh-only.py',
  ])
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
