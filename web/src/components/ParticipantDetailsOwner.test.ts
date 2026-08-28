import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const challengesRoute = readFileSync('src/pages/games/[id]/Challenges.tsx', 'utf8')
const scoreboardRoute = readFileSync('src/pages/games/[id]/Scoreboard.tsx', 'utf8')
const challengePanel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
const teamRank = readFileSync('src/components/TeamRank.tsx', 'utf8')
const gameHooks = readFileSync('src/hooks/useGame.ts', 'utf8')
const challengeModal = readFileSync('src/components/GameChallengeModal.tsx', 'utf8')
const gameLanding = readFileSync('src/pages/games/[id]/Index.tsx', 'utf8')
const userHooks = readFileSync('src/hooks/useUser.tsx', 'utf8')

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

test('participant catalog and delta reads have separate completion-scheduled owners', () => {
  const participantHook = gameHooks.slice(
    gameHooks.indexOf('export const participantDeltaSWRConfig'),
    gameHooks.indexOf('export const useGameTeamInfo') + 1_500
  )
  assert.match(participantHook, /CompletionPollSWRConfig/)
  assert.equal(participantHook.match(/useCompletionPolling\(\{/g)?.length, 2)
  assert.match(participantHook, /jitterPollingDelay\(60_000\)/)
  assert.match(participantHook, /jitterPollingDelay\(10_000\)/)
  assert.doesNotMatch(participantHook, /refreshInterval/)
  assert.doesNotMatch(gameHooks, /participantDeltaPollMiddleware|participantDeltaSubscribers/)
})

test('the route-owned A&D state read also has one completion-scheduled cadence', () => {
  const adHook = gameHooks.slice(
    gameHooks.indexOf('export const useAdState'),
    gameHooks.indexOf('export const useAdScoreboard')
  )
  assert.match(adHook, /CompletionPollSWRConfig/)
  assert.match(adHook, /useCompletionPolling\(\{/)
  assert.match(adHook, /jitterPollingDelay\(10_000\)/)
  assert.doesNotMatch(adHook, /refreshInterval/)
})

test('solve, team, join, and leave mutations invalidate viewer-scoped derived reads', () => {
  assert.match(challengeModal, /swrRequestPath\(key\)/)
  assert.match(challengeModal, /path === '\/api\/account\/stats'/)
  assert.match(challengeModal, /path === '\/api\/game\/challenges'/)
  assert.match(challengeModal, /swrRequestPath\(key\) === `\/api\/game\/\$\{gameId\}\/details\/live`/)
  assert.match(challengeModal, /\{ revalidate: false \}/)
  assert.match(challengeModal, /void onAccepted\?\.\(\)/)
  const catalog = readFileSync('src/pages/challenges/Index.tsx', 'utf8')
  assert.match(catalog, /onAccepted=\{\(\) => mutate\(\)\}/)
  assert.match(userHooks, /swrRequestPath\(key\) === '\/api\/team\/selector'/)
  assert.match(gameLanding, /invalidateParticipationReads/)
  assert.match(gameLanding, /path === '\/api\/game\/challenges'/)
  assert.match(gameLanding, /path === `\/api\/game\/\$\{numId\}\/details\/catalog`/)
  assert.equal(gameLanding.match(/await invalidateParticipationReads\(\)/g)?.length, 2)
  assert.equal(gameLanding.match(/void mutate\(\)/g)?.length, 2)
})
