import { test, expect } from '@playwright/test'
import { build } from 'esbuild'
import { mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { serveAccountBuild } from '../../tests/visual/account-access-fixtures.mjs'
import { arenaBrowserSetup } from '../../tests/visual/attack-arena-fixtures.mjs'
import { fixture, player } from '../../tests/visual/full-layout-fixtures.mjs'
import { discoverPageRoutes, validatePageRoutes, repositoryRoot } from '../../tests/visual/route-catalog.mjs'
import { installSurfaceProbe, surfaceFailures } from '../../tests/visual/surface-layout-metrics.mjs'
import { pdfFixture } from '../../tests/visual/writeup-grading-fixtures.mjs'

const routes = discoverPageRoutes({ gameId: 901, challengeId: 9001, postId: '00000001' })
expect(validatePageRoutes(routes)).toEqual([])
expect(
  routes.filter((r) => /[\[\]]/.test(r.path)),
  'new dynamic parameters need explicit fixture IDs'
).toEqual([])
let server, scanner
const surfaces = '[role="dialog"], [role="menu"], [role="listbox"], [role="tree"]'
const dialogTriggers = {
  '/account/profile': ['Change Email', 'Change Password'],
  '/admin/games': ['New Game'],
  '/admin/repo-bindings': ['Add repository'],
}

test.beforeAll(async () => {
  server = await serveAccountBuild(resolve(repositoryRoot, 'web/build'))
  scanner = (
    await build({
      entryPoints: [resolve(repositoryRoot, 'web/e2e/scan-controls.mjs')],
      bundle: true,
      write: false,
      format: 'iife',
      platform: 'browser',
    })
  ).outputFiles[0].text
})
test.afterAll(async () => {
  await server?.close()
})

for (const route of routes) {
  test(`${route.path} maps controls and popup states`, async ({ page, context }, info) => {
    const report = {
      route: route.path,
      source: route.sourceFile,
      project: info.project.name,
      role: route.auth === 'anonymous' && route.path.startsWith('/account/') ? 'Guest' : 'Admin',
      states: [],
      probes: [],
      unknownReads: [],
      writes: [],
      emptyReads: [],
      errors: [],
      limits: [
        'One fixture identity per route; not all roles or data states.',
        'Nested popup controls are inventoried; deeper behavior requires explicit scenarios.',
        'Forms, downloads, external links and mutation controls are mapped but not activated.',
      ],
    }
    page.on('pageerror', (error) => report.errors.push(String(error)))
    await context.route('**/*', async (intercepted) => {
      const request = intercepted.request(),
        url = new URL(request.url())
      // Only the owned loopback server can be reached. No live API or external side effect.
      if (url.origin !== server.origin) return intercepted.abort('blockedbyclient')
      if (url.pathname === '/assets/writeup-fixture.pdf')
        return intercepted.fulfill({ contentType: 'application/pdf', body: pdfFixture() })
      if (/^\/(api\/|hub)/i.test(url.pathname)) {
        const result = fixture(url.pathname + url.search, request.method(), report.role)
        if (result.status === 404) report.unknownReads.push(url.pathname)
        if (result.status === 405) report.writes.push({ path: url.pathname, method: request.method() })
        if (result.contract) report.emptyReads.push({ path: url.pathname, contract: result.contract })
        await new Promise((r) => setTimeout(r, 150))
        return intercepted.fulfill({ status: result.status || 200, json: result.body })
      }
      return intercepted.continue()
    })
    await context.addInitScript({
      content: `${arenaBrowserSetup};
      localStorage.setItem('rsctf-player-guide:${player.profile.userId}',JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[]}));
      localStorage.setItem('challenge-explorer-view',JSON.stringify('list'));
      Object.defineProperty(navigator,'connection',{value:{saveData:true,effectiveType:'3g'}});`,
    })
    const scan = async (name) => {
      const controls = await page.evaluate(() => window.__scanControls())
      report.states.push({ name, controls })
      return controls
    }
    try {
      await page.goto(server.origin + route.urlPath)
      await expect(page.locator('#main-content h1').first()).toBeVisible()
      await expect.poll(() => new URL(page.url()).pathname).toBe(route.expectedPath)
      // Bounded settling for chained fixture reads; do not use networkidle with polling.
      await page.waitForTimeout(1500)
      await page.evaluate(() => document.fonts.ready)
      await expect(page.locator('[data-request-error], [data-error-fallback]')).toHaveCount(0)
      await page.addScriptTag({ content: scanner })
      await page.evaluate(installSurfaceProbe, { popupSelector: surfaces })
      const initial = await scan('page')
      expect(initial.length, 'route must expose a control inventory').toBeGreaterThan(0)
      const triggers = initial.filter(
        (c) =>
          c.safeOpen ||
          (!c.hidden && !c.disabled && c.role === 'button' && dialogTriggers[route.path]?.includes(c.name))
      )
      expect(triggers.length, 'increase reviewed scan budget when a page grows').toBeLessThanOrEqual(60)
      for (const control of triggers) {
        const target = page.locator(control.selector.replaceAll(' >>> ', ' '))
        const probe = { selector: control.selector, name: control.name, status: 'unverified' }
        report.probes.push(probe)
        // Rendering may have replaced/hidden a previously discovered control.
        if ((await target.count()) !== 1 || !(await target.isVisible()) || !(await target.isEnabled())) {
          probe.reason = control.reason = 'Control changed state before its probe'
          continue
        }
        try {
          await target.click({ trial: true, timeout: 1500 })
        } catch {
          probe.reason = control.reason = 'Not pointer-actionable in this state'
          continue
        }
        await page.mouse.move(0, 0)
        const before = await page.locator(surfaces).filter({ visible: true }).count()
        const expanded = await target.getAttribute('aria-expanded')
        const wasOpen = control.tag === 'summary' && (await target.evaluate((e) => e.parentElement.open))
        // Start at the actual pointer event, after Playwright's actionability wait.
        // Starting earlier mistakes normal entrance animations for late movement.
        await target.evaluate((e) => {
          window.__interactionMeasurement = null
          e.addEventListener(
            'pointerdown',
            () => {
              window.__interactionMeasurement = window.__surfaceProbe(() => {})
            },
            { once: true }
          )
        })
        await target.click()
        const frames = await page.evaluate(() => window.__interactionMeasurement)
        expect(frames, 'pointer event must start the geometry observer').toBeTruthy()
        await scan(`opened:${control.selector}`)
        expect(new URL(page.url()).pathname, 'open-only probe navigated unexpectedly').toBe(route.expectedPath)
        const after = await page.locator(surfaces).filter({ visible: true }).count()
        const expandedAfter = await target.getAttribute('aria-expanded')
        const summaryChanged =
          control.tag === 'summary' && wasOpen !== (await target.evaluate((e) => e.parentElement.open))
        const opened = after > before || expanded !== expandedAfter || summaryChanged
        if (after > before) {
          probe.layoutFailures = surfaceFailures(frames.frames, { popup: true, entranceFromFirstPaint: true })
          if (probe.layoutFailures.length) probe.geometry = frames
          expect.soft(probe.layoutFailures, `${control.name} layout`).toEqual([])
        }
        if (control.tag === 'summary') await target.click()
        else {
          await page.keyboard.press('Escape')
          // Some expanded toggles use the same button to close rather than Escape.
          if (
            opened &&
            (await target.getAttribute('aria-expanded')) !== expanded &&
            (await page.locator(surfaces).filter({ visible: true }).count()) <= before
          )
            await target.click()
          await target.blur()
        }
        await expect(page.locator(surfaces).filter({ visible: true })).toHaveCount(before)
        if (expanded !== null) await expect(target).toHaveAttribute('aria-expanded', expanded)
        if (control.tag === 'summary') expect(await target.evaluate((e) => e.parentElement.open)).toBe(wasOpen)
        if (opened && !probe.layoutFailures?.length) {
          probe.status = 'exercised'
          probe.reason = 'Open state observed, nested controls mapped, and surface closed'
          control.status = 'exercised'
          control.reason = probe.reason
        } else
          probe.reason = opened
            ? 'Surface opened but geometry assertions failed'
            : 'Click produced no observable expanded surface in this fixture'
        control.reason = probe.reason
      }
      await scan('after-probes')
      expect(report.unknownReads).toEqual([])
      expect(report.writes).toEqual([])
      expect(report.errors).toEqual([])
      expect(server.unhandled).toEqual([])
    } finally {
      const body = Buffer.from(JSON.stringify(report, null, 2))
      await info.attach('interaction-inventory', { body, contentType: 'application/json' })
      const dir = resolve(repositoryRoot, 'visual-audit-output/interactions/routes')
      mkdirSync(dir, { recursive: true })
      writeFileSync(resolve(dir, `${info.project.name}-${route.name}.json`), body)
    }
  })
}
