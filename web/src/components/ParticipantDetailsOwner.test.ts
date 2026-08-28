import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const challengesRoute = readFileSync('src/pages/games/[id]/Challenges.tsx', 'utf8')
const scoreboardRoute = readFileSync('src/pages/games/[id]/Scoreboard.tsx', 'utf8')
const challengePanel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
const teamRank = readFileSync('src/components/TeamRank.tsx', 'utf8')

const callCount = (source: string) => source.match(/useGameTeamInfo\(/g)?.length ?? 0

test('game routes own one participant-details snapshot and pass it to every child', () => {
  assert.equal(callCount(challengesRoute), 1)
  assert.equal(callCount(scoreboardRoute), 1)
  assert.equal(callCount(challengePanel), 0)
  assert.equal(callCount(teamRank), 0)

  assert.match(challengesRoute, /<ChallengePanel[^>]*participantDetails=\{participantDetails\}/)
  assert.match(challengesRoute, /<TeamRank participantDetails=\{participantDetails\}/)
  assert.match(scoreboardRoute, /<TeamRank participantDetails=\{participantDetails\}/)
  assert.match(challengePanel, /const \{ teamInfo, game,[^}]*\} = participantDetails/)
  assert.match(teamRank, /const \{ teamInfo, game, error \} = participantDetails/)
})
