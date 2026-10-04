import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('score timeline updates its chart only at minute or lifecycle boundaries', () => {
  const source = readFileSync('src/components/charts/ScoreTimeLine.tsx', 'utf8')

  assert.match(source, /const timelineNowMs = now\.startOf\('minute'\)\.valueOf\(\)/)
  assert.match(source, /const option = useMemo<EChartsOption>/)
  assert.match(source, /option=\{option\}/)
  assert.doesNotMatch(source, /option=\{\{/)
  assert.doesNotMatch(source, /\[activeTeams, game, endTime, colorScheme/)
})

test('score timeline distinguishes unavailable data from a loaded empty history', () => {
  const source = readFileSync('src/components/charts/ScoreTimeLine.tsx', 'utf8')
  const unavailable = source.indexOf('if (!scoreboard) return null')
  const empty = source.indexOf('if (!activeTeams?.length)')
  const chart = source.indexOf('<EchartsContainer')

  assert.ok(unavailable > 0 && empty > unavailable && chart > empty)
  assert.match(source, /<Empty title=\{t\('game.timeline.empty_title'\)\}/)
  assert.doesNotMatch(source, /if \(!activeTeams\?\.some/, 'zero-score teams still get their timeline')
})
