import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import test from 'node:test'

const webRequire = createRequire(new URL('../../web/package.json', import.meta.url))
const reactPdfRequire = createRequire(webRequire.resolve('react-pdf'))
const viewer = readFileSync(new URL('../../web/src/components/admin/PDFViewer.tsx', import.meta.url), 'utf8')

test('the bundled PDF worker matches the engine resolved by React-PDF', () => {
  const worker = webRequire('pdfjs-dist/package.json')
  const engine = reactPdfRequire('pdfjs-dist/package.json')
  assert.equal(worker.version, engine.version, 'PDF.js rejects a worker with a different API version')
  assert.match(viewer, /new URL\('pdfjs-dist\/legacy\/build\/pdf\.worker\.min\.mjs', import\.meta\.url\)/)
})

test('the PDF viewer keeps callback-based loading and errors when React-PDF defaults change', () => {
  const document = viewer.match(/<Document\s[\s\S]*?(?=\{\/\* Keep the document)/)?.[0]
  assert.ok(document, 'the existing Document boundary must remain present')
  assert.match(document, /suspense=\{false\}/)
  assert.match(document, /onLoadSuccess=/)
  assert.match(document, /onLoadError=/)
})

test('both PDF.js contexts use upstream compatibility builds for supported older browsers', () => {
  const vite = readFileSync(new URL('../../web/vite.config.mts', import.meta.url), 'utf8')
  assert.ok(vite.includes('find: /^pdfjs-dist$/'), 'the bare engine import must have an exact alias')
  assert.match(vite, /replacement: 'pdfjs-dist\/legacy\/build\/pdf\.mjs'/)
  assert.match(viewer, /new URL\('pdfjs-dist\/legacy\/build\/pdf\.worker\.min\.mjs', import\.meta\.url\)/)
})

test('writeup audits wait for tab commits and rendered frames before collecting animations', () => {
  const harness = readFileSync(new URL('./writeup-grading.mjs', import.meta.url), 'utf8')
  const audit = harness.slice(harness.indexOf('const audit ='), harness.indexOf('\ntry {'))
  const frames = audit.indexOf('requestAnimationFrame')
  assert.ok(frames >= 0 && frames < audit.indexOf('document.getAnimations()'),
    'a pending React commit can create an entrance animation after the old animation list was read')
  assert.match(harness, /await clickText\('Projected scoreboard'\);\s*await wait\(`[^`]*aria-selected[^`]*Projected scoreboard[^`]*`\)\s*await audit\(name\+'-ranking'\)/)
  assert.match(audit, /values:\['wcag2a','wcag2aa','wcag21aa'\]/,
    'animation synchronization must not disable the accessibility rules')
})
