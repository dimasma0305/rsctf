import { test, expect } from '@playwright/test'
import { build } from 'esbuild'
import { resolve } from 'node:path'
import { repositoryRoot } from '../../tests/visual/route-catalog.mjs'

test('scanner preserves hidden, disabled, custom, duplicate and shadow controls without clicking', async ({ page }) => {
  const scanner = (
    await build({
      entryPoints: [resolve(repositoryRoot, 'web/e2e/scan-controls.mjs')],
      bundle: true,
      write: false,
      format: 'iife',
      platform: 'browser',
    })
  ).outputFiles[0].text
  await page.setContent(`<label for="email">Email</label><input id="email">
    <button aria-label="Icon action"><svg></svg></button><button disabled>Disabled</button>
    <button hidden>Hidden</button><button>Duplicate</button><button>Duplicate</button>
    <div role="button" tabindex="0">Custom role</div><div id="custom">Custom handler</div>
    <button aria-haspopup="menu">Open menu</button><a href="https://example.invalid">External</a>
    <input type="hidden" value="must-not-be-in-inventory"><div id="shadow"></div>`)
  await page.evaluate(() => {
    const resource = document.createElement('link')
    resource.rel = 'icon'
    resource.href = 'data:,'
    document.head.append(resource)
    window.clicks = 0
    document.addEventListener('click', () => window.clicks++)
    document.querySelector('#custom').__reactProps$fixture = { onClick: () => {} }
    document.querySelector('#shadow').attachShadow({ mode: 'open' }).innerHTML =
      '<button>Shadow action</button><button>Shadow action</button>'
  })
  await page.addScriptTag({ content: scanner })
  const controls = await page.evaluate(() => window.__scanControls())
  expect(controls.find((c) => c.tag === 'input').name).toBe('Email')
  expect(controls.find((c) => c.name === 'Icon action').role).toBe('button')
  expect(controls.find((c) => c.name === 'Disabled').status).toBe('disabled')
  expect(controls.some((c) => c.status === 'hidden')).toBe(true)
  expect(controls.filter((c) => c.name === 'Duplicate')).toHaveLength(2)
  expect(new Set(controls.map((c) => c.selector)).size).toBe(controls.length)
  expect(controls.some((c) => c.handler && !c.role)).toBe(true)
  expect(controls.find((c) => c.name === 'Open menu').safeOpen).toBe(true)
  expect(controls.find((c) => c.name === 'External').safeOpen).toBe(false)
  expect(controls.find((c) => c.name === 'Shadow action').selector).toContain(' >>> ')
  expect(controls.some((c) => c.type === 'hidden')).toBe(false)
  expect(controls.some((c) => c.tag === 'link' || c.selector.includes('head:'))).toBe(false)
  expect(await page.evaluate(() => window.clicks)).toBe(0)
})
