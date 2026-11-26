import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
import { serveAccountBuild } from './account-access-fixtures.mjs'
import { arenaBrowserSetup, createArenaFixture } from './attack-arena-fixtures.mjs'
import { resolveChromium } from './cdp.mjs'

const { chromium } = createRequire(new URL('../../web/package.json', import.meta.url))('@playwright/test')
const app = await serveAccountBuild(resolve(process.argv[2] || 'web/build'))
const fixture = createArenaFixture('mixed', 8)
const browser = await chromium.launch({ executablePath: resolveChromium(), args: ['--disable-dev-shm-usage'] })
try {
  const page = await browser.newPage({ viewport: { width: 1180, height: 760 }, colorScheme: 'dark', reducedMotion: 'no-preference' })
  const errors = []
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(arenaBrowserSetup)
  if (process.argv.includes('--software')) {
    await page.addInitScript(() => {
      const getContext = HTMLCanvasElement.prototype.getContext
      HTMLCanvasElement.prototype.getContext = function(type, options) {
        if (type === 'webgl' && options?.failIfMajorPerformanceCaveat) return null
        return getContext.call(this, type, options)
      }
    })
  }
  await page.route('**/*', route => {
    const url = new URL(route.request().url())
    if (url.origin !== app.origin) return route.abort()
    if (!/^\/(api\/|hub)/i.test(url.pathname)) return route.continue()
    const { status, body } = fixture.fixture(url.href, route.request().method())
    return route.fulfill({ status, json: body })
  })
  await page.goto(app.origin + '/games/901/attack')
  await page.locator('#rk-p2').waitFor({ state: 'attached' })
  await page.locator('#arena-tab-teams').click()
  await page.locator('#fsBtn').click()
  await page.locator('#rk-p2').evaluate(button => button.click())
  await page.locator('#mapLabelsBtn').evaluate(button => button.click())
  await page.waitForTimeout(800)
  const report = await page.evaluate(async names => {
    const root = document.querySelector('[data-arena-theme]').shadowRoot
    const socket = arenaSockets.find(s => s.readyState === 1)
    const intervals = []
    const start = performance.now()
    let last = start, sent = 0
    const timer = setInterval(() => {
      while (sent < 100 && performance.now() - start >= sent * 80) {
        const victim = sent % 2
        socket.frame({ kind: 'attack', type: 'Normal', teamName: names[victim + 2 + sent % 3], victimTeamName: names[victim], challengeTitle: 'Relay' })
        sent++
      }
    }, 20)
    await new Promise(resolve => {
      function sample(ts) {
        if (ts > last && ts - start > 500) intervals.push(ts - last)
        last = ts
        if (ts - start < 9000) requestAnimationFrame(sample)
        else resolve()
      }
      requestAnimationFrame(sample)
    })
    clearInterval(timer)
    intervals.sort((a, b) => a - b)
    const quantile = q => intervals[Math.floor((intervals.length - 1) * q)]
    const mean = intervals.reduce((sum, time) => sum + time, 0) / intervals.length
    const gpu = root.querySelector('canvas.planet-impact:not([id])')?.getContext('webgl')
    const info = gpu?.getExtension('WEBGL_debug_renderer_info')
    return {
      hits: Number(root.getElementById('arenaSurface').dataset.impacts),
      renderer: gpu ? (info ? gpu.getParameter(info.UNMASKED_RENDERER_WEBGL) : 'WebGL') : 'Canvas 2D',
      fps: 1000 / mean, frames: intervals.length,
      ms: { mean, p50: quantile(.5), p90: quantile(.9), p95: quantile(.95), p99: quantile(.99), max: intervals.at(-1) },
    }
  }, fixture.names)
  console.log(JSON.stringify(report))
  assert.equal(report.hits, 100)
  assert.deepEqual(errors, [])
} finally {
  await browser.close()
  await app.close()
}
