import assert from 'node:assert/strict'
import { test } from 'node:test'
import { scoreboardPagePath } from './ScoreboardPage'
import { isConditionalScoreboardPath } from './ConditionalScoreboard'

test('paged standings use separate conditional keys for page, division, search and team lookup', () => {
  const first = scoreboardPagePath(7, { skip: 0, count: 30, divisionId: null, search: '' })
  const next = scoreboardPagePath(7, { skip: 30, count: 30, divisionId: null, search: '' })
  assert.equal(first, '/api/game/7/scoreboard/page?skip=0&count=30')
  assert.notEqual(first, next)
  assert.equal(isConditionalScoreboardPath(first), true)
  const filtered = scoreboardPagePath(7, {
    skip: 0, count: 10, divisionId: 3, search: ' a&b ', locateTeam: 'Team / #1',
  })
  const parsed = new URL(filtered, 'https://example.test')
  assert.equal(parsed.searchParams.get('divisionId'), '3')
  assert.equal(parsed.searchParams.get('search'), 'a&b')
  assert.equal(parsed.searchParams.get('locateTeam'), 'Team / #1')
  assert.equal(isConditionalScoreboardPath(filtered), true)
  assert.equal(isConditionalScoreboardPath('/api/game/7/scoreboard/page/other?count=30'), false)
})
