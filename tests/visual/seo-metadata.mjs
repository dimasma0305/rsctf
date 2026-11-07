// Real browser navigation with disposable public data; no deployment writes.
import assert from 'node:assert/strict'
import { resolve } from 'node:path'
import { chromium, expect } from '../../web/node_modules/@playwright/test/index.mjs'
import { events, fixture } from './overview-pages-fixtures.mjs'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { resolveChromium } from './cdp.mjs'
import { repositoryRoot } from './route-catalog.mjs'

const server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
const browser = await chromium.launch({ executablePath: resolveChromium(), args: ['--no-sandbox'] }).catch(async error => {
  await server.close()
  throw error
})
try {
  const page = await browser.newPage()
  const errors = []
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(() => {
    localStorage.setItem('language', JSON.stringify('en-US'))
    for (const id of ['guest', '11111111-1111-4111-8111-111111111111']) {
      localStorage.setItem('rsctf-player-guide:' + id, JSON.stringify({ interactiveEnabled: false, completedVersion: 5, seenFeatures: [], activeTourStep: null, tourPaused: true }))
    }
  })
  await page.route('**/hub**', route => route.fulfill({ json: {} }))
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const result = structuredClone(fixture(path, route.request().method()))
    if (path === '/api/config') result.body = { ...result.body, title: 'SEO fixture', description: 'Learn security together', publicUrl: 'https://ctf.example' }
    if (path === '/api/game/19') {
      result.status = 200
      result.body = { ...events[0], id: 19, title: 'Public CTF', summary: 'Practice with your team', hidden: false }
    }
    await route.fulfill({ status: result.status || 200, json: result.body })
  })
  const meta = name => page.locator(`head meta[name="${name}"],head meta[property="${name}"]`)
  await page.goto(server.origin + '/games/19?tracking=ignored#fragment')
  await expect(meta('og:title')).toHaveAttribute('content', 'Public CTF - SEO fixture::CTF')
  await expect(meta('description')).toHaveAttribute('content', 'Practice with your team')
  await expect(meta('robots')).toHaveAttribute('content', 'index, follow')
  await expect(page.locator('head link[rel="canonical"]')).toHaveAttribute('href', 'https://ctf.example/games/19')
  // Use a real SPA link to verify that the previous page's metadata is retired.
  await page.locator('a[href="/games"]').first().click()
  await expect(page).toHaveURL(server.origin + '/games')
  await expect(meta('description')).toHaveAttribute('content', 'Learn security together')
  await expect(page.locator('head link[rel="canonical"]')).toHaveAttribute('href', 'https://ctf.example/games')
  await page.goBack()
  await expect(meta('description')).toHaveAttribute('content', 'Practice with your team')
  await page.goto(server.origin + '/admin/settings?token=never-in-metadata')
  await expect(page.getByText('Platform Settings', { exact: true })).toBeVisible()
  await expect(meta('robots')).toHaveAttribute('content', 'noindex, nofollow')
  await expect(meta('og:title')).toHaveAttribute('content', 'SEO fixture::CTF')
  await expect(page.locator('head link[rel="canonical"]')).toHaveCount(0)
  assert.equal((await page.locator('head').innerHTML()).includes('never-in-metadata'), false)
  assert.deepEqual(errors, [])
  console.log('SEO metadata: public previews, canonical URLs, SPA navigation, Back and private-page noindex passed in Chromium.')
} finally {
  await browser.close()
  await server.close()
}
