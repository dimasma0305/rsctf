// Exercise saved branding through the real settings UI in Chromium.
// API responses and logo assets are disposable fixtures; no deployment is used.
import assert from 'node:assert/strict'
import { resolve } from 'node:path'
import { chromium, expect } from '../../web/node_modules/@playwright/test/index.mjs'
import { fixture } from './admin-navigation-fixtures.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { resolveChromium } from './cdp.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const browser = await chromium.launch({ executablePath: resolveChromium(), args: ['--no-sandbox'] }).catch(async error => {
  await server.close()
  throw error
})
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9e0AAAAASUVORK5CYII=', 'base64')
let hash = null, stagedHash = null, revision = 1, uploads = 0
const errors = [], requestedIcons = new Set()
try {
  const page = await browser.newPage()
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(() => {
    localStorage.setItem('language', JSON.stringify('en-US'))
    for (const id of ['guest', '11111111-1111-4111-8111-111111111111']) {
      localStorage.setItem('rsctf-player-guide:' + id, JSON.stringify({ interactiveEnabled: false, completedVersion: 5, seenFeatures: [], activeTourStep: null, tourPaused: true }))
    }
  })
  await page.route('**/assets/*/logo', async route => {
    requestedIcons.add(new URL(route.request().url()).pathname)
    await route.fulfill({ contentType: 'image/png', body: png })
  })
  await page.route('**/hub**', route => route.fulfill({ json: {} }))
  await page.route('**/api/**', async route => {
    const request = route.request(), path = new URL(request.url()).pathname
    let result = structuredClone(fixture(path, request.method()))
    if (path === '/api/config') result.body = { ...result.body, logoUrl: hash ? `/assets/${hash}/logo` : null }
    if (path === '/api/admin/config' && request.method() === 'GET') {
      result.body = { ...result.body, revision, globalConfig: { ...result.body.globalConfig, logoHash: hash, faviconHash: hash } }
    }
    if (path.startsWith('/api/admin/config/logo/stage/')) {
      stagedHash = `logo-${++uploads}`
      result = { body: { operationId: path.split('/').at(-1), brandingHash: stagedHash } }
    }
    if (path === '/api/admin/config' && request.method() === 'PUT') {
      const body = request.postDataJSON()
      assert.equal(body.expectedRevision, revision)
      hash = body.brandingAction === 'Clear' ? null : stagedHash
      result = { body: { operationId: body.operationId, revision: ++revision, brandingHash: hash } }
    }
    await route.fulfill({ status: result.status || 200, json: result.body })
  })
  const icon = page.locator('head link[rel="icon"]')
  const checkIcon = async (url, type) => {
    await expect(icon).toHaveCount(1)
    await expect(icon).toHaveAttribute('href', url)
    assert.equal(await icon.getAttribute('type'), type)
  }
  await page.goto(server.origin + '/admin/settings')
  await expect(page.getByText('Platform Settings', { exact: true })).toBeVisible()
  await checkIcon('/favicon.svg', 'image/svg+xml')
  for (let i = 1; i <= 2; i++) {
    const previous = i === 1 ? '/favicon.svg' : '/assets/logo-1/logo'
    await page.locator('input[type=file]').setInputFiles({ name: `logo-${i}.png`, mimeType: 'image/png', buffer: png })
    await checkIcon(previous, i === 1 ? 'image/svg+xml' : null)
    await page.getByRole('button', { name: 'Save Changes', exact: true }).click()
    await checkIcon(`/assets/logo-${i}/logo`, null)
    await expect.poll(() => requestedIcons.has(`/assets/logo-${i}/logo`)).toBe(true)
    await page.reload()
    await checkIcon(`/assets/logo-${i}/logo`, null)
  }
  await page.goto(server.origin + '/about')
  await checkIcon('/assets/logo-2/logo', null)
  await page.goto(server.origin + '/admin/settings')
  await expect(page.getByText('Platform Settings', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Reset', exact: true }).click()
  await checkIcon('/assets/logo-2/logo', null)
  await page.getByRole('button', { name: 'Save Changes', exact: true }).click()
  await checkIcon('/favicon.svg', 'image/svg+xml')
  await page.reload()
  await expect(page.getByText('Platform Settings', { exact: true })).toBeVisible()
  await checkIcon('/favicon.svg', 'image/svg+xml')
  assert.deepEqual(errors, [])
  console.log('Favicon: default, upload, replacement, reset, unsaved previews and reloads passed in Chromium.')
} finally {
  await browser.close()
  await server.close()
}
