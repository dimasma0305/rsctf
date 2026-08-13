import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const guide = readFileSync('src/components/KothGuideModal.tsx', 'utf8')
const scoreboard = readFileSync('src/components/KothScoreboardTable.tsx', 'utf8')
const challenge = readFileSync('src/components/KothChallengePanel.tsx', 'utf8')
const operations = readFileSync('src/components/admin/KothOpsPanel.tsx', 'utf8')

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

test('Leaderboard lifecycle copy distinguishes persistent health recovery from Crown resets', () => {
  assert.match(scoreboard, /Persistent · health supervised/)
  assert.match(scoreboard, /stays online across scoring rounds and epochs/)
  assert.match(challenge, /your event token remains valid/)
  assert.match(operations, /runtime attempt/)
})

test('KotH scoreboard distinguishes the event average from hill-local performance', () => {
  assert.match(scoreboard, /Event score averages every finalized epoch/)
  assert.match(scoreboard, /Hill performance is a local average/)
  assert.match(scoreboard, /not added directly to the event score/i)
  assert.match(scoreboard, /weighted epoch-points.*finalized epoch weight.*event score/s)
  assert.match(scoreboard, /settledEpochPoints/)
  assert.match(scoreboard, /settledEpochWeight/)
  assert.doesNotMatch(scoreboard, /Finalized hill contribution/)
})
