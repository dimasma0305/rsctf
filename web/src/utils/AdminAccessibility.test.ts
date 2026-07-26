import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

test('admin scroll regions provide keyboard access and an accessible name', () => {
  const dashboard = readFileSync('src/pages/admin/Dashboard.tsx', 'utf8')
  const instances = readFileSync('src/pages/admin/Instances.tsx', 'utf8')
  const logs = readFileSync('src/pages/admin/Logs.tsx', 'utf8')

  assert.equal((dashboard.match(/viewportProps=\{\{/g) ?? []).length, 4)
  assert.equal((dashboard.match(/tabIndex:\s*0,/g) ?? []).length, 4)
  assert.match(instances, /viewportProps=\{\{[\s\S]*?tabIndex:\s*0,[\s\S]*?'aria-label':/)
  assert.match(logs, /viewportProps=\{\{[\s\S]*?tabIndex:\s*0,[\s\S]*?'aria-label':/)
})

test('dense operational history uses responsive cards and named controls', () => {
  const builds = readFileSync('src/pages/admin/builds.tsx', 'utf8')
  const buildPresentation = readFileSync('src/components/admin/builds/buildPresentation.ts', 'utf8')
  const logs = readFileSync('src/pages/admin/Logs.tsx', 'utf8')

  assert.match(builds, /<BuildHistoryCard/)
  assert.match(builds, /hiddenFrom="md"/)
  assert.match(builds, /getItemProps=\{\(itemPage\) => \(\{/)
  assert.match(builds, /<Pagination\.Previous[\s\S]*?aria-label=/)
  assert.match(builds, /<Pagination\.Next[\s\S]*?aria-label=/)
  assert.match(buildPresentation, /BUILD_STATUS_VARIANT = 'light'/)

  assert.match(logs, /visibleFrom="md"/)
  assert.match(logs, /hiddenFrom="md"/)
  assert.doesNotMatch(logs, /tableClasses\.overflow/)
  assert.equal((logs.match(/closeButtonProps:/g) ?? []).length, 2)
})

test('admin action columns expose text to assistive technology', () => {
  const pages = [
    'src/pages/admin/Teams.tsx',
    'src/pages/admin/Users.tsx',
    'src/pages/admin/Instances.tsx',
    'src/pages/admin/builds.tsx',
    'src/pages/admin/games/Index.tsx',
    'src/pages/admin/anti-cheat.tsx',
  ]

  for (const page of pages) {
    const source = readFileSync(page, 'utf8')
    assert.doesNotMatch(source, /<Table\.Th[^>]*aria-label=[^>]*\/>/, page)
    assert.match(source, /<Table\.Th[^>]*>[\s\S]*?className="app-sr-only"/, page)
  }
})

test('repeated mobile action landmarks use entity-specific names', () => {
  for (const page of ['src/pages/admin/Teams.tsx', 'src/pages/admin/Users.tsx', 'src/pages/admin/games/Index.tsx']) {
    const source = readFileSync(page, 'utf8')
    assert.doesNotMatch(source, /component="section"\s+aria-label=\{t\('common\.label\.action'/, page)
    assert.match(source, /component="section"[\s\S]*?Actions for \{\{name\}\}/, page)
  }
})

test('game notices keep one realtime connection across ordinary rerenders', () => {
  const source = readFileSync('src/components/GameNoticePanel.tsx', 'utf8')

  assert.match(source, /\}, \[id, numId, t, theme\.primaryColor\]\)/)
  assert.doesNotMatch(source, /\n  \}\)\n\n  const allNotices/)
})

test('the mobile app-shell scroll region remains keyboard accessible', () => {
  const source = readFileSync('src/components/WithNavbar.tsx', 'utf8')

  assert.match(source, /id="main-content"[\s\S]*?tabIndex=\{isMobile \? 0 : -1\}/)
})
