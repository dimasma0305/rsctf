import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { arenaTeamInitials, arenaTeamLabel, arenaTeamPosition, initialArenaRanking } from '../pages/games/[id]/arenaPresentation'

test('selected arena controls pair managed accent surfaces with contrast-safe text', () => {
  const css = readFileSync('src/pages/games/[id]/arenaTheme.css', 'utf8')
  for (const selector of ['.btn.on', '.rank-tabs button.on']) {
    const rule = css.slice(css.indexOf(`${selector} {`)).split('}')[0]
    assert.match(rule, /background: var\(--app-accent-surface\)/)
    assert.match(rule, /color: var\(--app-accent-surface-text\)/)
  }
})

test('arena defaults to a scoring mode that the event actually has', () => {
  assert.equal(initialArenaRanking(0, 0), 'jeopardy')
  assert.equal(initialArenaRanking(0, 3), 'koth')
  assert.equal(initialArenaRanking(4, 0), 'ad')
  assert.equal(initialArenaRanking(4, 3), 'ad')
})

test('small and dense rosters stay on the globe, outside the central hills', () => {
  for (const count of [1, 8, 49, 100, 500]) {
    const positions = Array.from({ length: count }, (_, index) => arenaTeamPosition(index, count))
    assert.equal(new Set(positions.map(({ x, y }) => `${x},${y}`)).size, count)
    for (const { x, y } of positions) {
      assert.ok(Math.hypot(x - 500, y - 500) > 220)
      assert.ok(Math.hypot(x - 500, y - 500) < 365)
    }
    assert.deepEqual(positions[0], arenaTeamPosition(0, count))
  }
})

test('team labels are bounded without cutting Unicode code points', () => {
  assert.equal(arenaTeamInitials('  Moon Discovery  '), 'MD')
  assert.equal(arenaTeamInitials('⚡APT-69'), '⚡')
  assert.equal(arenaTeamLabel('Moon Discovery'), 'Moon Discovery')
  assert.equal(Array.from(arenaTeamLabel('🚀'.repeat(30))).length, 16)
  assert.equal(arenaTeamLabel('🚀'.repeat(30)).endsWith('…'), true)
})
