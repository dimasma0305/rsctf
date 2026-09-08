import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { ChallengeType, type ChallengeInfoModel, type GameInfoModel } from '@Api'
import { canShowReadinessCache, eventReadiness, readinessError } from './EventReadiness'

const game: GameInfoModel = {
  id: 19,
  title: 'Rehearsal',
  start: 1_800_000_000_000,
  end: 1_800_003_600_000,
  sourceRevision: 1,
}
const challenge: ChallengeInfoModel = {
  id: 1,
  title: 'Task',
  isEnabled: true,
  reviewStatus: 'Active',
  buildStatus: 'Success',
}
const check = (key: string, changes: Partial<GameInfoModel> = {}, challenges = [challenge]) =>
  eventReadiness({ ...game, ...changes }, challenges).find((item) => item.key === key)!

test('readiness checks saved time windows and required writeup deadlines', () => {
  assert.equal(check('schedule').state, 'checked')
  for (const start of [NaN, Infinity, 9e15, game.end, game.end + 1])
    assert.equal(check('schedule', { start }).state, 'attention')
  assert.equal(check('freeze', { freeze: null }).reason, 'freeze_off')
  for (const freeze of [NaN, game.start, game.end, game.end + 1])
    assert.equal(check('freeze', { freeze }).state, 'attention')
  assert.equal(check('freeze', { freeze: game.start + 1000 }).state, 'checked')
  assert.equal(check('writeup').reason, 'writeup_off')
  for (const writeupDeadline of [undefined, NaN, game.start])
    assert.equal(check('writeup', { writeupRequired: true, writeupDeadline }).state, 'attention')
  assert.equal(check('writeup', { writeupRequired: true, writeupDeadline: game.end }).state, 'checked')
})

test('readiness permits hidden rehearsals and never turns successful builds into runtime readiness', () => {
  assert.deepEqual(
    eventReadiness({ ...game, hidden: true, practiceMode: true }, [challenge]),
    eventReadiness(game, [challenge])
  )
  assert.equal(check('builds').state, 'info')
  assert.equal(check('builds').reason, 'builds_recorded')
  assert.equal(check('builds', {}, []).state, 'info')
  assert.equal(check('challenges', {}, []).state, 'attention')
})

test('readiness treats unknown approval as unresolved and ignores disabled build failures', () => {
  for (const reviewStatus of [undefined, 'Pending', 'Rejected'] as const) {
    assert.equal(check('challenges', {}, [{ ...challenge, reviewStatus }]).reason, 'challenges_review')
  }
  const tasks: ChallengeInfoModel[] = [challenge, { ...challenge, id: 2, isEnabled: false, buildStatus: 'Failed' }]
  assert.equal(check('builds', {}, tasks).reason, 'builds_recorded')
  assert.equal(check('challenges', {}, tasks).count, 1)
})

test('readiness distinguishes build failures, in-flight builds, missing evidence and prebuilt archives', () => {
  for (const buildStatus of ['Failed', 'MissingDockerfile'] as const)
    assert.equal(check('builds', {}, [{ ...challenge, buildStatus }]).reason, 'builds_failed')
  for (const buildStatus of ['Queued', 'Building'] as const)
    assert.equal(check('builds', {}, [{ ...challenge, buildStatus }]).reason, 'builds_running')
  for (const buildStatus of [undefined, 'None'] as const) {
    assert.equal(
      check('builds', {}, [
        { ...challenge, hasOriginalArchive: true, type: ChallengeType.StaticContainer, buildStatus },
      ]).reason,
      'builds_unknown'
    )
  }
  assert.equal(check('builds', {}, [{ ...challenge, buildStatus: 'NotApplicable' }]).state, 'info')
})

test('readiness errors guide recovery and discard cached private data on access loss', () => {
  for (const [status, reason] of [
    [401, 'session'],
    [403, 'permission'],
    [404, 'missing'],
    [429, 'busy'],
    [502, 'service'],
  ] as const) {
    assert.equal(readinessError({ status }), reason)
    assert.equal(readinessError({ response: { status } }), reason)
  }
  assert.equal(readinessError(new Error('network unavailable')), 'request')
  assert.equal(readinessError({ title: 'private backend details' }), 'request')
  assert.equal(canShowReadinessCache([]), true)
  assert.equal(canShowReadinessCache([{ status: 503 }]), true)
  for (const status of [401, 403, 404]) assert.equal(canShowReadinessCache([{ status: 503 }, { status }]), false)
})

test('readiness stays read-only, manually refreshed, translated and linked into event navigation', () => {
  const page = readFileSync('src/pages/admin/games/[id]/Readiness.tsx', 'utf8')
  assert.match(page, /useEditGetGame\(eventId, OnceSWRConfig, validId\)/)
  assert.match(page, /useEditGetGameChallenges\(eventId, OnceSWRConfig, validId\)/)
  assert.match(page, /if \(refreshOwner.current\) return/)
  assert.doesNotMatch(page, /setInterval|setTimeout|editUpdate|editEnsure|useAdminAdState|useAdminKothState/)
  assert.match(readFileSync('src/components/admin/navigation.ts', 'utf8'), /path: 'readiness'/)
  const en = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8')).readiness
  const id = JSON.parse(readFileSync('src/locales/id-ID/admin.json', 'utf8')).readiness
  const keys = (value: Record<string, unknown>, prefix = ''): string[] =>
    Object.entries(value)
      .flatMap(([key, item]) =>
        typeof item === 'object' && item
          ? keys(item as Record<string, unknown>, `${prefix}${key}.`)
          : [`${prefix}${key}`]
      )
      .sort()
  assert.deepEqual(keys(en), keys(id))
  for (const row of eventReadiness(game, [challenge])) assert.ok(en.reasons[row.reason])
})
