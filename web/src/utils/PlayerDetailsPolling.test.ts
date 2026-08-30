import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const hooks = readFileSync('src/hooks/useGame.ts', 'utf8')
const challengeRoute = readFileSync('src/pages/games/[id]/Challenges.tsx', 'utf8')
const scoreboardRoute = readFileSync('src/pages/games/[id]/Scoreboard.tsx', 'utf8')
const challengePanel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
const teamRank = readFileSync('src/components/TeamRank.tsx', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')

test('the live team poll uses the compact conditional participant projection', () => {
  const hook = hooks.slice(hooks.indexOf('export const useGameTeamInfo'), hooks.indexOf('/** A&D'))
  assert.match(hook, /useGameChallengesWithTeamInfo\(numId/)
  assert.doesNotMatch(hook, /refreshInterval:/)
  assert.match(hook, /useGameParticipantDelta/)
  assert.match(hook, /useCompletionPolling/)
  assert.match(hook, /jitterPollingDelay\(10_000\)/)
  assert.match(api, /\/api\/game\/\$\{id\}\/details\/participant/)
})

test('challenge and scoreboard children reuse their route-owned team snapshot', () => {
  assert.equal((challengeRoute.match(/useGameTeamInfo\(numId\)/g) ?? []).length, 1)
  assert.match(challengeRoute, /<ChallengePanel teamState=\{teamState\}/)
  assert.match(challengeRoute, /<TeamRank teamState=\{teamState\}/)
  assert.doesNotMatch(challengePanel, /useGameTeamInfo\(numId/)
  assert.doesNotMatch(teamRank, /useGameTeamInfo\(numId/)

  assert.equal((scoreboardRoute.match(/useGameTeamInfo\(numId, false\)/g) ?? []).length, 1)
  assert.match(scoreboardRoute, /<TeamRank teamState=\{teamState\}/)
})

