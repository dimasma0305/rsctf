import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { buildSemanticAccentColors, contrastRatio } from '../utils/ThemeContrast'

test('settings grids respond to their panel width, including the secondary navigation', () => {
  const source = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  const grids = [...source.matchAll(/<(?:SimpleGrid|Grid)\s[\s\S]*?>/g)]
  assert.ok(grids.length >= 10)
  for (const [grid] of grids) {
    assert.match(grid, /type="container"/)
    if (grid.startsWith('<SimpleGrid')) {
      assert.doesNotMatch(grid, /\b(?:sm|md|lg):/, 'SimpleGrid container queries require explicit lengths')
    }
  }
})

test('monitor uses one horizontal section toolbar while preserving access and export ownership', () => {
  const source = readFileSync('src/components/WithGameMonitor.tsx', 'utf8')
  assert.match(source, /<WithRole requiredRole=\{Role.Monitor\}/)
  assert.match(source, /gameScoreboardSheet\(numId, \{ format: 'blob' \}\)/)
  assert.match(source, /game.button.export_scoreboard/)
  assert.match(source, /aria-label=\{t\('game.tab.monitor.index'\)\}/)
  assert.match(source, /if \(overflow !== 0\) scroller.scrollTo/)
  assert.doesNotMatch(source, /useIsMobile|orientation=|calc\(100% - 11rem\)/)
})

test('long workspace names truncate without changing sidebar geometry', () => {
  const css = readFileSync('src/styles/components/AppNavbar.module.css', 'utf8')
  const brand = css.match(/\.brandLink :global\(\.mantine-Title-root\) \{([\s\S]*?)\n\}/)?.[1]
  assert.ok(brand)
  assert.match(brand, /white-space: nowrap/)
  assert.match(brand, /text-overflow: ellipsis/)
  assert.doesNotMatch(brand, /overflow-wrap: anywhere/)
  assert.match(readFileSync('src/components/LogoHeader.tsx', 'utf8'), /title=\{/)
})

test('monitor empty feedback waits for the current snapshot and includes buffered events', () => {
  const source = readFileSync('src/pages/games/[id]/monitor/Events.tsx', 'utf8')
  assert.match(source, /events && visibleEvents.length === 0 && \(/)
  assert.match(source, /h=\{events && visibleEvents.length === 0 \? 'auto'/)
  assert.match(source, /game.content.events_empty_description/)
  assert.doesNotMatch(source, /events\?\.length === 0 && \(/, 'incoming buffered events are not an empty feed')
})

test('shared dark surfaces retain readable neutral and custom accent text', () => {
  const css = readFileSync('src/styles/App.css', 'utf8')
  const dark = css.match(/\[data-mantine-color-scheme='dark'\] \{([\s\S]*?)\n\}/)?.[1]
  assert.ok(dark)
  const surfaces = [
    ...dark.matchAll(
      /--app-(?:background|shell-surface|raised-surface|subtle-surface|hover-surface): (#[0-9a-f]{6});/g
    ),
  ].map((match) => match[1])
  const text = [...dark.matchAll(/--app-text-(?:primary|secondary|muted): (#[0-9a-f]{6});/g)].map((match) => match[1])
  assert.equal(surfaces.length, 5)
  assert.equal(text.length, 3)
  for (const surface of surfaces) {
    for (const color of text) assert.ok(contrastRatio(color, surface) >= 4.5, `${color} on ${surface}`)
    for (const color of ['#0d9488', '#ffff00', '#ffffff', '#000000', '#777777']) {
      assert.ok(contrastRatio(buildSemanticAccentColors(color)[1], surface) >= 4.5, `${color} on ${surface}`)
    }
  }
})
