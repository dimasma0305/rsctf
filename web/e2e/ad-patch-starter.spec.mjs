import { expect, test } from '@playwright/test'
import { resolve } from 'node:path'
import JSZip from 'jszip'
import { serveAccountBuild } from '../../tests/visual/account-access-fixtures.mjs'
import { fixture } from '../../tests/visual/admin-navigation-fixtures.mjs'
import { repositoryRoot } from '../../tests/visual/route-catalog.mjs'

let server
test.beforeAll(async () => { server = await serveAccountBuild(resolve(repositoryRoot, 'web/build')) })
test.afterAll(async () => { await server?.close() })

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
