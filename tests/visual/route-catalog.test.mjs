import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import test from 'node:test'
import {
  discoverPageRoutes,
  pagesRoot,
  parseRouteShard,
  repositoryRoot,
  selectRouteShard,
  validatePageRoutes,
  viewportCatalog,
} from './route-catalog.mjs'

const context = { gameId: 67, challengeId: 326, postId: 'ffac23df' }
const routes = discoverPageRoutes(context)

test('visual route catalog covers every React page component exactly once', () => {
  assert.equal(routes.length, 50)
  assert.deepEqual(validatePageRoutes(routes), [])
  assert.ok(routes.every((route) => route.sourceFile.endsWith('.tsx')))
  assert.ok(routes.some((route) => route.sourceFile === '[...all].tsx'))
  assert.ok(routes.some((route) => route.path === '/admin/games/67/challenges/326/flags'))
  assert.ok(routes.some((route) => route.path === '/posts/ffac23df/edit'))
  assert.ok(pagesRoot.endsWith(join('web', 'src', 'pages')))
})

test('visual routes select the least privileged useful browser identity', () => {
  for (const route of routes.filter((candidate) => candidate.path.startsWith('/admin/'))) {
    assert.equal(route.auth, 'admin', route.path)
  }
  assert.equal(routes.find((route) => route.path === '/account/login')?.auth, 'anonymous')
  assert.equal(routes.find((route) => route.path === '/account/profile')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/teams')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/games/67/submit')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/games/67/monitor/events')?.auth, 'admin')
  assert.equal(routes.find((route) => route.path === '/account/stats')?.expectedPath, '/account/profile')
})

test('visual audit covers desktop, intermediate, tablet, and compact breakpoints', () => {
  assert.deepEqual(viewportCatalog, {
    desktop: { width: 1440, height: 1100, mobile: false },
    laptop: { width: 1024, height: 768, mobile: false },
    tablet: { width: 768, height: 1024, mobile: true },
    mobile: { width: 390, height: 844, mobile: true },
    compact: { width: 320, height: 568, mobile: true },
  })
})

test('visual route shards cover every route exactly once', () => {
  const first = selectRouteShard(routes, parseRouteShard('1/2'))
  const second = selectRouteShard(routes, parseRouteShard('2/2'))
  assert.equal(first.length, 25)
  assert.equal(second.length, 25)
  assert.deepEqual([...first, ...second], routes)
  assert.throws(() => parseRouteShard('0/2'), /INDEX\/TOTAL/)
  assert.throws(() => parseRouteShard('3/2'), /cannot exceed/)
})

test('visual audit waits for loaded page content before taking screenshots', () => {
  const auditSource = readFileSync(join(repositoryRoot, 'tests', 'visual', 'audit.mjs'), 'utf8')
  assert.match(auditSource, /state\.h1 === 1/)
  assert.match(auditSource, /state\.loadingOverlays === 0/)
  assert.match(auditSource, /\.mantine-LoadingOverlay-root/)
  assert.match(auditSource, /shadowRoot\?\.querySelectorAll\('h1'\)/)
})

test('visual audit artifacts are excluded from source control and Docker contexts', () => {
  const gitIgnore = readFileSync(join(repositoryRoot, '.gitignore'), 'utf8')
  const dockerIgnore = readFileSync(join(repositoryRoot, '.dockerignore'), 'utf8')
  assert.match(gitIgnore, /^\/visual-audit-output\/?$/m)
  assert.match(dockerIgnore, /^\/visual-audit-output\/?$/m)
})
