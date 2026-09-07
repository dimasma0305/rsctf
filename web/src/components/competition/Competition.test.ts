import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { ChallengeCategory, ChallengeType, SubmissionType, type ChallengeInfo } from '@Api'
import { challengePage, isAcceptedSolve, projectSpherePoint, resolveChallengeView, sortChallenges } from './model'

const challenge = (id: number, type = ChallengeType.StaticAttachment): ChallengeInfo => ({
  id,
  title: `Challenge ${id}`,
  category: ChallengeCategory.Web,
  type,
  score: id * 100,
  solved: id,
  bloods: [],
  disableBloodBonus: true,
})

test('competition view defaults to the list on compact screens and rejects corrupt stored preferences', () => {
  assert.equal(resolveChallengeView(null, true), 'list')
  assert.equal(resolveChallengeView(null, false), 'globe')
  assert.equal(resolveChallengeView('unexpected', false), 'globe')
  assert.equal(resolveChallengeView('cards', true), 'cards')
})

test('competition pages bound one hundred nodes and retain every challenge exactly once', () => {
  const items = Array.from({ length: 100 }, (_, index) => challenge(index + 1))
  const collected = Array.from({ length: 13 }, (_, index) => challengePage(items, index + 1, 8).items).flat()
  assert.equal(collected.length, 100)
  assert.equal(new Set(collected.map((item) => item.id)).size, 100)
  assert.equal(challengePage(items, 200, 8).current, 13)
  assert.equal(challengePage([], 200, 8).current, 1)
  assert.deepEqual(challengePage([], 1, 8).items, [])
})

test('competition point sorting does not treat live-engine placeholder points as Jeopardy scores', () => {
  const items = [
    challenge(100, ChallengeType.KingOfTheHill),
    challenge(1),
    challenge(2),
    challenge(200, ChallengeType.AttackDefense),
  ]
  const sorted = sortChallenges(items, 'score')
  assert.deepEqual(
    sorted.slice(0, 2).map((item) => item.id),
    [2, 1]
  )
  assert.equal(items[0].id, 100, 'do not mutate the shared server snapshot')
  assert.equal(isAcceptedSolve(undefined), false)
  assert.equal(isAcceptedSolve({ type: SubmissionType.Unaccepted }), false)
  assert.equal(isAcceptedSolve({ type: SubmissionType.Normal }), true)
})

test('competition sphere rotation preserves radius and has no idle animation loop', () => {
  const p = projectSpherePoint(0.6, 0, 0.8, 1.2, 0.3)
  assert.ok(Math.abs(p.x ** 2 + p.y ** 2 + p.z ** 2 - 1) < 1e-10)
  const globe = readFileSync('src/components/competition/ChallengeGlobe.tsx', 'utf8')
  assert.doesNotMatch(globe, /setInterval|setTimeout|fetch\(|useSWR/)
  assert.match(globe, /cancelAnimationFrame\(pendingFrame.current\)/)
  assert.match(globe, /type="button"/)
  assert.match(globe, /event.pointerType !== 'mouse'/)
})

test('competition panels reuse owned challenge reads and keep archives and mobile dialogs', () => {
  const panel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
  const shell = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
  const navigation = readFileSync('src/components/WithNavbar.tsx', 'utf8')
  assert.match(panel, /challengeOwned=\{selection\?\.gameId === numId && selection.challengeId === challenge.id\}/)
  assert.match(panel, /embedded=\{inlineDetail\}/)
  assert.match(shell, /if \(embedded && !flagVerdict\)/)
  assert.match(shell, /aria-labelledby="competition-challenge-title"/)
  assert.match(shell, /!readOnlyArchive && \(/)
  assert.match(shell, /<Modal.Root/)
  assert.match(
    navigation,
    /!isMobile && \(\s*<AppNavbar/,
    'desktop competition pages retain the shared sidebar without duplicating mobile connection controls'
  )
  assert.match(navigation, /isMobile && <AppHeader/)
  assert.doesNotMatch(navigation, /desktop: competition/)
  assert.doesNotMatch(panel, /classes\.emptyDetail/)
})
