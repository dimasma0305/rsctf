import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { VIEWER_SCOPE_MARKER } from './Cache'
import { ACCOUNT_STATS_PATH, CHALLENGE_CATALOG_PATH, isPlayerReadPath, TEAM_SELECTOR_PATH } from './PlayerReadCache'

const source = (path: string) => readFileSync(path, 'utf8')

test('passive SWR reads are one-shot unless a route explicitly owns polling', () => {
  const app = source('src/App.tsx')
  const config = source('src/hooks/useConfig.ts')
  const stats = source('src/components/account/StatsPanel.tsx')
  const catalog = source('src/pages/challenges/Index.tsx')

  assert.match(app, /refreshInterval:\s*0/)
  assert.match(app, /shouldRetryOnError:\s*false/)
  assert.match(config, /OnceSWRConfig[\s\S]*refreshInterval:\s*0/)
  assert.match(config, /OnceSWRConfig[\s\S]*shouldRetryOnError:\s*false/)
  assert.match(stats, /useAccountStats\(OnceSWRConfig\)/)
  assert.doesNotMatch(stats, /fetch\(url/)
  assert.match(catalog, /refreshInterval:\s*0/)
  assert.match(catalog, /shouldRetryOnError:\s*false/)
})

test('closed dialogs do not mount duplicate roster or SSH reads', () => {
  const gameJoin = source('src/components/GameJoinModal.tsx')
  const teamEdit = source('src/components/TeamEditModal.tsx')
  const adGuide = source('src/components/AdGuideModal.tsx')
  const challengeRoute = source('src/pages/games/[id]/Challenges.tsx')
  const adPanel = source('src/components/AdChallengePanel.tsx')

  assert.doesNotMatch(gameJoin, /useTeams\(/)
  assert.match(gameJoin, /teams\?: TeamSelectorInfoModel\[\]/)
  assert.doesNotMatch(teamEdit, /useTeamGetTeamsInfo\(/)
  assert.match(teamEdit, /mutateTeams: KeyedMutator<TeamInfoModel\[\]>/)
  assert.match(adGuide, /Boolean\(modalProps\.opened && gameId > 0\)/)
  assert.match(challengeRoute, /<ChallengePanel[\s\S]*adStateOwner=/)
  assert.match(adPanel, /active: active && !stateOwner/)
})

test('player read invalidation recognizes viewer-scoped keys and every catalog query variant', () => {
  const paths = new Set([ACCOUNT_STATS_PATH, CHALLENGE_CATALOG_PATH, TEAM_SELECTOR_PATH])
  const scopedCatalog = [VIEWER_SCOPE_MARKER, 'user:1:User', [CHALLENGE_CATALOG_PATH, { search: 'pwn' }]] as const

  assert.equal(isPlayerReadPath(scopedCatalog, paths), true)
  assert.equal(isPlayerReadPath([CHALLENGE_CATALOG_PATH, { mode: 'koth' }], paths), true)
  assert.equal(isPlayerReadPath('/api/game/1/details', paths), false)
})

test('accepted solves and membership mutations invalidate derived player reads', () => {
  const challenge = source('src/components/GameChallengeModal.tsx')
  const game = source('src/pages/games/[id]/Index.tsx')
  const userHook = source('src/hooks/useUser.tsx')

  assert.match(challenge, /AnswerResult\.Accepted[\s\S]*ACCOUNT_STATS_PATH, CHALLENGE_CATALOG_PATH/)
  assert.match(game, /gameJoinGame[\s\S]*invalidatePlayerReads\(mutateCache, \[CHALLENGE_CATALOG_PATH\]\)/)
  assert.match(game, /gameLeaveGame[\s\S]*invalidatePlayerReads\(mutateCache, \[CHALLENGE_CATALOG_PATH\]\)/)
  assert.match(userHook, /invalidatePlayerReads\(mutateCache, \[TEAM_SELECTOR_PATH\]\)/)
})
