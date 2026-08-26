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
