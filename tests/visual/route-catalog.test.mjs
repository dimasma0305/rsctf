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
  assert.equal(routes.length, 53)
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
  assert.equal(routes.find((route) => route.path === '/donations')?.auth, 'anonymous')
  assert.equal(routes.find((route) => route.path === '/account/profile')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/teams')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/challenges')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/guide')?.auth, 'anonymous')
  assert.equal(routes.find((route) => route.path === '/games/67/submit')?.auth, 'player')
  assert.equal(routes.find((route) => route.path === '/games/67/monitor/events')?.auth, 'admin')
  assert.equal(routes.find((route) => route.path === '/account/stats')?.expectedPath, '/account/profile')
})

test('game workspace routes share one visual layout group', () => {
  const workspacePaths = [
    '/games/67/challenges',
    '/games/67/scoreboard',
    '/games/67/submit',
    '/games/67/monitor/events',
    '/games/67/monitor/submissions',
    '/games/67/monitor/cheatcheck',
    '/games/67/monitor/traffic',
  ]
  for (const path of workspacePaths) {
    assert.equal(routes.find((route) => route.path === path)?.layoutGroup, 'game-workspace', path)
  }
  assert.equal(routes.find((route) => route.path === '/games/67')?.layoutGroup, undefined)
  assert.equal(routes.find((route) => route.path === '/games/67/attack')?.layoutGroup, undefined)
})

test('game workspace uses one bounded width and container-sized challenge cards', () => {
  const sources = [
    'web/src/components/WithGameMonitor.tsx',
    'web/src/pages/games/[id]/Challenges.tsx',
    'web/src/pages/games/[id]/Scoreboard.tsx',
    'web/src/pages/games/[id]/submit.tsx',
  ]
  for (const source of sources) {
    const contents = readFileSync(join(repositoryRoot, source), 'utf8')
    assert.match(contents, /width=\{GAME_PAGE_CONTENT_WIDTH\}/, source)
  }

  const navbar = readFileSync(join(repositoryRoot, 'web/src/components/WithNavbar.tsx'), 'utf8')
  assert.match(navbar, /GAME_PAGE_CONTENT_WIDTH = '1800px'/)
  assert.match(navbar, /data-page-content/)

  const challengeGrid = readFileSync(
    join(repositoryRoot, 'web/src/styles/components/ChallengePanel.module.css'),
    'utf8'
  )
  assert.match(challengeGrid, /repeat\(auto-fill, minmax\(min\(15rem, 100%\), 1fr\)\)/)
})

test('cheat analysis separates its sections and keeps evidence tabs on one row', () => {
  const component = readFileSync(join(repositoryRoot, 'web/src/components/monitor/CheatInfo.tsx'), 'utf8')
  const styles = readFileSync(join(repositoryRoot, 'web/src/components/monitor/CheatInfo.module.css'), 'utf8')
  const audit = readFileSync(join(repositoryRoot, 'tests/visual/audit.mjs'), 'utf8')

  assert.match(component, /data-layout-section="cheat-summary"/)
  assert.match(component, /data-min-block-gap="8"/)
  assert.match(styles, /\.summaryGrid[\s\S]*margin-bottom: var\(--mantine-spacing-md\)/)
  assert.match(styles, /\.innerTabList[\s\S]*flex-wrap: nowrap/)
  assert.match(styles, /\.innerTabList[\s\S]*overflow-x: auto/)
  assert.match(component, /data-max-layout-rows="1"/)
  assert.match(audit, /section gap is \$\{gap\.actual\}px/)
  assert.match(audit, /uses \$\{rows\.actual\} rows/)
  assert.match(audit, /has \$\{rows\.overflowingChildren\} overflowing children/)
})

test('visual audit covers ultrawide, desktop, intermediate, and compact breakpoints', () => {
  assert.deepEqual(viewportCatalog, {
    ultrawide: { width: 3440, height: 1440, mobile: false },
    wide: { width: 1920, height: 1080, mobile: false },
    desktop: { width: 1440, height: 1100, mobile: false },
    notebook: { width: 1366, height: 768, mobile: false },
    laptop: { width: 1024, height: 768, mobile: false },
    tablet: { width: 768, height: 1024, mobile: true },
    mobile: { width: 390, height: 844, mobile: true },
    compact: { width: 320, height: 568, mobile: true },
  })
})

test('visual route shards cover every route exactly once', () => {
  const first = selectRouteShard(routes, parseRouteShard('1/2'))
  const second = selectRouteShard(routes, parseRouteShard('2/2'))
  assert.equal(first.length, 26)
  assert.equal(second.length, 27)
  assert.deepEqual([...first, ...second], routes)
  assert.throws(() => parseRouteShard('0/2'), /INDEX\/TOTAL/)
  assert.throws(() => parseRouteShard('3/2'), /cannot exceed/)
})

test('visual audit waits for loaded page content before taking screenshots', () => {
  const auditSource = readFileSync(join(repositoryRoot, 'tests', 'visual', 'audit.mjs'), 'utf8')
  assert.match(auditSource, /state\.h1 === 1/)
  assert.match(auditSource, /state\.loadingOverlays === 0/)
  assert.match(auditSource, /filter\.startsWith\('='\)/)
  assert.match(auditSource, /RSCTF_VISUAL_DISABLE_GUIDE/)
  assert.match(auditSource, /\.mantine-LoadingOverlay-root/)
  assert.match(auditSource, /shadowRoot\?\.querySelectorAll\('h1'\)/)
})

test('visual audit enforces compact and usable interactive guide budgets', () => {
  const auditSource = readFileSync(join(repositoryRoot, 'tests', 'visual', 'audit.mjs'), 'utf8')
  assert.match(auditSource, /data-guide-surface="coachmark"/)
  assert.match(auditSource, /guideAreaBudget = result\.width\.viewport <= 320 \? 0\.45/)
  assert.match(auditSource, /result\.guide\.targetVisibleRatio < 0\.9/)
  assert.match(auditSource, /guide target is not pointer-accessible/)
  assert.match(auditSource, /guide controls are outside the viewport/)
  assert.match(auditSource, /budget is 280/)
})

test('profile identity header shrinks without escaping compact viewports', () => {
  const profile = readFileSync(join(repositoryRoot, 'web/src/pages/account/Profile.tsx'), 'utf8')
  assert.match(profile, /<Group wrap="nowrap" w="100%">/)
  assert.match(profile, /<Box miw=\{0\} style=\{\{ flex: 1 \}\}>/)
  assert.match(profile, /<Text size="sm" c="dimmed" truncate title=\{user\?\.email \?\? undefined\}>/)
})

test('visual audit artifacts are excluded from source control and Docker contexts', () => {
  const gitIgnore = readFileSync(join(repositoryRoot, '.gitignore'), 'utf8')
  const dockerIgnore = readFileSync(join(repositoryRoot, '.dockerignore'), 'utf8')
  assert.match(gitIgnore, /^\/visual-audit-output\/?$/m)
  assert.match(dockerIgnore, /^\/visual-audit-output\/?$/m)
})
