import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { isReadOnlyGameArchive } from './gameArchive'

test('only ended non-practice games use the read-only archive experience', () => {
  const end = 2_000

  assert.equal(isReadOnlyGameArchive(undefined, end), false)
  assert.equal(isReadOnlyGameArchive({ end, practiceMode: false }, end - 1), false)
  assert.equal(isReadOnlyGameArchive({ end, practiceMode: false }, end), true)
  assert.equal(isReadOnlyGameArchive({ end, practiceMode: true }, end + 1), false)
})

test('ended challenge archives keep reads while hiding mutation controls', () => {
  const tabs = readFileSync('src/components/WithGameTab.tsx', 'utf8')
  const modal = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
  const overview = readFileSync('src/pages/games/[id]/Index.tsx', 'utf8')

  assert.match(tabs, /location\.pathname\.includes\('challenges'\)[\s\S]*ParticipationStatus\.Accepted/)
  assert.match(tabs, /game\?\.allowUserSubmissions === false \|\| archived/)
  assert.match(modal, /readOnlyArchive[\s\S]*submissions and workloads are closed/)
  assert.match(modal, /!readOnlyArchive && \(\s*<>[\s\S]*<form/)
  assert.match(overview, /status === ParticipationStatus\.Accepted && started/)
})

test('event access, archives, and challenge modals use the server-corrected lifecycle clock', () => {
  const tabs = readFileSync('src/components/WithGameTab.tsx', 'utf8')
  const challengePanel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
  const teamRank = readFileSync('src/components/TeamRank.tsx', 'utf8')
  const challengePage = readFileSync('src/pages/games/[id]/Challenges.tsx', 'utf8')
  const catalog = readFileSync('src/pages/challenges/Index.tsx', 'utf8')
  const challengeEditor = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')
  const archive = readFileSync('src/utils/gameArchive.ts', 'utf8')

  assert.match(tabs, /const \{ started, finished, now \} = useGameStatus\(game\)/)
  assert.doesNotMatch(tabs, /const now = dayjs\(\)/)
  assert.match(challengePanel, /const \{ finished \} = useGameStatus\(game\)/)
  assert.doesNotMatch(challengePanel, /dayjs\(game\?\.end\)/)
  assert.match(teamRank, /isReadOnlyGameArchive\(game, serverNow\.valueOf\(\)\)/)
  assert.match(challengePage, /isReadOnlyGameArchive\(game, serverNow\.valueOf\(\)\)/)
  assert.match(catalog, /const \{ finished \} = useGameStatus\(/)
  assert.doesNotMatch(catalog, /dayjs\(\)\.isAfter\(dayjs\(game/)
  assert.match(challengeEditor, /const \{ started: eventSecurityFrozen \} = useGameStatus\(game\)/)
  assert.doesNotMatch(archive, /Date\.now\(\)/)
})
