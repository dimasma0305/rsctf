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
  const bindings = readFileSync('src/pages/admin/repo-bindings.tsx', 'utf8')

  assert.match(builds, /<BuildHistoryCard/)
  assert.match(builds, /visibleFrom="lg"/)
  assert.match(builds, /hiddenFrom="lg"/)
  assert.match(builds, /getItemProps=\{\(itemPage\) => \(\{/)
  assert.match(builds, /<Pagination\.Previous[\s\S]*?aria-label=/)
  assert.match(builds, /<Pagination\.Next[\s\S]*?aria-label=/)
  assert.match(buildPresentation, /BUILD_STATUS_VARIANT = 'light'/)

  assert.match(logs, /visibleFrom="md"/)
  assert.match(logs, /hiddenFrom="md"/)
  assert.doesNotMatch(logs, /tableClasses\.overflow/)
  assert.equal((logs.match(/closeButtonProps:/g) ?? []).length, 2)
  assert.equal((bindings.match(/<AccessibleModal/g) ?? []).length, 2)
})

test('repository binding pagination stays compact and mounted while history pages load', () => {
  const bindings = readFileSync('src/pages/admin/repo-bindings.tsx', 'utf8')
  const loadHistory = bindings.slice(bindings.indexOf('const loadHistory'), bindings.indexOf('const onOpenHistory'))

  assert.equal((bindings.match(/<ResponsivePagination\s+value=/g) ?? []).length, 2)
  assert.match(bindings, /useMediaQuery\('\(max-width: 35\.99em\)'/)
  assert.match(bindings, /compact \? \([\s\S]*?common\.pagination\.page_of[\s\S]*?: \([\s\S]*?<Pagination\.Items/)
  assert.match(loadHistory, /setHistoryLoading\(true\)/)
  assert.doesNotMatch(loadHistory, /setHistory\(null\)/)
  assert.match(bindings, /bindingKnownPageCount !== undefined && bindingKnownPageCount > 1/)
  assert.match(loadHistory, /setHistoryRequestedPage\(page\)/)
  assert.equal((bindings.match(/setHistoryPage\(page\)/g) ?? []).length, 1)
  assert.match(bindings, /loadHistory\(historyTarget, historyRequestedPage\)/)
  assert.match(bindings, /<Stack gap="sm" aria-busy=\{historyLoading\}>/)
})

test('dense admin inventories use readable breakpoints and manageable pages', () => {
  const mobileStyles = readFileSync('src/pages/admin/AdminMobileList.module.css', 'utf8')
  const users = readFileSync('src/pages/admin/Users.tsx', 'utf8')
  const games = readFileSync('src/pages/admin/games/Index.tsx', 'utf8')
  const buildCards = readFileSync('src/components/admin/builds/BuildHistoryCard.tsx', 'utf8')

  assert.match(mobileStyles, /@media \(max-width: 25rem\)[\s\S]*?\.actionGrid \{[\s\S]*?repeat\(2,/)
  assert.match(mobileStyles, /\.recordTitle \{[\s\S]*?-webkit-line-clamp: 2;/)

  assert.match(users, /const ITEM_COUNT_PER_PAGE = 20/)
  assert.match(users, /visibleFrom="lg"/)
  assert.match(users, /hiddenFrom="lg" cols=\{\{ base: 1, sm: 2 \}\}/)

  assert.match(games, /const ITEM_COUNT_PER_PAGE = 15/)
  assert.match(games, /visibleFrom="lg"/)
  assert.match(games, /hiddenFrom="lg" cols=\{\{ base: 1, sm: 2 \}\}/)

  assert.match(buildCards, /view_log_short/)
  assert.match(buildCards, /reenqueue_short/)
  assert.match(buildCards, /delete_short/)
})

test('admin navigation keeps every section discoverable without a horizontal scrollbar', () => {
  const navigation = readFileSync('src/components/admin/WithAdminTab.tsx', 'utf8')
  const navigationStyles = readFileSync('src/styles/components/AdminTabs.module.css', 'utf8')
  const workers = readFileSync('src/pages/admin/workers.tsx', 'utf8')

  assert.match(navigation, /visibleFrom="lg"/)
  assert.match(navigation, /hiddenFrom="lg"/)
  assert.match(navigationStyles, /\.navigationItems \{[\s\S]*?display: grid;/)
  assert.match(navigationStyles, /repeat\(auto-fit, minmax\(8\.75rem, 1fr\)\)/)
  assert.doesNotMatch(navigationStyles, /\.navigationViewport \{[\s\S]*?overflow-x: auto;/)
  assert.match(workers, /miw="9rem"[\s\S]*?admin\.workers\.add/)
  assert.match(workers, /const origin = window\.location\.origin/)
})

test('the source-development server forwards copied worker installer URLs to the API', () => {
  const viteConfig = readFileSync('vite.config.mts', 'utf8')

  assert.match(viteConfig, /'\/install': TARGET/)
})

test('admin list responses are decoded before they reach array state', () => {
  const gameInfo = readFileSync('src/pages/admin/games/[id]/Info.tsx', 'utf8')
  const managers = readFileSync('src/pages/admin/games/[id]/Managers.tsx', 'utf8')
  const workers = readFileSync('src/pages/admin/workers.tsx', 'utf8')

  assert.match(gameInfo, /requireApiCollection<EventVpnOverrideModel>[\s\S]*?itemKeys: \['overrides'\]/)
  assert.doesNotMatch(gameInfo, /setVpnOverrides\((?:response|refreshed)\.data\)/)
  assert.doesNotMatch(managers, /as any/)
  assert.equal((managers.match(/requireApiCollection</g) ?? []).length, 2)
  assert.match(workers, /return requireApiCollection<Worker>[\s\S]*?\.items/)
})

test('admin dashboard keeps popular-game metrics visible and action labels intact', () => {
  const dashboard = readFileSync('src/pages/admin/Dashboard.tsx', 'utf8')
  const games = readFileSync('src/pages/admin/games/Index.tsx', 'utf8')

  assert.match(dashboard, /span=\{\{ base: 12, lg: 7 \}\}/)
  assert.match(dashboard, /span=\{\{ base: 12, lg: 5 \}\}/)
  assert.match(dashboard, /className=\{classes\.popularGameMobileRow\}/)
  assert.match(dashboard, /visibleFrom="sm"[\s\S]*?<Table miw=\{360\} horizontalSpacing="xs">/)
  assert.doesNotMatch(games, /grow=\{!isNarrow\}/)
  assert.match(games, /: \{ flexShrink: 0 \}/)
})

test('intentionally shortened operational values expose their full text', () => {
  const gameCards = readFileSync('src/components/GameCard.tsx', 'utf8')
  const buildCards = readFileSync('src/components/admin/builds/BuildHistoryCard.tsx', 'utf8')
  const bindings = readFileSync('src/pages/admin/repo-bindings.tsx', 'utf8')
  const cheatInfo = readFileSync('src/components/monitor/CheatInfo.tsx', 'utf8')

  assert.match(gameCards, /lineClamp=\{2\} className=\{classes\.title\} title=\{eventTitle\}/)
  assert.match(buildCards, /className=\{classes\.cardReference\} title=\{build\.imageRef\}/)
  assert.match(bindings, /lineClamp=\{1\} title=\{b\.currentActivity\}/)
  assert.match(bindings, /lineClamp=\{2\} ff="monospace" title=\{b\.lastScanMessage\}/)
  assert.match(cheatInfo, /className=\{classes\.truncate\} title=\{teamName\}/)
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
  const owner = readFileSync('src/hooks/useRecoveringHub.ts', 'utf8')

  assert.match(source, /useRecoveringHub\(\{[\s\S]*?url: `\/hub\/user\?game=\$\{numId\}`/)
  assert.doesNotMatch(source, /new signalR\.HubConnectionBuilder/)
  assert.match(owner, /useEffect\(\(\) => \{[\s\S]*?handlersRef\.current = handlers/)
  assert.match(owner, /if \(!disposed\) handlersRef\.current\[name\]/)
  assert.match(owner, /\}, \[active, ownerKey, pollingIntervalMs, url\]\)/)
  assert.doesNotMatch(owner, /\[active, handlers, revalidate/)
})

test('the mobile app-shell scroll region remains keyboard accessible', () => {
  const source = readFileSync('src/components/WithNavbar.tsx', 'utf8')

  assert.match(source, /id="main-content"[\s\S]*?tabIndex=\{isMobile \? 0 : -1\}/)
})

test('the post tag editor keeps a comfortably sized mobile text target', () => {
  const source = readFileSync('src/pages/posts/[postId]/Edit.tsx', 'utf8')

  assert.match(source, /styles=\{\{ inputField: \{ minHeight: 28 \} \}\}/)
})
