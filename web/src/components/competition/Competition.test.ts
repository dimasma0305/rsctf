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
  assert.match(globe, /data-globe-choice=\{node.id\}/)
  assert.equal(
    (globe.match(/onClick=\{node.onSelect\}/g) ?? []).length,
    2,
    'globe and navigator use the same action owner'
  )
  const css = readFileSync('src/components/competition/Competition.module.css', 'utf8')
  assert.match(css, /\.globeStage\s*\{[^}]*aspect-ratio: 1;/, 'sphere and node projection share a square stage')
})

test('globe dominates its panel without changing the shared event shell or adding animated effects', () => {
  const css = readFileSync('src/components/competition/Competition.module.css', 'utf8')
  const globe = readFileSync('src/components/competition/ChallengeGlobe.tsx', 'utf8')
  assert.match(css, /\.globeStage\s*\{[^}]*max-width: 58rem;/)
  assert.match(css, /grid-template-columns: minmax\(0, 1fr\) minmax\(14rem, 18rem\)/)
  assert.match(css, /@container \(max-width: 48rem\)/)
  assert.doesNotMatch(css, /animation:|backdrop-filter:/)
  assert.match(globe, /data-globe-stage/)
  assert.match(globe, /element.width = size \* 1.5/)
  assert.match(globe, /element.height = size \* 1.5/)
})

test('competition header groups event, team and navigation without an ended countdown', () => {
  const tabs = readFileSync('src/components/WithGameTab.tsx', 'utf8')
  assert.match(tabs, /data-event-workspace-header/)
  assert.match(tabs, /if \(compact && finished\) return null/)
  assert.doesNotMatch(
    tabs,
    /summary \? classes.masthead|summary \? 'underline'/,
    'the event header does not change between sections'
  )
  assert.ok(
    tabs.indexOf('{summary &&') > tabs.indexOf('<IconTabs'),
    'route-specific team data stays below the stable navigation frame'
  )
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

test('competition layout measures available width and keeps one toolbar above the inspector', () => {
  const panel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
  const toolbar = readFileSync('src/components/competition/ChallengeToolbar.tsx', 'utf8')
  const list = readFileSync('src/components/competition/ChallengeList.tsx', 'utf8')
  const modal = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
  assert.match(panel, /workspaceWidth >= 1200/)
  assert.ok(panel.indexOf('<ChallengeToolbar') < panel.indexOf('data-competition-workspace'))
  assert.match(panel, /drawer=\{!inlineDetail && !isCompact\}/)
  assert.match(modal, /if \(drawer && !flagVerdict\)/)
  assert.match(modal, /<Drawer/)
  assert.match(toolbar, /data-challenge-filters/)
  assert.match(toolbar, /trapFocus/)
  assert.match(toolbar, /returnFocus/)
  assert.doesNotMatch(list, /label=\{t\('game.arena.sort'/)
  assert.equal((list.match(/scope="col" className=\{classes.number\}/g) ?? []).length, 2)
})
