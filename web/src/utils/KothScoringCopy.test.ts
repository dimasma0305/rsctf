import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const guide = readFileSync('src/components/KothGuideModal.tsx', 'utf8')
const scoreboard = readFileSync('src/components/KothScoreboardTable.tsx', 'utf8')

test('KotH player guidance states the unique-leader Crown and exact-tie rule', () => {
  for (const source of [guide, scoreboard]) {
    assert.match(source, /unique (?:leader|Crown)/i)
    assert.match(source, /exact (?:top-score )?tie/i)
    assert.match(source, /no team receives the Crown/i)
  }
})

test('KotH player guidance cannot regress to a hidden tie breaker', () => {
  const copy = `${guide}\n${scoreboard}`
  assert.doesNotMatch(copy, /participating incumbent/i)
  assert.doesNotMatch(copy, /earliest (?:server-confirmed )?tied/i)
  assert.doesNotMatch(copy, /tied-best Crown/i)
})
